//! OSS-style HMAC signature authentication middleware.

use axum::body::Body;
use axum::extract::{FromRequestParts, State};
use axum::http::{request::Parts, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

use crate::store::StoreHandle;
use crate::models::app::App;
use crate::services::s3_signature;
use crate::services::signature::{
    build_string_to_sign, verify_date, verify_signature, verify_signed_url,
};

/// App state shared with auth middleware.
#[derive(Clone)]
pub struct AuthState {
    pub pool: StoreHandle,
    pub master_key: String,
}

/// Authenticated app info extracted by the middleware.
#[derive(Debug, Clone)]
pub struct AuthenticatedApp {
    pub app_id: String,
}

/// 标记：本次请求走的是 S3（AWS Signature V4）鉴权。
///
/// handler 依据它决定响应格式（XML vs JSON）与错误语义。
#[derive(Debug, Clone)]
pub struct S3AuthContext {
    /// S3 AccessKey（即 App id）。
    pub access_key: String,
    /// 客户端声明的 region（S3 签名 scope 中携带）。
    pub region: String,
    /// 是否通过预签名 URL 鉴权。
    pub presigned: bool,
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for S3AuthContext
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<S3AuthContext>()
            .cloned()
            .ok_or(AuthError::MissingAuth)
    }
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for AuthenticatedApp
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthenticatedApp>()
            .cloned()
            .ok_or(AuthError::MissingAuth)
    }
}

/// Authentication error types.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Missing authentication")]
    MissingAuth,
    #[error("Invalid authorization format")]
    InvalidFormat,
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Request expired")]
    Expired,
    #[error("App not found")]
    AppNotFound,
    #[error("Missing Date header")]
    MissingDate,
    #[error("Internal error")]
    InternalError,
    /// S3（V4）鉴权失败：按 S3 语义返回 XML 错误。
    #[error("S3 authentication failed: {code}")]
    S3 {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::S3 {
                status,
                code,
                message,
            } => {
                return crate::s3::error::S3Error::new(status, code, message).into_response();
            }
            Self::MissingAuth => (StatusCode::UNAUTHORIZED, "Missing authentication"),
            Self::InvalidFormat => (StatusCode::BAD_REQUEST, "Invalid authorization format"),
            Self::InvalidSignature => (StatusCode::FORBIDDEN, "Invalid signature"),
            Self::Expired => (StatusCode::FORBIDDEN, "Request expired or time skew too large"),
            Self::AppNotFound => (StatusCode::FORBIDDEN, "Invalid credentials"),
            Self::MissingDate => (StatusCode::BAD_REQUEST, "Missing Date header"),
            Self::InternalError => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
        };

        let body = Json(serde_json::json!({
            "code": status.as_u16(),
            "message": message,
        }));

        (status, body).into_response()
    }
}

/// OSS signature authentication middleware.
///
/// Checks `Authorization: OSS {app_id}:{signature}` header or signed URL query params.
/// Credentials are optional: requests without either pass through unauthenticated,
/// which lets handlers support public reads while still rejecting invalid credentials.
pub async fn oss_auth_middleware(
    State(state): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, AuthError> {
    let (mut parts, body) = request.into_parts();

    let raw_query = parts.uri.query().unwrap_or("").to_string();
    let query_params = parse_query_params(&raw_query);
    let auth_header = parts
        .headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let mut authenticated = None;
    let mut s3_context = None;

    if s3_signature::is_presigned_query(&query_params) {
        let ctx = verify_s3_presigned(&state, &parts, &raw_query, &query_params).await?;
        authenticated = Some(AuthenticatedApp {
            app_id: ctx.access_key.clone(),
        });
        s3_context = Some(ctx);
    } else if auth_header.starts_with(s3_signature::ALGORITHM) {
        let ctx = verify_s3_header(&state, &parts, &raw_query, &auth_header).await?;
        authenticated = Some(AuthenticatedApp {
            app_id: ctx.access_key.clone(),
        });
        s3_context = Some(ctx);
    } else if let Some((app_id, signature, expires)) = extract_signed_url_params(&parts) {
        authenticated = Some(verify_signed_url_auth(&state, &parts, &app_id, &signature, expires).await?);
    } else if parts.headers.contains_key("Authorization") {
        let (app_id, signature) = extract_auth_header(&parts)?;
        authenticated = Some(verify_header_auth(&state, &parts, &app_id, &signature).await?);
    }

    if let Some(app) = authenticated {
        parts.extensions.insert(app);
    }
    if let Some(ctx) = s3_context {
        parts.extensions.insert(ctx);
    }
    let request = Request::from_parts(parts, body);
    Ok(next.run(request).await)
}

/// 解析 AccessKey -> App，并解出用于验签的 SecretKey。
async fn load_s3_credentials(
    state: &AuthState,
    access_key: &str,
) -> Result<(App, String), AuthError> {
    let app = App::get_by_id(&state.pool, access_key)
        .await
        .map_err(|err| {
            tracing::error!("S3 auth: failed to load app {}: {}", access_key, err);
            AuthError::InternalError
        })?
        .ok_or_else(|| AuthError::S3 {
            status: StatusCode::FORBIDDEN,
            code: "InvalidAccessKeyId",
            message: "The AWS Access Key Id you provided does not exist in our records."
                .to_string(),
        })?;

    let secret = app.secret(&state.master_key).map_err(|err| {
        tracing::error!("S3 auth: failed to decrypt app secret: {}", err);
        AuthError::InternalError
    })?;

    Ok((app, secret))
}

fn map_s3_signature_error(err: s3_signature::S3SigError) -> AuthError {
    let mapped = crate::s3::error::S3Error::from_signature(err);
    AuthError::S3 {
        status: mapped.status,
        code: mapped.code,
        message: mapped.message,
    }
}

async fn verify_s3_header(
    state: &AuthState,
    parts: &Parts,
    raw_query: &str,
    auth_header: &str,
) -> Result<S3AuthContext, AuthError> {
    let parsed = s3_signature::parse_authorization(auth_header).map_err(map_s3_signature_error)?;
    let (app, secret) = load_s3_credentials(state, &parsed.credential.access_key).await?;

    s3_signature::verify_header_signature(
        &parts.method,
        parts.uri.path(),
        raw_query,
        &parts.headers,
        &parsed,
        &secret,
        s3_signature::now_secs(),
    )
    .map_err(map_s3_signature_error)?;

    Ok(S3AuthContext {
        access_key: app.app_id,
        region: parsed.credential.region,
        presigned: false,
    })
}

async fn verify_s3_presigned(
    state: &AuthState,
    parts: &Parts,
    raw_query: &str,
    params: &std::collections::HashMap<String, String>,
) -> Result<S3AuthContext, AuthError> {
    let parsed = s3_signature::parse_presign(params).map_err(map_s3_signature_error)?;
    let (app, secret) = load_s3_credentials(state, &parsed.credential.access_key).await?;

    s3_signature::verify_presigned_signature(
        &parts.method,
        parts.uri.path(),
        raw_query,
        &parts.headers,
        &parsed,
        &secret,
        s3_signature::now_secs(),
    )
    .map_err(map_s3_signature_error)?;

    Ok(S3AuthContext {
        access_key: app.app_id,
        region: parsed.credential.region,
        presigned: true,
    })
}

fn extract_auth_header(parts: &Parts) -> Result<(String, String), AuthError> {
    let auth_header = parts
        .headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::MissingAuth)?;

    let auth = auth_header
        .strip_prefix("OSS ")
        .ok_or(AuthError::InvalidFormat)?;

    let parts_vec: Vec<&str> = auth.splitn(2, ':').collect();
    if parts_vec.len() != 2 {
        return Err(AuthError::InvalidFormat);
    }

    Ok((parts_vec[0].to_string(), parts_vec[1].to_string()))
}

fn extract_signed_url_params(parts: &Parts) -> Option<(String, String, i64)> {
    let query = parts.uri.query()?;
    let params = parse_query_params(query);

    let app_id = params.get("OSSAccessKeyId")?.clone();
    let signature = params.get("Signature")?.clone();
    let expires: i64 = params.get("Expires")?.parse().ok()?;

    Some((app_id, signature, expires))
}

/// Simple URL query parser (avoids extra dependency).
fn parse_query_params(query: &str) -> std::collections::HashMap<String, String> {
    let mut params = std::collections::HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = url_decode(key);
            let value = url_decode(value);
            params.insert(key, value);
        }
    }
    params
}

/// Minimal URL percent-decoding.
fn url_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                result.push(byte);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

async fn verify_header_auth(
    state: &AuthState,
    parts: &Parts,
    app_id: &str,
    signature: &str,
) -> Result<AuthenticatedApp, AuthError> {
    let app = App::get_by_id(&state.pool, app_id)
        .await
        .map_err(|_| AuthError::InternalError)?
        .ok_or(AuthError::AppNotFound)?;

    let date = parts
        .headers
        .get("Date")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::MissingDate)?;

    if !verify_date(date) {
        return Err(AuthError::Expired);
    }

    let content_md5 = parts
        .headers
        .get("Content-MD5")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let content_type = parts
        .headers
        .get("Content-Type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let method = parts.method.as_str();
    let path = parts.uri.path();

    let string_to_sign = build_string_to_sign(
        method,
        &content_md5,
        &content_type,
        date,
        path,
    );

    let app_secret = app.secret(&state.master_key).map_err(|_| AuthError::InternalError)?;

    if !verify_signature(&app_secret, &string_to_sign, signature) {
        return Err(AuthError::InvalidSignature);
    }

    Ok(AuthenticatedApp {
        app_id: app.app_id,
    })
}

async fn verify_signed_url_auth(
    state: &AuthState,
    parts: &Parts,
    app_id: &str,
    signature: &str,
    expires: i64,
) -> Result<AuthenticatedApp, AuthError> {
    let app = App::get_by_id(&state.pool, app_id)
        .await
        .map_err(|_| AuthError::InternalError)?
        .ok_or(AuthError::AppNotFound)?;

    let method = parts.method.as_str();
    let path = parts.uri.path();

    let app_secret = app.secret(&state.master_key).map_err(|_| AuthError::InternalError)?;

    if !verify_signed_url(&app_secret, method, expires, path, signature) {
        return Err(AuthError::InvalidSignature);
    }

    Ok(AuthenticatedApp {
        app_id: app.app_id,
    })
}
