//! S3 协议兼容层。
//!
//! 设计原则：与自研 OSS 接口共用同一套路由与存储，通过“请求特征”
//! （AWS4 签名 / `X-Amz-*` query / S3 专用 query）在 handler 内分流，
//! 避免出现两套路径互吞的情况。

pub mod bucket;
pub mod error;
pub mod object;
pub mod xml;

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::middleware::auth::{AuthenticatedApp, S3AuthContext};
use crate::models::app::App;
use crate::models::bucket::Bucket;
use crate::services::storage::StorageService;
use crate::store::StoreHandle;
use crate::web::admin_handler;

use error::S3Error;
use xml::{escape, S3_NS, XML_DECL};

/// S3 handler 需要的共享状态。
#[derive(Clone)]
pub struct S3State {
    pub pool: StoreHandle,
    pub storage: StorageService,
    pub max_upload_size: u64,
}

impl S3State {
    /// 从旧 OSS handler 的状态转换：两套接口共享同一份存储与上传限额。
    pub fn from_object(state: &crate::handlers::object::ObjectState) -> Self {
        Self {
            pool: state.pool.clone(),
            storage: state.storage.clone(),
            max_upload_size: state.max_upload_size,
        }
    }
}

/// 根路径分流：S3 请求走 ListBuckets，浏览器请求仍跳转管理后台。
pub async fn root(
    state: State<S3State>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
) -> Response {
    if s3.is_some() {
        list_buckets(state, auth).await
    } else {
        admin_handler::root_redirect().await
    }
}

/// ListBuckets：`GET /`。
pub async fn list_buckets(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
) -> Response {
    let Some(auth) = auth else {
        return S3Error::access_denied("Anonymous access is not allowed.")
            .with_resource("/")
            .into_response();
    };

    let app = match App::get_by_id(&state.pool, &auth.app_id).await {
        Ok(Some(app)) => app,
        Ok(None) => {
            return S3Error::access_denied("The app associated with this access key no longer exists.")
                .with_resource("/")
                .into_response()
        }
        Err(err) => {
            tracing::error!("S3 ListBuckets: failed to load app: {}", err);
            return S3Error::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                "We encountered an internal error. Please try again.",
            )
            .into_response();
        }
    };

    let buckets = match Bucket::list_by_app(&state.pool, &auth.app_id, 0, 10_000).await {
        Ok(buckets) => buckets,
        Err(err) => {
            tracing::error!("S3 ListBuckets: failed to list buckets: {}", err);
            return S3Error::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                "We encountered an internal error. Please try again.",
            )
            .into_response();
        }
    };

    let mut body = String::new();
    body.push_str(XML_DECL);
    body.push_str(&format!(
        "<ListAllMyBucketsResult xmlns=\"{S3_NS}\">"
    ));
    body.push_str(&format!(
        "<Owner><ID>{}</ID><DisplayName>{}</DisplayName></Owner>",
        escape(&app.app_id),
        escape(&app.name)
    ));
    body.push_str("<Buckets>");
    for bucket in buckets {
        let created = bucket.created_at_dt().format("%Y-%m-%dT%H:%M:%S.000Z");
        body.push_str(&format!(
            "<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>",
            escape(&bucket.name),
            created
        ));
    }
    body.push_str("</Buckets></ListAllMyBucketsResult>");

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/xml",
        )],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 公共工具
// ---------------------------------------------------------------------------

/// 绑定到指定 App 的桶查询；不存在或不属于该 App 统一按 NoSuchBucket 处理，
/// 避免泄露其他 App 的桶是否存在。
pub async fn bucket_for_app(
    pool: &StoreHandle,
    app_id: &str,
    name: &str,
) -> Result<Bucket, S3Error> {
    Bucket::get_by_app_and_name(pool, app_id, name)
        .await
        .map_err(|err| {
            tracing::error!("S3: failed to load bucket {}: {}", name, err);
            internal_error()
        })?
        .ok_or_else(|| S3Error::no_such_bucket(name))
}

pub fn internal_error() -> S3Error {
    S3Error::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalError",
        "We encountered an internal error. Please try again.",
    )
}

/// 构造 XML 响应。
pub fn xml_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml")],
        Body::from(body),
    )
        .into_response()
}

/// 收集 `x-amz-meta-*` 自定义元数据（键去前缀并小写）。
pub fn collect_meta(headers: &HeaderMap) -> HashMap<String, String> {
    let mut meta = HashMap::new();
    for (name, value) in headers.iter() {
        let name = name.as_str();
        if let Some(key) = name.strip_prefix("x-amz-meta-") {
            if let Ok(value) = value.to_str() {
                meta.insert(key.to_ascii_lowercase(), value.to_string());
            }
        }
    }
    meta
}

/// 把 sidecar 元数据写回响应头。
pub fn append_meta_headers(headers: &mut HeaderMap, meta: &HashMap<String, String>) {
    for (key, value) in meta {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(format!("x-amz-meta-{key}").as_bytes()),
            axum::http::HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
}

/// S3 桶名校验（3-63、小写字母数字与 `-`/`.`、首尾字母数字、不能像 IP、不能含 `..`）。
pub fn validate_bucket_name(name: &str) -> Result<(), S3Error> {
    let invalid = |msg: &str| S3Error::new(StatusCode::BAD_REQUEST, "InvalidBucketName", msg);

    if !(3..=63).contains(&name.len()) {
        return Err(invalid("Bucket name must be between 3 and 63 characters long."));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return Err(invalid(
            "Bucket name may only contain lowercase letters, digits, hyphens and dots.",
        ));
    }
    let first = name.chars().next().unwrap();
    let last = name.chars().last().unwrap();
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return Err(invalid("Bucket name must begin and end with a letter or digit."));
    }
    if name.contains("..") || name.contains(".-") || name.contains("-.") {
        return Err(invalid("Bucket name must not contain adjacent periods or period-hyphen pairs."));
    }
    if name.starts_with("xn--") || name.starts_with("sthree-") {
        return Err(invalid("Bucket name uses a reserved prefix."));
    }
    // 形如 192.168.1.1 的 IP 形式不允许。
    if name
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.')
        && name.split('.').count() == 4
        && name.split('.').all(|p| p.parse::<u8>().is_ok())
    {
        return Err(invalid("Bucket name must not be formatted as an IP address."));
    }
    Ok(())
}

/// S3 对象 key 的基础安全校验（比旧 OSS 规则宽松，但禁止路径穿越与 Windows 非法字符）。
pub fn validate_key(key: &str) -> Result<(), S3Error> {
    let invalid = |msg: &str| S3Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", msg);

    if key.is_empty() || key.len() > 1024 {
        return Err(invalid("Object key must be between 1 and 1024 bytes."));
    }
    if key.starts_with('/') {
        return Err(invalid("Object key must not start with '/'."));
    }
    if key
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(invalid(
            "Object key must not contain empty, '.' or '..' path segments.",
        ));
    }
    if key
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(invalid("Object key contains characters that are not supported by this server."));
    }
    Ok(())
}

/// 解析 `Range: bytes=...`，返回闭区间 `(start, end)`；非法返回 416 InvalidRange。
pub fn parse_byte_range(header: &str, size: u64) -> Result<(u64, u64), S3Error> {
    let invalid = || {
        S3Error::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "InvalidRange",
            "The requested range is not satisfiable.",
        )
    };
    let raw = header.strip_prefix("bytes=").ok_or_else(invalid)?;
    let (start, end) = raw.split_once('-').ok_or_else(invalid)?;

    if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| invalid())?;
        if suffix == 0 || size == 0 {
            return Err(invalid());
        }
        let start = size.saturating_sub(suffix);
        return Ok((start, size - 1));
    }

    let start: u64 = start.parse().map_err(|_| invalid())?;
    if start >= size {
        return Err(invalid());
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| invalid())?.min(size - 1)
    };
    if end < start {
        return Err(invalid());
    }
    Ok((start, end))
}

/// 去掉 ETag 的引号。
pub fn unquote_etag(value: &str) -> &str {
    value.trim().trim_matches('"')
}

/// ISO8601（S3 XML 使用毫秒精度）。
pub fn iso8601(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap())
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string()
}

/// 生成带 XML 声明的文档。
pub fn document(body: &str) -> String {
    format!("{XML_DECL}{body}")
}

/// 生成空 XML 元素。
pub fn element(name: &str, value: &str) -> String {
    format!("<{name}>{}</{name}>", escape(value))
}

/// S3 路由未实现的可选能力。
pub fn not_implemented(feature: &str) -> S3Error {
    S3Error::new(
        StatusCode::NOT_IMPLEMENTED,
        "NotImplemented",
        format!("{feature} is not supported by this S3-compatible server."),
    )
}

/// 请求是否由 S3 客户端发起（由鉴权中间件写入扩展）。
pub fn is_s3_request(s3: &Option<Extension<S3AuthContext>>) -> bool {
    s3.is_some()
}

