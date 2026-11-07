//! 错误类型：S3 服务端错误 + 传输层错误。

use std::fmt;

/// S3 服务端返回的结构化错误（来自 XML `<Error>` 或 HTTP 状态推断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    /// S3 错误码，例如 `NoSuchKey`、`AccessDenied`、`SignatureDoesNotMatch`。
    pub code: String,
    /// 人类可读的错误描述。
    pub message: String,
    /// HTTP 状态码。
    pub status: u16,
    /// 服务端请求 ID（本服务固定为 `unknown`，字段仍保留以兼容真实 S3）。
    pub request_id: Option<String>,
}

impl fmt::Display for S3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (HTTP {}): {}",
            self.code, self.status, self.message
        )
    }
}

impl std::error::Error for S3Error {}

impl S3Error {
    /// 对象 / 桶 / upload 不存在。
    pub fn is_not_found(&self) -> bool {
        matches!(
            self.code.as_str(),
            "NoSuchKey" | "NoSuchBucket" | "NoSuchUpload" | "NotFound"
        ) || self.status == 404
    }

    /// 鉴权失败（签名错误、凭证不存在、预签名过期、无权限）。
    pub fn is_access_denied(&self) -> bool {
        matches!(
            self.code.as_str(),
            "AccessDenied"
                | "InvalidAccessKeyId"
                | "SignatureDoesNotMatch"
                | "RequestTimeTooSkewed"
        ) || self.status == 403
    }

    /// 是否值得重试（429、5xx）；404 / 403 永不重试。
    pub fn is_retryable(&self) -> bool {
        self.status == 429 || (500..=599).contains(&self.status)
    }
}

/// SDK 统一错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 配置非法。
    #[error("配置错误: {0}")]
    Config(String),
    /// 本地文件 / 磁盘 I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 传输层错误（连接失败、DNS、TLS 等）。
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),
    /// 请求超时。SDK 会在重试次数内自动重试，最终仍失败时返回该错误。
    #[error("请求超时")]
    Timeout,
    /// 服务端返回的 S3 错误。
    #[error("S3 服务端错误: {0}")]
    Service(#[from] S3Error),
    /// 响应内容无法解析（例如期望 XML 却拿到空 body）。
    #[error("响应解析失败: {0}")]
    InvalidResponse(String),
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Service(e) if e.is_not_found())
    }

    pub fn is_access_denied(&self) -> bool {
        matches!(self, Error::Service(e) if e.is_access_denied())
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Timeout => true,
            // 传输层失败（连接、DNS、TLS、发送中断等）都属于网络错误，可重试。
            Error::Http(_) => true,
            Error::Service(e) => e.is_retryable(),
            _ => false,
        }
    }

    pub fn service(&self) -> Option<&S3Error> {
        match self {
            Error::Service(e) => Some(e),
            _ => None,
        }
    }
}

pub(crate) fn status_default_code(status: u16) -> &'static str {
    match status {
        400 => "BadRequest",
        401 => "Unauthorized",
        403 => "AccessDenied",
        404 => "NotFound",
        405 => "MethodNotAllowed",
        409 => "Conflict",
        412 => "PreconditionFailed",
        413 => "EntityTooLarge",
        416 => "InvalidRange",
        429 => "TooManyRequests",
        500 => "InternalError",
        501 => "NotImplemented",
        503 => "ServiceUnavailable",
        _ => "HttpError",
    }
}
