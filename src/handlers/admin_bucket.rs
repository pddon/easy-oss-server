//! Admin API bucket handlers (bypass app-level auth, use admin token).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::store::StoreHandle;
use crate::handlers::bucket::{BucketResponse, ListQuery};
use crate::handlers::{error_response, PaginatedResponse};
use crate::middleware::admin_auth::AdminUser;
use crate::models::bucket::{validate_bucket_name, Bucket};

/// Admin bucket handler state.
#[derive(Clone)]
pub struct AdminBucketState {
    pub pool: StoreHandle,
}

/// Create bucket request (admin).
#[derive(Deserialize)]
pub struct AdminCreateBucketRequest {
    pub name: String,
    #[serde(default)]
    pub is_public: bool,
}

/// Update bucket request.
#[derive(Deserialize)]
pub struct AdminUpdateBucketRequest {
    pub is_public: bool,
}

/// Create a bucket for a specific app (admin only).
pub async fn admin_create_bucket(
    _admin: AdminUser,
    State(state): State<AdminBucketState>,
    Path(app_id): Path<String>,
    Json(body): Json<AdminCreateBucketRequest>,
) -> Result<(StatusCode, Json<BucketResponse>), axum::response::Response> {
    use crate::models::app::App;

    // Verify app exists
    let app = App::get_by_id(&state.pool, &app_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "App not found"))?;

    if !validate_bucket_name(&body.name) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Invalid bucket name",
        ));
    }

    // Check global uniqueness
    if Bucket::get_by_name(&state.pool, &body.name)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .is_some()
    {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Bucket name already exists",
        ));
    }

    let bucket = Bucket::create(&state.pool, &app.app_id, &body.name, body.is_public)
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

/// List buckets for an app (admin only).
pub async fn admin_list_buckets(
    _admin: AdminUser,
    State(state): State<AdminBucketState>,
    Path(app_id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<PaginatedResponse<BucketResponse>>, axum::response::Response> {
    let limit = query.limit.clamp(1, 100);
    let offset = query.offset.max(0);

    let buckets = Bucket::list_by_app(&state.pool, &app_id, offset, limit)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list buckets")
        })?;

    let total = Bucket::count_by_app(&state.pool, &app_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
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

/// Update bucket visibility (admin only).
pub async fn admin_update_bucket(
    _admin: AdminUser,
    State(state): State<AdminBucketState>,
    Path((app_id, bucket_name)): Path<(String, String)>,
    Json(body): Json<AdminUpdateBucketRequest>,
) -> Result<Json<BucketResponse>, axum::response::Response> {
    let bucket = Bucket::get_by_app_and_name(&state.pool, &app_id, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    Bucket::set_public(&state.pool, &bucket.bucket_id, body.is_public)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update bucket")
        })?;

    Ok(Json(BucketResponse {
        bucket_id: bucket.bucket_id,
        name: bucket.name,
        is_public: body.is_public,
        created_at: bucket.created_at,
    }))
}

/// Delete a bucket (admin only).
pub async fn admin_delete_bucket(
    _admin: AdminUser,
    State(state): State<AdminBucketState>,
    Path((app_id, bucket_name)): Path<(String, String)>,
) -> Result<StatusCode, axum::response::Response> {
    let bucket = Bucket::get_by_app_and_name(&state.pool, &app_id, &bucket_name)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Bucket not found"))?;

    let count = crate::models::object::Object::count_by_bucket(&state.pool, &bucket.bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        })?;

    if count > 0 {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Bucket is not empty",
        ));
    }

    Bucket::delete(&state.pool, &bucket.bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete bucket")
        })?;

    Ok(StatusCode::NO_CONTENT)
}
