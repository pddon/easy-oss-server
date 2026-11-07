//! Bucket management handlers.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::store::StoreHandle;
use crate::handlers::{error_response, PaginatedResponse};
use crate::middleware::auth::AuthenticatedApp;
use crate::models::bucket::{validate_bucket_name, Bucket};

/// Bucket handler state.
#[derive(Clone)]
pub struct BucketState {
    pub pool: StoreHandle,
}

/// Request body for creating a bucket.
#[derive(Deserialize)]
pub struct CreateBucketRequest {
    pub name: String,
    #[serde(default)]
    pub is_public: bool,
}

/// Request body for updating bucket visibility.
#[derive(Deserialize)]
pub struct UpdateBucketRequest {
    pub is_public: bool,
}

/// Response body for bucket info.
#[derive(Serialize)]
pub struct BucketResponse {
    pub bucket_id: String,
    pub name: String,
    pub is_public: bool,
    pub created_at: i64,
}

/// Bucket list query params.
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_offset() -> i64 {
    0
}

fn default_limit() -> i64 {
    20
}

/// Create a bucket (authenticated app).
pub async fn create_bucket(
    State(state): State<BucketState>,
    auth: AuthenticatedApp,
    Json(body): Json<CreateBucketRequest>,
) -> Result<(StatusCode, Json<BucketResponse>), axum::response::Response> {
    if !validate_bucket_name(&body.name) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid bucket name. Must be 3-63 chars, lowercase letters, numbers, hyphens, start/end with letter or number",
        ));
    }

    // Bucket names are globally unique because public downloads resolve by name.
    let existing = Bucket::get_by_name(&state.pool, &body.name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to check bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create bucket")
        })?;

    if existing.is_some() {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Bucket name already exists",
        ));
    }

    let bucket = Bucket::create(&state.pool, &auth.app_id, &body.name, body.is_public)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create bucket")
        })?;

    Ok((
        StatusCode::CREATED,
        Json(BucketResponse {
            bucket_id: bucket.bucket_id,
            name: bucket.name,
            is_public: bucket.is_public,
            created_at: bucket.created_at,
        }),
    ))
}

/// List buckets for an app (authenticated app).
pub async fn list_buckets(
    State(state): State<BucketState>,
    auth: AuthenticatedApp,
    Query(query): Query<ListQuery>,
) -> Result<Json<PaginatedResponse<BucketResponse>>, axum::response::Response> {
    let limit = query.limit.clamp(1, 100);
    let offset = query.offset.max(0);

    let buckets = Bucket::list_by_app(&state.pool, &auth.app_id, offset, limit)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list buckets: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list buckets")
        })?;

    let total = Bucket::count_by_app(&state.pool, &auth.app_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count buckets: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to count buckets")
        })?;

    let items: Vec<BucketResponse> = buckets
        .into_iter()
        .map(|b| BucketResponse {
            bucket_id: b.bucket_id,
            name: b.name,
            is_public: b.is_public,
            created_at: b.created_at,
        })
        .collect();

    Ok(Json(PaginatedResponse {
        items,
        total,
        offset,
        limit,
    }))
}

/// Get bucket info by name (authenticated app).
pub async fn get_bucket(
    State(state): State<BucketState>,
    auth: AuthenticatedApp,
    Path(bucket_name): Path<String>,
) -> Result<Json<BucketResponse>, axum::response::Response> {
    let bucket = Bucket::get_by_app_and_name(&state.pool, &auth.app_id, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get bucket")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    Ok(Json(BucketResponse {
        bucket_id: bucket.bucket_id,
        name: bucket.name,
        is_public: bucket.is_public,
        created_at: bucket.created_at,
    }))
}

/// Delete a bucket (authenticated app).
pub async fn delete_bucket(
    State(state): State<BucketState>,
    auth: AuthenticatedApp,
    Path(bucket_name): Path<String>,
) -> Result<StatusCode, axum::response::Response> {
    let bucket = Bucket::get_by_app_and_name(&state.pool, &auth.app_id, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete bucket")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    // Check if bucket has objects
    let count = crate::models::object::Object::count_by_bucket(&state.pool, &bucket.bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to count objects: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete bucket")
        })?;

    if count > 0 {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Bucket is not empty. Delete all objects first.",
        ));
    }

    let deleted = Bucket::delete(&state.pool, &bucket.bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to delete bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete bucket")
        })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(error_response(StatusCode::NOT_FOUND, "Bucket not found"))
    }
}

/// Update bucket visibility (authenticated app).
pub async fn update_bucket(
    State(state): State<BucketState>,
    auth: AuthenticatedApp,
    Path(bucket_name): Path<String>,
    Json(body): Json<UpdateBucketRequest>,
) -> Result<Json<BucketResponse>, axum::response::Response> {
    let bucket = Bucket::get_by_app_and_name(&state.pool, &auth.app_id, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update bucket")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    Bucket::set_public(&state.pool, &bucket.bucket_id, body.is_public)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update bucket: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update bucket")
        })?;

    Ok(Json(BucketResponse {
        bucket_id: bucket.bucket_id,
        name: bucket.name,
        is_public: body.is_public,
        created_at: bucket.created_at,
    }))
}
