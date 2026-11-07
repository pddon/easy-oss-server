//! Admin API object handlers (bypass app-level auth, use admin token).

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;

use crate::store::StoreHandle;
use crate::handlers::object::{
    handle_range_request, ListObjectsQuery, ListObjectsResponse, ObjectInfo, ObjectState,
};
use crate::handlers::error_response;
use crate::middleware::admin_auth::AdminUser;
use crate::models::bucket::Bucket;
use crate::models::object::{validate_object_key, Object};
use crate::services::storage::StorageService;

/// Admin object handler state.
#[derive(Clone)]
pub struct AdminObjectState {
    pub pool: StoreHandle,
    pub storage: StorageService,
    pub max_upload_size: u64,
}

impl From<AdminObjectState> for ObjectState {
    fn from(s: AdminObjectState) -> Self {
        ObjectState {
            pool: s.pool,
            storage: s.storage,
            max_upload_size: s.max_upload_size,
        }
    }
}

/// List objects in a bucket (admin only).
pub async fn admin_list_objects(
    _admin: AdminUser,
    State(state): State<AdminObjectState>,
    Path(bucket_name): Path<String>,
    Query(query): Query<ListObjectsQuery>,
) -> Result<Json<ListObjectsResponse>, Response> {
    let bucket = Bucket::get_by_name(&state.pool, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    let max_keys = query.max_keys.clamp(1, 1000);

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
    let contents: Vec<ObjectInfo> = objects
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
        contents,
    }))
}

/// Upload an object (admin only).
pub async fn admin_put_object(
    _admin: AdminUser,
    State(state): State<AdminObjectState>,
    Path((bucket_name, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Response, Response> {
    if !validate_object_key(&key) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid object key",
        ));
    }

    let bucket = Bucket::get_by_name(&state.pool, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let content_length: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    if let Some(cl) = content_length {
        if cl > state.max_upload_size {
            return Err(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "File too large",
            ));
        }
    }

    let stream = Box::pin(body.into_data_stream().map(|item| {
        item.map_err(|e| std::io::Error::other(e.to_string()))
    }));

    let object_id = uuid::Uuid::new_v4().to_string();

    let (etag, size) = state
        .storage
        .write_stream(
            &bucket.app_id,
            &bucket.bucket_id,
            &key,
            &object_id,
            stream,
            state.max_upload_size,
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to write object: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to upload object")
        })?;

    // 覆盖写：store 会把同 key 的旧版本移入回收站（旧文件保留）。
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
            tracing::error!("Failed to create object: {}", e);
            let _ = state
                .storage
                .delete(&bucket.app_id, &bucket.bucket_id, &key, &object_id)
                .await;
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to upload object",
            ));
        }
    };

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(header::ETAG, format!("\"{}\"", obj.etag).parse().unwrap());

    Ok((StatusCode::OK, resp_headers).into_response())
}

/// Download an object (admin only).
pub async fn admin_get_object(
    _admin: AdminUser,
    State(state): State<AdminObjectState>,
    Path((bucket_name, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Response> {
    if !validate_object_key(&key) {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid object key"));
    }

    let bucket = Bucket::get_by_name(&state.pool, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    let obj = Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object not found"))?;

    let obj_state: ObjectState = state.clone().into();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());

    if let Some(range) = range_header {
        return handle_range_request(&obj_state, &bucket, &obj, range).await;
    }

    let (stream, size) = state
        .storage
        .read_stream(&bucket.app_id, &bucket.bucket_id, &key, &obj.object_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to read object: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read object")
        })?;

    let body = axum::body::Body::from_stream(stream);

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        obj.content_type.parse().unwrap(),
    );
    resp_headers.insert(
        header::CONTENT_LENGTH,
        size.to_string().parse().unwrap(),
    );
    resp_headers.insert(
        header::ETAG,
        format!("\"{}\"", obj.etag).parse().unwrap(),
    );
    resp_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().unwrap(),
    );
    resp_headers.insert(
        header::ACCEPT_RANGES,
        "bytes".parse().unwrap(),
    );

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .unwrap();
    *response.headers_mut() = resp_headers;
    Ok(response.into_response())
}

/// Delete an object (admin only).
pub async fn admin_delete_object(
    _admin: AdminUser,
    State(state): State<AdminObjectState>,
    Path((bucket_name, key)): Path<(String, String)>,
) -> Result<StatusCode, Response> {
    if !validate_object_key(&key) {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid object key"));
    }

    let bucket = Bucket::get_by_name(&state.pool, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    let obj = Object::get_by_bucket_and_key(&state.pool, &bucket.bucket_id, &key)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Object not found"))?;

    // 软删除：移入回收站，磁盘文件保留（彻底删除才会释放空间）。
    Object::delete(&state.pool, &obj.object_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete object")
        })?;

    Ok(StatusCode::NO_CONTENT)
}
