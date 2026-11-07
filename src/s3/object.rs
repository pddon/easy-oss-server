//! S3 Object 接口：Put / Get / Head / Delete / Copy、ListObjectsV1/V2、
//! 以及分片上传全流程。

use std::collections::HashMap;

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use futures_util::StreamExt;

use crate::middleware::auth::{AuthenticatedApp, S3AuthContext};
use crate::models::bucket::Bucket;
use crate::models::object::Object;
use crate::services::multipart::{MultipartService, MultipartUpload};
use crate::store::StoreHandle;

use super::error::S3Error;
use super::xml::{escape, tag_values, S3_NS, XML_DECL};
use super::{
    append_meta_headers, bucket_for_app, collect_meta, element, internal_error, is_s3_request,
    iso8601, parse_byte_range, unquote_etag, validate_key, S3State,
};

// ---------------------------------------------------------------------------
// axum handler 包装（bucket 级路由直接注册这些）
// ---------------------------------------------------------------------------

/// HEAD /{bucket}/{key}：HeadObject。
pub async fn head_object(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path((bucket_name, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !is_s3_request(&s3) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let app_id = auth.as_ref().map(|a| a.app_id.as_str());
    get_object(
        &state,
        app_id,
        &bucket_name,
        &key,
        &params,
        &headers,
        true,
    )
    .await
}

/// POST /{bucket}/{key}：CreateMultipartUpload（`?uploads`）或
/// CompleteMultipartUpload（`?uploadId=`）。
pub async fn post_object(
    State(state): State<S3State>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path((bucket_name, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_s3_request(&s3) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(auth) = auth else {
        return S3Error::access_denied("Anonymous access is not allowed.").into_response();
    };
    if let Err(err) = validate_key(&key) {
        return err.into_response();
    }
    let bucket = match bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    if params.contains_key("uploads") {
        return create_multipart(&state, &bucket, &key, &headers).await;
    }
    if params.contains_key("uploadId") {
        return complete_multipart(&state, &bucket, &key, &params, &body).await;
    }

    S3Error::new(
        StatusCode::BAD_REQUEST,
        "InvalidRequest",
        "POST to an object is only supported for multipart upload operations.",
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// PutObject / CopyObject
// ---------------------------------------------------------------------------

/// PUT /{bucket}/{key}：PutObject / UploadPart / CopyObject。
pub async fn put_object(
    state: &S3State,
    app_id: &str,
    bucket_name: &str,
    key: &str,
    headers: &HeaderMap,
    params: &HashMap<String, String>,
    body: Body,
) -> Response {
    if let Err(err) = validate_key(key) {
        return err.with_resource(format!("/{bucket_name}/{key}")).into_response();
    }
    let bucket = match bucket_for_app(&state.pool, app_id, bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    if params.contains_key("partNumber") {
        return upload_part(state, &bucket, key, params, headers, body).await;
    }
    if let Some(source) = headers
        .get("x-amz-copy-source")
        .and_then(|v| v.to_str().ok())
    {
        return copy_object(state, app_id, &bucket, key, source, headers).await;
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_length: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    if let Some(len) = content_length {
        if len > state.max_upload_size {
            return S3Error::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "EntityTooLarge",
                format!(
                    "Your proposed upload exceeds the maximum allowed size of {} bytes.",
                    state.max_upload_size
                ),
            )
            .into_response();
        }
    }

    let object_id = uuid::Uuid::new_v4().to_string();
    let stream = Box::pin(body.into_data_stream().map(|item| {
        item.map_err(|e| std::io::Error::other(e.to_string()))
    }));
    let (etag, size) = match state
        .storage
        .write_stream(
            app_id,
            &bucket.bucket_id,
            key,
            &object_id,
            stream,
            state.max_upload_size,
        )
        .await
    {
        Ok(result) => result,
        Err(err) => {
            let message = err.to_string();
            tracing::error!("S3 PutObject: write failed: {}", err);
            if message.contains("exceeds maximum") {
                return S3Error::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "EntityTooLarge",
                    format!(
                        "Your proposed upload exceeds the maximum allowed size of {} bytes.",
                        state.max_upload_size
                    ),
                )
                .into_response();
            }
            return internal_error().into_response();
        }
    };

    if let Err(err) = verify_content_md5(headers, &etag) {
        let _ = state
            .storage
            .delete(app_id, &bucket.bucket_id, key, &object_id)
            .await;
        return err.into_response();
    }

    let meta = collect_meta(headers);
    if let Err(err) = write_sidecar(state, app_id, &bucket.bucket_id, key, &object_id, &meta).await
    {
        tracing::error!("S3 PutObject: failed to write metadata sidecar: {}", err);
        let _ = state
            .storage
            .delete(app_id, &bucket.bucket_id, key, &object_id)
            .await;
        return internal_error().into_response();
    }

    match Object::create_with_id(
        &state.pool,
        &object_id,
        &bucket.bucket_id,
        key,
        size as i64,
        &content_type,
        &etag,
    )
    .await
    {
        Ok(obj) => {
            let mut out = HeaderMap::new();
            out.insert(header::ETAG, quoted(&obj.etag));
            (StatusCode::OK, out).into_response()
        }
        Err(err) => {
            tracing::error!("S3 PutObject: failed to create object record: {}", err);
            let _ = state
                .storage
                .delete(app_id, &bucket.bucket_id, key, &object_id)
                .await;
            internal_error().into_response()
        }
    }
}

/// 复制对象。`x-amz-copy-source` 形如 `/{bucket}/{key}`，需先做 URL 解码。
async fn copy_object(
    state: &S3State,
    app_id: &str,
    dst_bucket: &Bucket,
    dst_key: &str,
    source: &str,
    headers: &HeaderMap,
) -> Response {
    let decoded = urlencoding::decode(source.trim())
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| source.trim().to_string());
    let source = decoded.trim_start_matches('/');
    let Some((src_bucket_name, src_key)) = source.split_once('/') else {
        return S3Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            "x-amz-copy-source must be of the form /{bucket}/{key}.",
        )
        .into_response();
    };

    let src_bucket = match bucket_for_app(&state.pool, app_id, src_bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };
    let src = match Object::get_by_bucket_and_key(&state.pool, &src_bucket.bucket_id, src_key).await
    {
        Ok(Some(obj)) => obj,
        Ok(None) => return S3Error::no_such_key(src_key).into_response(),
        Err(err) => {
            tracing::error!("S3 CopyObject: lookup failed: {}", err);
            return internal_error().into_response();
        }
    };

    let object_id = uuid::Uuid::new_v4().to_string();
    if let Err(err) = state
        .storage
        .copy_object(
            &src.app_id,
            &src.bucket_id,
            &src.key,
            &src.object_id,
            app_id,
            &dst_bucket.bucket_id,
            dst_key,
            &object_id,
        )
        .await
    {
        tracing::error!("S3 CopyObject: copy failed: {}", err);
        return internal_error().into_response();
    }

    let replace = headers
        .get("x-amz-metadata-directive")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("REPLACE"))
        .unwrap_or(false);
    let meta = if replace {
        collect_meta(headers)
    } else {
        state
            .storage
            .read_meta(&src.app_id, &src.bucket_id, &src.key, &src.object_id)
            .await
            .unwrap_or(None)
            .unwrap_or_default()
    };
    let content_type = if replace {
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(&src.content_type)
            .to_string()
    } else {
        src.content_type.clone()
    };

    if let Err(err) = write_sidecar(
        state,
        app_id,
        &dst_bucket.bucket_id,
        dst_key,
        &object_id,
        &meta,
    )
    .await
    {
        tracing::error!("S3 CopyObject: failed to write metadata sidecar: {}", err);
        let _ = state
            .storage
            .delete(app_id, &dst_bucket.bucket_id, dst_key, &object_id)
            .await;
        return internal_error().into_response();
    }

    match Object::create_with_id(
        &state.pool,
        &object_id,
        &dst_bucket.bucket_id,
        dst_key,
        src.size,
        &content_type,
        &src.etag,
    )
    .await
    {
        Ok(obj) => {
            let body = format!(
                "{XML_DECL}<CopyObjectResult xmlns=\"{S3_NS}\"><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>",
                iso8601(obj.created_at),
                escape(&format!("\"{}\"", obj.etag))
            );
            xml_ok(body)
        }
        Err(err) => {
            tracing::error!("S3 CopyObject: failed to create record: {}", err);
            let _ = state
                .storage
                .delete(app_id, &dst_bucket.bucket_id, dst_key, &object_id)
                .await;
            internal_error().into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// GetObject / HeadObject
// ---------------------------------------------------------------------------

/// GET / HEAD /{bucket}/{key}（以及 `?uploadId=` 的 ListParts）。
pub async fn get_object(
    state: &S3State,
    app_id: Option<&str>,
    bucket_name: &str,
    key: &str,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let bucket = match Bucket::get_by_name(&state.pool, bucket_name).await {
        Ok(Some(bucket)) => bucket,
        Ok(None) => return S3Error::no_such_bucket(bucket_name).into_response(),
        Err(err) => {
            tracing::error!("S3 GetObject: bucket lookup failed: {}", err);
            return internal_error().into_response();
        }
    };

    let allowed = bucket.is_public || app_id.map(|id| id == bucket.app_id).unwrap_or(false);
    if !allowed {
        return S3Error::access_denied("Access Denied.").into_response();
    }

    if params.contains_key("uploadId") {
        let Some(app_id) = app_id else {
            return S3Error::access_denied("Anonymous access is not allowed.").into_response();
        };
        if app_id != bucket.app_id {
            return S3Error::access_denied("Access Denied.").into_response();
        }
        return list_parts(state, &bucket, key, params).await;
    }

    let obj = match Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, key).await {
        Ok(Some(obj)) => obj,
        Ok(None) => return S3Error::no_such_key(key).into_response(),
        Err(err) => {
            tracing::error!("S3 GetObject: object lookup failed: {}", err);
            return internal_error().into_response();
        }
    };

    // 条件请求
    if let Some(if_match) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) {
        if !etag_matches(if_match, &obj.etag) {
            return S3Error::new(
                StatusCode::PRECONDITION_FAILED,
                "PreconditionFailed",
                "At least one of the pre-conditions you specified did not hold.",
            )
            .into_response();
        }
    }
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if etag_matches(if_none_match, &obj.etag) {
            let mut out = HeaderMap::new();
            out.insert(header::ETAG, quoted(&obj.etag));
            out.insert(
                header::LAST_MODIFIED,
                last_modified(&obj),
            );
            return (StatusCode::NOT_MODIFIED, out).into_response();
        }
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|value| parse_byte_range(value, obj.size as u64));

    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&obj.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    out.insert(header::ETAG, quoted(&obj.etag));
    out.insert(header::LAST_MODIFIED, last_modified(&obj));
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let meta = state
        .storage
        .read_meta(&obj.app_id, &obj.bucket_id, &obj.key, &obj.object_id)
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    append_meta_headers(&mut out, &meta);

    let (status, body) = match range {
        Some(Ok((start, end))) => {
            if head {
                let len = end - start + 1;
                out.insert(header::CONTENT_LENGTH, header_value(len));
                out.insert(
                    header::CONTENT_RANGE,
                    header_value(format!("bytes {start}-{end}/{}", obj.size)),
                );
                (StatusCode::PARTIAL_CONTENT, Body::empty())
            } else {
                match state
                    .storage
                    .read_range(
                        &obj.app_id,
                        &obj.bucket_id,
                        &obj.key,
                        &obj.object_id,
                        start,
                        end,
                    )
                    .await
                {
                    Ok((stream, len)) => {
                        out.insert(header::CONTENT_LENGTH, header_value(len));
                        out.insert(
                            header::CONTENT_RANGE,
                            header_value(format!("bytes {start}-{end}/{}", obj.size)),
                        );
                        (
                            StatusCode::PARTIAL_CONTENT,
                            Body::from_stream(stream),
                        )
                    }
                    Err(err) => {
                        tracing::error!("S3 GetObject: range read failed: {}", err);
                        return internal_error().into_response();
                    }
                }
            }
        }
        Some(Err(err)) => return err.into_response(),
        None => {
            out.insert(header::CONTENT_LENGTH, header_value(obj.size as u64));
            if head {
                (StatusCode::OK, Body::empty())
            } else {
                match state
                    .storage
                    .read_stream(&obj.app_id, &obj.bucket_id, &obj.key, &obj.object_id)
                    .await
                {
                    Ok((stream, _)) => (StatusCode::OK, Body::from_stream(stream)),
                    Err(err) => {
                        tracing::error!("S3 GetObject: read failed: {}", err);
                        return internal_error().into_response();
                    }
                }
            }
        }
    };

    let mut response = Response::builder()
        .status(status)
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    *response.headers_mut() = out;
    response
}

// ---------------------------------------------------------------------------
// DeleteObject
// ---------------------------------------------------------------------------

/// DELETE /{bucket}/{key}：DeleteObject（软删除进回收站；不存在也返回 204）。
pub async fn delete_object(
    state: &S3State,
    app_id: &str,
    bucket_name: &str,
    key: &str,
    params: &HashMap<String, String>,
) -> Response {
    let bucket = match bucket_for_app(&state.pool, app_id, bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    if params.contains_key("uploadId") {
        return abort_multipart(state, &bucket, key, params).await;
    }

    match Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, key).await {
        Ok(Some(obj)) => {
            if let Err(err) = Object::delete(&state.pool, &obj.object_id).await {
                tracing::error!("S3 DeleteObject: soft delete failed: {}", err);
                return internal_error().into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        // S3 语义：删除不存在的对象也返回 204。
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!("S3 DeleteObject: lookup failed: {}", err);
            internal_error().into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// ListObjects V1 / V2
// ---------------------------------------------------------------------------

enum Entry {
    Key(Object),
    Prefix(String),
}

impl Entry {
    fn same_group(&self, other: &Entry) -> bool {
        match (self, other) {
            (Entry::Key(a), Entry::Key(b)) => a.key == b.key,
            (Entry::Prefix(a), Entry::Prefix(b)) => a == b,
            _ => false,
        }
    }

    fn render(&self) -> String {
        match self {
            Entry::Key(obj) => format!(
                "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                escape(&obj.key),
                iso8601(obj.created_at),
                escape(&format!("\"{}\"", obj.etag)),
                obj.size
            ),
            Entry::Prefix(prefix) => {
                format!("<CommonPrefixes>{}</CommonPrefixes>", element("Prefix", prefix))
            }
        }
    }
}

/// GET /{bucket}：ListObjectsV2（`?location` 时返回 GetBucketLocation）。
pub async fn list_objects(
    state: &S3State,
    app_id: &str,
    bucket_name: &str,
    params: &HashMap<String, String>,
) -> Response {
    // 桶存在性必须先校验：GetBucketLocation 对已删除的桶不能返回 200。
    let bucket = match bucket_for_app(&state.pool, app_id, bucket_name).await {
        Ok(bucket) => bucket,
        Err(err) => return err.into_response(),
    };

    if params.contains_key("location") {
        return super::bucket::get_location();
    }

    let prefix = params.get("prefix").cloned().unwrap_or_default();
    let delimiter = params.get("delimiter").cloned().unwrap_or_default();
    let max_keys = match params.get("max-keys") {
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(value) => value.min(1000),
            Err(_) => {
                return S3Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidArgument",
                    "max-keys must be a non-negative integer.",
                )
                .into_response()
            }
        },
        None => 1000,
    };

    let v2 = params.get("list-type").map(|v| v.as_str()) == Some("2");
    let token = if v2 {
        params.get("continuation-token").cloned()
    } else {
        params.get("marker").cloned()
    };
    let start_after = params.get("start-after").cloned().unwrap_or_default();
    let start_marker = token
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| start_after.clone());

    let (entries, truncated, next_marker) = match collect_entries(
        &state.pool,
        &bucket.bucket_id,
        &prefix,
        &delimiter,
        &start_marker,
        max_keys,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => return err.into_response(),
    };

    let mut body = String::new();
    body.push_str(&format!("<ListBucketResult xmlns=\"{S3_NS}\">"));
    body.push_str(&element("Name", &bucket.name));
    body.push_str(&format!("<Prefix>{}</Prefix>", escape(&prefix)));
    if v2 {
        body.push_str(&format!("<KeyCount>{}</KeyCount>", entries.len()));
    } else {
        body.push_str(&format!(
            "<Marker>{}</Marker>",
            escape(token.as_deref().unwrap_or_default())
        ));
    }
    body.push_str(&format!("<MaxKeys>{max_keys}</MaxKeys>"));
    if !delimiter.is_empty() {
        body.push_str(&format!("<Delimiter>{}</Delimiter>", escape(&delimiter)));
    }
    body.push_str(&format!("<IsTruncated>{}</IsTruncated>", truncated));
    if let Some(token) = token.as_ref().filter(|t| !t.is_empty()) {
        if v2 {
            body.push_str(&format!("<ContinuationToken>{}</ContinuationToken>", escape(token)));
        }
    }
    if v2 && !start_after.is_empty() {
        body.push_str(&format!("<StartAfter>{}</StartAfter>", escape(&start_after)));
    }
    for entry in &entries {
        body.push_str(&entry.render());
    }
    if truncated {
        if v2 {
            body.push_str(&format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                escape(&next_marker)
            ));
        } else {
            body.push_str(&format!("<NextMarker>{}</NextMarker>", escape(&next_marker)));
        }
    }
    body.push_str("</ListBucketResult>");

    xml_ok(format!("{XML_DECL}{body}"))
}

/// 按 S3 语义遍历：delimiter 分组、CommonPrefixes、计数到 max-keys 即截断。
async fn collect_entries(
    pool: &StoreHandle,
    bucket_id: &str,
    prefix: &str,
    delimiter: &str,
    start_marker: &str,
    max_keys: usize,
) -> Result<(Vec<Entry>, bool, String), S3Error> {
    if max_keys == 0 {
        return Ok((Vec::new(), false, String::new()));
    }

    let mut entries: Vec<Entry> = Vec::new();
    let mut marker = start_marker.to_string();
    let mut last_key = start_marker.to_string();
    let mut truncated = false;

    'outer: loop {
        let batch = Object::list(pool, bucket_id, prefix, &marker, 1000)
            .await
            .map_err(|err| {
                tracing::error!("S3 ListObjects: query failed: {}", err);
                internal_error()
            })?;
        if batch.is_empty() {
            break;
        }
        let previous_marker = marker.clone();
        for obj in batch {
            marker = obj.key.clone();
            let entry = if delimiter.is_empty() {
                Entry::Key(obj.clone())
            } else {
                let rest = obj.key.get(prefix.len()..).unwrap_or("");
                match rest.find(delimiter) {
                    Some(index) => Entry::Prefix(format!(
                        "{}{}{}",
                        prefix,
                        &rest[..index],
                        delimiter
                    )),
                    None => Entry::Key(obj.clone()),
                }
            };

            if entries.last().map(|last| last.same_group(&entry)).unwrap_or(false) {
                last_key = obj.key.clone();
                continue;
            }
            if entries.len() >= max_keys {
                truncated = true;
                break 'outer;
            }
            entries.push(entry);
            last_key = obj.key.clone();
        }
        // 防御：后端没有推进 marker 时避免死循环。
        if marker == previous_marker {
            break;
        }
    }

    Ok((entries, truncated, last_key))
}

// ---------------------------------------------------------------------------
// Multipart Upload
// ---------------------------------------------------------------------------

async fn create_multipart(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    headers: &HeaderMap,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let service = MultipartService::new(&state.storage);
    let upload = match service
        .create(
            &bucket.app_id,
            &bucket.bucket_id,
            key,
            &content_type,
            collect_meta(headers),
        )
        .await
    {
        Ok(upload) => upload,
        Err(err) => {
            tracing::error!("S3 CreateMultipartUpload failed: {}", err);
            return internal_error().into_response();
        }
    };
    let body = format!(
        "{XML_DECL}<InitiateMultipartUploadResult xmlns=\"{S3_NS}\">\
         {}{}{}</InitiateMultipartUploadResult>",
        element("Bucket", &bucket.name),
        element("Key", key),
        element("UploadId", &upload.upload_id)
    );
    xml_ok(body)
}

async fn load_upload(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    params: &HashMap<String, String>,
) -> Result<MultipartUpload, S3Error> {
    let upload_id = params.get("uploadId").ok_or_else(|| {
        S3Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            "uploadId is required.",
        )
    })?;
    let service = MultipartService::new(&state.storage);
    let upload = service
        .load(upload_id)
        .await
        .map_err(|err| {
            tracing::error!("S3 multipart: failed to load upload: {}", err);
            internal_error()
        })?
        .ok_or_else(|| no_such_upload(upload_id))?;
    if upload.app_id != bucket.app_id
        || upload.bucket_id != bucket.bucket_id
        || upload.key != key
    {
        return Err(no_such_upload(upload_id));
    }
    Ok(upload)
}

fn no_such_upload(upload_id: &str) -> S3Error {
    S3Error::new(
        StatusCode::NOT_FOUND,
        "NoSuchUpload",
        format!("The specified upload does not exist: {upload_id}"),
    )
}

async fn upload_part(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    if headers.contains_key("x-amz-copy-source") {
        return super::not_implemented("UploadPartCopy").into_response();
    }
    let part_number = match params
        .get("partNumber")
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|v| (1..=10_000).contains(v))
    {
        Some(value) => value,
        None => {
            return S3Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "partNumber must be an integer between 1 and 10000.",
            )
            .into_response()
        }
    };
    let upload = match load_upload(state, bucket, key, params).await {
        Ok(upload) => upload,
        Err(err) => return err.into_response(),
    };

    let service = MultipartService::new(&state.storage);
    let stream = Box::pin(body.into_data_stream().map(|item| {
        item.map_err(|e| std::io::Error::other(e.to_string()))
    }));
    match service
        .put_part(
            &upload.upload_id,
            part_number,
            stream,
            state.max_upload_size,
        )
        .await
    {
        Ok(part) => {
            // Content-MD5 可选：带了就校验，不一致时不能把分片留在 upload 里。
            if let Err(err) = verify_content_md5(headers, &part.etag) {
                if let Err(cleanup) = service.remove_part(&upload.upload_id, part_number).await {
                    tracing::error!("S3 UploadPart: failed to rollback part: {}", cleanup);
                }
                return err.into_response();
            }
            let mut out = HeaderMap::new();
            out.insert(header::ETAG, quoted(&part.etag));
            (StatusCode::OK, out).into_response()
        }
        Err(err) => {
            let message = err.to_string();
            tracing::error!("S3 UploadPart failed: {}", err);
            if message.contains("NoSuchUpload") {
                return no_such_upload(&upload.upload_id).into_response();
            }
            if message.contains("exceeds maximum") {
                return S3Error::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "EntityTooLarge",
                    format!(
                        "Your proposed upload exceeds the maximum allowed size of {} bytes.",
                        state.max_upload_size
                    ),
                )
                .into_response();
            }
            internal_error().into_response()
        }
    }
}

async fn complete_multipart(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    params: &HashMap<String, String>,
    body: &Bytes,
) -> Response {
    let upload = match load_upload(state, bucket, key, params).await {
        Ok(upload) => upload,
        Err(err) => return err.into_response(),
    };

    let text = String::from_utf8_lossy(body);
    let mut parts: Vec<(i32, String)> = Vec::new();
    for segment in text.split("<Part>").skip(1) {
        let numbers = tag_values(segment, "PartNumber");
        let etags = tag_values(segment, "ETag");
        if let (Some(number), Some(etag)) = (numbers.first(), etags.first()) {
            if let Ok(number) = number.trim().parse::<i32>() {
                parts.push((number, etag.trim().to_string()));
            }
        }
    }
    if parts.is_empty() {
        return S3Error::new(
            StatusCode::BAD_REQUEST,
            "MalformedXML",
            "The XML you provided was not well-formed or did not specify any parts.",
        )
        .into_response();
    }

    let object_id = uuid::Uuid::new_v4().to_string();
    let dest = state
        .storage
        .object_path(&bucket.app_id, &bucket.bucket_id, key, &object_id);
    let service = MultipartService::new(&state.storage);
    let completed = match service
        .complete(&upload.upload_id, &parts, &dest)
        .await
    {
        Ok(completed) => completed,
        Err(err) => {
            let message = err.to_string();
            tracing::error!("S3 CompleteMultipartUpload failed: {}", err);
            if message.contains("InvalidPartOrder") {
                return S3Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidPartOrder",
                    "The list of parts was not in ascending order.",
                )
                .into_response();
            }
            if message.contains("InvalidPart") {
                return S3Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidPart",
                    "One or more of the specified parts could not be found or the ETag did not match.",
                )
                .into_response();
            }
            if message.contains("NoSuchUpload") {
                return no_such_upload(&upload.upload_id).into_response();
            }
            return internal_error().into_response();
        }
    };

    if let Err(err) = write_sidecar(
        state,
        &bucket.app_id,
        &bucket.bucket_id,
        key,
        &object_id,
        &upload.meta,
    )
    .await
    {
        tracing::error!("S3 CompleteMultipartUpload: sidecar failed: {}", err);
        let _ = state
            .storage
            .delete(&bucket.app_id, &bucket.bucket_id, key, &object_id)
            .await;
        return internal_error().into_response();
    }

    let obj = match Object::create_with_id(
        &state.pool,
        &object_id,
        &bucket.bucket_id,
        key,
        completed.size as i64,
        &upload.content_type,
        &completed.etag,
    )
    .await
    {
        Ok(obj) => obj,
        Err(err) => {
            tracing::error!("S3 CompleteMultipartUpload: record failed: {}", err);
            let _ = state
                .storage
                .delete(&bucket.app_id, &bucket.bucket_id, key, &object_id)
                .await;
            return internal_error().into_response();
        }
    };

    let _ = service.abort(&upload.upload_id).await;

    let body = format!(
        "{XML_DECL}<CompleteMultipartUploadResult xmlns=\"{S3_NS}\">\
         {}<Location>/{}/{}</Location>{}<ETag>{}</ETag></CompleteMultipartUploadResult>",
        element("Bucket", &bucket.name),
        bucket.name,
        escape(key),
        element("Key", key),
        escape(&format!("\"{}\"", obj.etag))
    );
    xml_ok(body)
}

async fn abort_multipart(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    params: &HashMap<String, String>,
) -> Response {
    let upload = match load_upload(state, bucket, key, params).await {
        Ok(upload) => upload,
        Err(err) => return err.into_response(),
    };
    let service = MultipartService::new(&state.storage);
    if let Err(err) = service.abort(&upload.upload_id).await {
        tracing::error!("S3 AbortMultipartUpload failed: {}", err);
        return internal_error().into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn list_parts(
    state: &S3State,
    bucket: &Bucket,
    key: &str,
    params: &HashMap<String, String>,
) -> Response {
    let upload = match load_upload(state, bucket, key, params).await {
        Ok(upload) => upload,
        Err(err) => return err.into_response(),
    };
    let service = MultipartService::new(&state.storage);
    let parts = match service.list_parts(&upload.upload_id).await {
        Ok(parts) => parts,
        Err(err) => {
            tracing::error!("S3 ListParts failed: {}", err);
            return internal_error().into_response();
        }
    };

    let mut body = String::new();
    body.push_str(&format!(
        "<ListPartsResult xmlns=\"{S3_NS}\">"
    ));
    body.push_str(&element("Bucket", &bucket.name));
    body.push_str(&element("Key", key));
    body.push_str(&element("UploadId", &upload.upload_id));
    body.push_str("<StorageClass>STANDARD</StorageClass>");
    body.push_str(&format!(
        "<PartNumberMarker>0</PartNumberMarker><NextPartNumberMarker>{}</NextPartNumberMarker><MaxParts>1000</MaxParts><IsTruncated>false</IsTruncated>",
        parts.last().map(|p| p.part_number).unwrap_or(0)
    ));
    for part in &parts {
        body.push_str(&format!(
            "<Part><PartNumber>{}</PartNumber><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size></Part>",
            part.part_number,
            iso8601(upload.created_at),
            escape(&format!("\"{}\"", part.etag)),
            part.size
        ));
    }
    body.push_str("</ListPartsResult>");
    xml_ok(format!("{XML_DECL}{body}"))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

async fn write_sidecar(
    state: &S3State,
    app_id: &str,
    bucket_id: &str,
    key: &str,
    object_id: &str,
    meta: &HashMap<String, String>,
) -> anyhow::Result<()> {
    if meta.is_empty() {
        return Ok(());
    }
    state
        .storage
        .write_meta(app_id, bucket_id, key, object_id, meta)
        .await
}

fn verify_content_md5(headers: &HeaderMap, etag_hex: &str) -> Result<(), S3Error> {
    let Some(value) = headers
        .get("content-md5")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim())
    else {
        return Ok(());
    };
    let expected = hex_to_base64(etag_hex).unwrap_or_default();
    if value != expected {
        return Err(S3Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "The Content-MD5 you specified did not match what we received.",
        ));
    }
    Ok(())
}

fn hex_to_base64(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let high = (chunk[0] as char).to_digit(16)?;
        let low = (chunk[1] as char).to_digit(16)?;
        bytes.push((high * 16 + low) as u8);
    }
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn etag_matches(header_value: &str, etag: &str) -> bool {
    if header_value.trim() == "*" {
        return true;
    }
    header_value
        .split(',')
        .any(|candidate| unquote_etag(candidate.trim()) == etag)
}

fn quoted(etag: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or_else(|_| HeaderValue::from_static("\"\""))
}

fn header_value(value: impl ToString) -> HeaderValue {
    HeaderValue::from_str(&value.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

fn last_modified(obj: &Object) -> HeaderValue {
    HeaderValue::from_str(
        &obj.created_at_dt()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string(),
    )
    .unwrap_or_else(|_| HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"))
}

fn xml_ok(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        body,
    )
        .into_response()
}
