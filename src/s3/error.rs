//! S3 错误响应（XML），错误码与 AWS S3 语义对齐。

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::services::s3_signature::S3SigError;

use super::xml::{escape, XML_DECL};

/// 一个 S3 协议错误：HTTP 状态码 + S3 Error Code + 可读信息。
#[derive(Debug, Clone)]
pub struct S3Error {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub resource: String,
}

impl S3Error {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            resource: String::new(),
        }
    }

    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = resource.into();
        self
    }

    /// 签名层错误 -> S3 错误码。
    pub fn from_signature(err: S3SigError) -> Self {
        match err {
            S3SigError::MalformedAuthorization
            | S3SigError::MalformedCredential
            | S3SigError::MalformedSignedHeaders => Self::new(
                StatusCode::BAD_REQUEST,
                "AuthorizationHeaderMalformed",
                err.to_string(),
            ),
            S3SigError::RequestTimeTooSkewed => Self::new(
                StatusCode::FORBIDDEN,
                "RequestTimeTooSkewed",
                "The difference between the request time and the server's time is too large.",
            ),
            S3SigError::Expired => Self::new(
                StatusCode::FORBIDDEN,
                "AccessDenied",
                "Request has expired.",
            ),
            S3SigError::SignatureDoesNotMatch => Self::new(
                StatusCode::FORBIDDEN,
                "SignatureDoesNotMatch",
                "The request signature we calculated does not match the signature you provided.",
            ),
        }
    }

    pub fn invalid_access_key() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "InvalidAccessKeyId",
            "The AWS Access Key Id you provided does not exist in our records.",
        )
    }

    pub fn access_denied(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "AccessDenied", message)
    }

    pub fn no_such_bucket(bucket: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NoSuchBucket",
            format!("The specified bucket does not exist: {bucket}"),
        )
    }

    pub fn no_such_key(key: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NoSuchKey",
            format!("The specified key does not exist: {key}"),
        )
    }

    pub fn render_body(&self) -> String {
        let resource = if self.resource.is_empty() {
            String::new()
        } else {
            format!("<Resource>{}</Resource>", escape(&self.resource))
        };
        format!(
            "{XML_DECL}<Error><Code>{}</Code><Message>{}</Message>{}<RequestId>unknown</RequestId></Error>",
            escape(self.code),
            escape(&self.message),
            resource
        )
    }
}

impl IntoResponse for S3Error {
    fn into_response(self) -> Response {
        let body = self.render_body();
        (
            self.status,
            [(header::CONTENT_TYPE, "application/xml")],
            Body::from(body),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_is_s3_xml() {
        let err = S3Error::no_such_key("a&b.txt");
        let body = err.render_body();
        assert!(body.starts_with(XML_DECL));
        assert!(body.contains("<Code>NoSuchKey</Code>"));
        assert!(body.contains("a&amp;b.txt"));
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn signature_error_mapping() {
        let err = S3Error::from_signature(S3SigError::SignatureDoesNotMatch);
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.code, "SignatureDoesNotMatch");
        let err = S3Error::from_signature(S3SigError::RequestTimeTooSkewed);
        assert_eq!(err.code, "RequestTimeTooSkewed");
    }
}
