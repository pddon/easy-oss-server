//! Admin authentication middleware.
//!
//! Uses a simple bearer token (X-Admin-Token header) for admin API access.

use axum::body::Body;
use axum::extract::{FromRequestParts, State};
use axum::http::{request::Parts, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::http::Request;

/// Admin auth state.
#[derive(Clone)]
pub struct AdminAuthState {
    pub admin_token: String,
}

/// Marker type indicating successful admin authentication.
#[derive(Debug, Clone)]
pub struct AdminUser;

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for AdminUser
where
    S: Send + Sync,
{
    type Rejection = AdminAuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AdminUser>()
            .cloned()
            .ok_or(AdminAuthError::Unauthorized)
    }
}

/// Admin authentication errors.
#[derive(Debug, thiserror::Error)]
pub enum AdminAuthError {
    #[error("Unauthorized")]
    Unauthorized,
}

impl IntoResponse for AdminAuthError {
    fn into_response(self) -> Response {
        let status = StatusCode::UNAUTHORIZED;
        let body = Json(serde_json::json!({
            "code": status.as_u16(),
            "message": "Admin authentication required",
        }));
        (status, body).into_response()
    }
}

/// Admin authentication middleware.
///
/// Checks the `X-Admin-Token` header against the configured admin token.
pub async fn admin_auth_middleware(
    State(state): State<AdminAuthState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, AdminAuthError> {
    let token = request
        .headers()
        .get("X-Admin-Token")
        .and_then(|h| h.to_str().ok());

    match token {
        Some(t) if t == state.admin_token => {
            let (mut parts, body) = request.into_parts();
            parts.extensions.insert(AdminUser);
            let request = Request::from_parts(parts, body);
            Ok(next.run(request).await)
        }
        _ => Err(AdminAuthError::Unauthorized),
    }
}
