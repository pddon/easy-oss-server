//! Object (file) handlers - upload, download, delete, list.

use std::collections::HashMap;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::store::StoreHandle;
use crate::handlers::error_response;
use crate::middleware::auth::{AuthenticatedApp, S3AuthContext};
use crate::models::bucket::Bucket;
use crate::models::object::{validate_object_key, Object};
use crate::s3::S3State;
use crate::services::storage::StorageService;

/// Object handler state.
#[derive(Clone)]
pub struct ObjectState {
    pub pool: StoreHandle,
    pub storage: StorageService,
    pub max_upload_size: u64,
}

/// List objects query params (OSS-style).
#[derive(Deserialize)]
pub struct ListObjectsQuery {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub marker: String,
    #[serde(default = "default_max_keys")]
    #[serde(rename = "max-keys")]
    pub max_keys: i64,
}

fn default_max_keys() -> i64 {
    100
}

/// Object list response.
#[derive(Serialize)]
pub struct ListObjectsResponse {
    pub bucket: String,
    pub prefix: String,
    pub marker: String,
    pub max_keys: i64,
    pub is_truncated: bool,
    pub contents: Vec<ObjectInfo>,
}

/// Object info in list response.
#[derive(Serialize)]
pub struct ObjectInfo {
    pub key: String,
    pub size: i64,
    pub etag: String,
    pub content_type: String,
    pub last_modified: i64,
}

// ------- Helper functions -------

async fn get_bucket_for_app(
    pool: &StoreHandle,
    app_id: &str,
    bucket_name: &str,
) -> Result<Bucket, Response> {
    Bucket::get_by_app_and_name(pool, app_id, bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))
}

// ------- Upload -------

/// Upload an object (PUT /{bucket}/{key}).
pub async fn put_object(
    State(state): State<ObjectState>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path((bucket_name, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Response, Response> {
    let Some(auth) = auth else {
        if s3.is_some() {
            return Ok(crate::s3::error::S3Error::access_denied(
                "Anonymous access is not allowed.",
            )
            .into_response());
        }
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Missing authentication",
        ));
    };

    // S3（含分片 UploadPart 与 CopyObject）走兼容层。
    if s3.is_some() {
        return Ok(crate::s3::object::put_object(
            &S3State::from_object(&state),
            &auth.app_id,
            &bucket_name,
            &key,
            &headers,
            &params,
            body,
        )
        .await);
    }

    // Validate key
    if !validate_object_key(&key) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid object key",
        ));
    }

    let bucket = get_bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await?;

    // Get content type from header
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // Check content length
    let content_length: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    if let Some(cl) = content_length {
        if cl > state.max_upload_size {
            return Err(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "File too large. Maximum allowed size is {} bytes",
                    state.max_upload_size
                ),
            ));
        }
    }

    // Convert body to a stream of Result<Bytes, ...>, boxed for Unpin
    let stream = Box::pin(body.into_data_stream().map(|item| {
        item.map_err(|e| std::io::Error::other(e.to_string()))
    }));

    // Generate object ID
    let object_id = uuid::Uuid::new_v4().to_string();

    // Stream to disk
    let (etag, size) = state
        .storage
        .write_stream(
            &auth.app_id,
            &bucket.bucket_id,
            &key,
            &object_id,
            stream,
            state.max_upload_size,
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to write object: {}", e);
            let msg = if e.to_string().contains("exceeds maximum") {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            error_response(msg, "Failed to upload object")
        })?;

    // 对象记录写入：store 负责把同 key 的旧 active 版本移入回收站
    // （旧磁盘文件保留，只有彻底删除才会释放空间）。新文件写入失败时清理新文件。
    let obj = match Object::create_with_id(
        &state.pool,
        &object_id,
        &bucket.bucket_id,
        &key,
        size as i64,
        &content_type,
        &etag,
    )
    .await
    {
        Ok(obj) => obj,
        Err(e) => {
            tracing::error!("Failed to create object record: {}", e);
            let _ = state
                .storage
                .delete(&auth.app_id, &bucket.bucket_id, &key, &object_id)
                .await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to upload object",
            ));
        }
    };

    // Build response with ETag and other headers
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, format!("\"{}\"", obj.etag).parse().unwrap());
    headers.insert(
        header::CONTENT_LENGTH,
        obj.size.to_string().parse().unwrap(),
    );

    Ok((StatusCode::OK, headers).into_response())
}

// ------- Download -------

/// Download an object (GET /{bucket}/{key}).
/// Supports public access (no auth) and Range requests.
pub async fn get_object(
    State(state): State<ObjectState>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path((bucket_name, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, Response> {
    if s3.is_some() {
        return Ok(crate::s3::object::get_object(
            &S3State::from_object(&state),
            auth.as_ref().map(|a| a.app_id.as_str()),
            &bucket_name,
            &key,
            &params,
            &headers,
            false,
        )
        .await);
    }

    if !validate_object_key(&key) {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid object key"));
    }

    // Look up bucket by name (bucket names are globally unique)
    let bucket = get_bucket_by_name(&state.pool, &bucket_name)
        .await?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    // Check access: public or authenticated and owns the bucket
    let has_access = bucket.is_public
        || auth.as_ref().map(|a| a.app_id == bucket.app_id).unwrap_or(false);

    if !has_access {
        return Err(error_response(
            StatusCode::FORBIDDEN,
            "Access denied",
        ));
    }

    // Get object
    let obj = Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get object: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object not found"))?;

    // Handle Range header
    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());

    if let Some(range) = range_header {
        return handle_range_request(&state, &bucket, &obj, range).await;
    }

    // Full file download
    let (stream, size) = state
        .storage
        .read_stream(&bucket.app_id, &bucket.bucket_id, &key, &obj.object_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to read object: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read object")
        })?;

    let body = axum::body::Body::from_stream(stream);

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        obj.content_type.parse().unwrap(),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        size.to_string().parse().unwrap(),
    );
    response_headers.insert(
        header::ETAG,
        format!("\"{}\"", obj.etag).parse().unwrap(),
    );
    response_headers.insert(
        header::LAST_MODIFIED,
        obj.created_at_dt()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string()
            .parse()
            .unwrap(),
    );
    // Prevent MIME sniffing
    response_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().unwrap(),
    );
    response_headers.insert(
        header::ACCEPT_RANGES,
        "bytes".parse().unwrap(),
    );

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    *response.headers_mut() = response_headers;
    Ok(response.into_response())
}

/// Handle HTTP Range requests for an object.
pub async fn handle_range_request(
    state: &ObjectState,
    bucket: &Bucket,
    obj: &Object,
    range: &str,
) -> Result<Response, Response> {
    // Parse Range header (simple bytes=start-end format)
    let range = range
        .strip_prefix("bytes=")
        .ok_or_else(|| error_response(StatusCode::BAD_REQUEST, "Invalid Range header"))?;

    let parts: Vec<&str> = range.splitn(2, '-').collect();
    if parts.len() != 2 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid Range header format",
        ));
    }

    let file_size = obj.size as u64;

    let (start, end) = if parts[0].is_empty() {
        // Suffix range: bytes=-500 means last 500 bytes
        let suffix: u64 = parts[1].parse().map_err(|_| {
            error_response(StatusCode::BAD_REQUEST, "Invalid Range header value")
        })?;
        let start = file_size.saturating_sub(suffix);
        (start, file_size.saturating_sub(1))
    } else if parts[1].is_empty() {
        // bytes=1000- means from 1000 to end
        let start: u64 = parts[0].parse().map_err(|_| {
            error_response(StatusCode::BAD_REQUEST, "Invalid Range header value")
        })?;
        (start, file_size.saturating_sub(1))
    } else {
        let start: u64 = parts[0].parse().map_err(|_| {
            error_response(StatusCode::BAD_REQUEST, "Invalid Range header value")
        })?;
        let end: u64 = parts[1].parse().map_err(|_| {
            error_response(StatusCode::BAD_REQUEST, "Invalid Range header value")
        })?;
        (start, end.min(file_size.saturating_sub(1)))
    };

    if start >= file_size || start > end {
        return Err(error_response(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Range not satisfiable",
        ));
    }

    let (stream, range_len) = state
        .storage
        .read_range(
            &bucket.app_id,
            &bucket.bucket_id,
            &obj.key,
            &obj.object_id,
            start,
            end,
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to read range: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read object")
        })?;

    let body = axum::body::Body::from_stream(stream);

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        obj.content_type.parse().unwrap(),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        range_len.to_string().parse().unwrap(),
    );
    response_headers.insert(
        header::CONTENT_RANGE,
        format!("bytes {}-{}/{}", start, end, file_size)
            .parse()
            .unwrap(),
    );
    response_headers.insert(
        header::ETAG,
        format!("\"{}\"", obj.etag).parse().unwrap(),
    );
    response_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().unwrap(),
    );

    let mut response = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .body(body)
        .unwrap();
    *response.headers_mut() = response_headers;
    Ok(response.into_response())
}

// ------- Delete -------

/// Delete an object (DELETE /{bucket}/{key}).
pub async fn delete_object(
    State(state): State<ObjectState>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path((bucket_name, key)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<StatusCode, Response> {
    let Some(auth) = auth else {
        if s3.is_some() {
            return Err(crate::s3::error::S3Error::access_denied(
                "Anonymous access is not allowed.",
            )
            .into_response());
        }
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Missing authentication",
        ));
    };

    if s3.is_some() {
        let response = crate::s3::object::delete_object(
            &S3State::from_object(&state),
            &auth.app_id,
            &bucket_name,
            &key,
            &params,
        )
        .await;
        return if response.status() == StatusCode::NO_CONTENT {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(response)
        };
    }

    if !validate_object_key(&key) {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid object key"));
    }

    let bucket = get_bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await?;

    let obj = Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get object: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object not found"))?;

    // 软删除：只移入回收站，磁盘文件保留，只有“彻底删除”才会释放空间。
    Object::delete(&state.pool, &obj.object_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to move object to trash: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete object")
        })?;

    Ok(StatusCode::NO_CONTENT)
}

// ------- List -------

/// List objects in a bucket (GET /{bucket}).
pub async fn list_objects(
    State(state): State<ObjectState>,
    auth: Option<AuthenticatedApp>,
    s3: Option<Extension<S3AuthContext>>,
    Path(bucket_name): Path<String>,
    Query(query): Query<ListObjectsQuery>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Json<ListObjectsResponse>, Response> {
    let Some(auth) = auth else {
        if s3.is_some() {
            return Err(crate::s3::error::S3Error::access_denied(
                "Anonymous access is not allowed.",
            )
            .into_response());
        }
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Missing authentication",
        ));
    };

    if s3.is_some() {
        return Err(crate::s3::object::list_objects(
            &S3State::from_object(&state),
            &auth.app_id,
            &bucket_name,
            &raw,
        )
        .await);
    }

    let bucket = get_bucket_for_app(&state.pool, &auth.app_id, &bucket_name).await?;

    let max_keys = query.max_keys.clamp(1, 1000);

    // Request one extra to know if there are more results
    let objects = Object::list(
        &state.pool,
        &bucket.bucket_id,
        &query.prefix,
        &query.marker,
        max_keys + 1,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to list objects: {}", e);
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list objects")
    })?;

    let is_truncated = objects.len() as i64 > max_keys;
    let items: Vec<ObjectInfo> = objects
        .into_iter()
        .take(max_keys as usize)
        .map(|o| ObjectInfo {
            key: o.key,
            size: o.size,
            etag: o.etag,
            content_type: o.content_type,
            last_modified: o.created_at,
        })
        .collect();

    Ok(Json(ListObjectsResponse {
        bucket: bucket.name,
        prefix: query.prefix,
        marker: query.marker,
        max_keys,
        is_truncated,
        contents: items,
    }))
}

// ------- Helper: find bucket by name (for public access) -------

async fn get_bucket_by_name(pool: &StoreHandle, name: &str) -> Result<Option<Bucket>, Response> {
    // Since bucket names are globally unique (UNIQUE constraint),
    // we look up by name.
    let bucket = Bucket::get_by_name(pool, name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket by name: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?;
    Ok(bucket)
}
