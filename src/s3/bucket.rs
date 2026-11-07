//! S3 Bucket 接口：Create / Delete / Head / GetLocation / DeleteObjects。
//!
//! 与旧 OSS `/buckets` 管理接口共用同一份 Bucket 元数据；S3 只是多了一层
//! 命名规则与 XML 响应格式。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::middleware::auth::AuthenticatedApp;
use crate::models::bucket::Bucket;
use crate::models::object::Object;

use super::error::S3Error;
use super::xml::{escape, S3_NS, XML_DECL};
use super::{
    bucket_for_app, internal_error, validate_bucket_name, S3State,
};

/// PUT /{bucket}：CreateBucket。
///
/// 桶名规则只在 S3 建桶时校验，不对存量桶回溯。
pub async fn create_bucket(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    Path(bucket_name): Path<String>,
) -> Response {
    let Some(auth) = auth else {
        return S3Error::access_denied("Anonymous access is not allowed.").into_response();
    };

    if let Err(err) = validate_bucket_name(&bucket_name) {
        return err.with_resource(format!("/{bucket_name}")).into_response();
    }

    match Bucket::get_by_name(&state.pool, &bucket_name).await {
        Ok(Some(existing)) => {
            let (code, message) = if existing.app_id == auth.app_id {
                (
                    "BucketAlreadyOwnedByYou",
                    "Your previous request to create the named bucket succeeded and you already own it.",
                )
            } else {
                (
                    "BucketAlreadyExists",
                    "The requested bucket name is not available.",
                )
            };
            return S3Error::new(StatusCode::CONFLICT, code, message)
                .with_resource(format!("/{bucket_name}"))
                .into_response();
        }
        Ok(None) => {}
        Err(err) => {
            tracing::error!("S3 CreateBucket: lookup failed: {}", err);
            return internal_error().into_response();
        }
    }

    let bucket = match Bucket::create(&state.pool, &auth.app_id, &bucket_name, false).await {
        Ok(bucket) => bucket,
        Err(err) => {
            if err.to_string().contains("already exists") {
                return S3Error::new(
                    StatusCode::CONFLICT,
                    "BucketAlreadyOwnedByYou",
                    "Your previous request to create the named bucket succeeded and you already own it.",
                )
                .into_response();
            }
            tracing::error!("S3 CreateBucket: failed to create: {}", err);
            return internal_error().into_response();
        }
    };

    let mut headers = HeaderMap::new();
    if let Ok(value) = format!("/{}", bucket.name).parse() {
        headers.insert(header::LOCATION, value);
    }
    (StatusCode::OK, headers).into_response()
}

/// DELETE /{bucket}：DeleteBucket（桶内仍有 active 对象时拒绝）。
pub async fn delete_bucket(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    Path(bucket_name): Path<String>,
) -> Response {
    let Some(auth) = auth else {
        return S3Error::access_denied("Anonymous access is not allowed.").into_response();
    };

    let bucket = match bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    match Object::count_by_bucket(&state.pool, &bucket.bucket_id).await {
        Ok(0) => {}
        Ok(_) => {
            return S3Error::new(
                StatusCode::CONFLICT,
                "BucketNotEmpty",
                "The bucket you tried to delete is not empty.",
            )
            .with_resource(format!("/{bucket_name}"))
            .into_response()
        }
        Err(err) => {
            tracing::error!("S3 DeleteBucket: count failed: {}", err);
            return internal_error().into_response();
        }
    }

    match Bucket::delete(&state.pool, &bucket.bucket_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => S3Error::no_such_bucket(&bucket_name).into_response(),
        Err(err) => {
            tracing::error!("S3 DeleteBucket: delete failed: {}", err);
            internal_error().into_response()
        }
    }
}

/// HEAD /{bucket}：HeadBucket。
pub async fn head_bucket(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    Path(bucket_name): Path<String>,
) -> Response {
    let Some(auth) = auth else {
        return StatusCode::FORBIDDEN.into_response();
    };

    match bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await {
        Ok(_) => (
            StatusCode::OK,
            [("x-amz-bucket-region", "us-east-1")],
        )
            .into_response(),
        Err(err) => err.into_response(),
    }
}

/// GET /{bucket}?location：GetBucketLocation。
pub fn get_location() -> Response {
    let body = format!(
        "{XML_DECL}<LocationConstraint xmlns=\"{S3_NS}\"></LocationConstraint>"
    );
    (StatusCode::OK, [("content-type", "application/xml")], body).into_response()
}

/// POST /{bucket}?delete：DeleteObjects（批量软删除）。
pub async fn post_bucket(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    Path(bucket_name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(auth) = auth else {
        return S3Error::access_denied("Anonymous access is not allowed.").into_response();
    };

    if !params.contains_key("delete") {
        return S3Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidRequest",
            "POST to a bucket is only supported for the DeleteObjects operation (?delete).",
        )
        .into_response();
    }

    let bucket = match bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    let text = String::from_utf8_lossy(&body);
    let keys = super::xml::tag_values(&text, "Key");
    let quiet = super::xml::tag_values(&text, "Quiet")
        .first()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if keys.is_empty() || keys.len() > 1000 {
        return S3Error::new(
            StatusCode::BAD_REQUEST,
            "MalformedXML",
            "The XML you provided was not well-formed or did not validate against our published schema.",
        )
        .into_response();
    }

    let mut deleted = String::new();
    let mut errors = String::new();
    for key in &keys {
        match Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, key).await {
            Ok(Some(obj)) => match Object::delete(&state.pool, &obj.object_id).await {
                Ok(_) => deleted.push_str(&format!(
                    "<Deleted>{}</Deleted>",
                    super::element("Key", key)
                )),
                Err(err) => {
                    tracing::error!("S3 DeleteObjects: soft delete failed: {}", err);
                    errors.push_str(&format!(
                        "<Error><Key>{}</Key><Code>InternalError</Code><Message>Failed to delete the object.</Message></Error>",
                        escape(key)
                    ));
                }
            },
            // S3 语义：对象不存在也算删除成功。
            Ok(None) => deleted.push_str(&format!(
                "<Deleted>{}</Deleted>",
                super::element("Key", key)
            )),
            Err(err) => {
                tracing::error!("S3 DeleteObjects: lookup failed: {}", err);
                errors.push_str(&format!(
                    "<Error><Key>{}</Key><Code>InternalError</Code><Message>Failed to look up the object.</Message></Error>",
                    escape(key)
                ));
            }
        }
    }

    let body = format!(
        "{XML_DECL}<DeleteResult xmlns=\"{S3_NS}\">{}</DeleteResult>",
        if quiet {
            errors
        } else {
            format!("{deleted}{errors}")
        }
    );
    (StatusCode::OK, [("content-type", "application/xml")], body).into_response()
}
