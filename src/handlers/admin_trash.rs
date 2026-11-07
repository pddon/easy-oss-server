//! 管理端回收站接口：列表 / 还原 / 彻底删除 / 批量操作。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::handlers::error_response;
use crate::middleware::admin_auth::AdminUser;
use crate::models::object::Object;
use crate::services::maintenance::MaintenanceService;
use crate::services::storage::StorageService;
use crate::store::{RestoreOutcome, StoreHandle};

/// 回收站 handler 状态。
#[derive(Clone)]
pub struct AdminTrashState {
    pub pool: StoreHandle,
    pub storage: StorageService,
    pub maintenance: Arc<MaintenanceService>,
}

/// 回收站列表查询参数。
#[derive(Debug, Deserialize)]
pub struct TrashListQuery {
    pub app_id: Option<String>,
    pub bucket_id: Option<String>,
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// 回收站条目响应。
#[derive(Debug, Serialize)]
pub struct TrashItem {
    pub object_id: String,
    pub app_id: String,
    pub app_name: Option<String>,
    pub bucket_id: String,
    pub bucket_name: Option<String>,
    pub key: String,
    pub size: i64,
    pub created_at: i64,
    pub deleted_at: i64,
}

/// 回收站列表响应（带总量与占用空间）。
#[derive(Debug, Serialize)]
pub struct TrashListResponse {
    pub items: Vec<TrashItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub total_size: i64,
}

/// 还原请求。
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRequest {
    #[serde(default)]
    pub overwrite: bool,
}

/// 批量操作请求。
#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    /// `restore` 或 `purge`。
    pub action: String,
    pub object_ids: Vec<String>,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Serialize)]
pub struct BatchFailure {
    pub object_id: String,
    pub reason: String,
}

/// 列出回收站。
pub async fn list_trash(
    _admin: AdminUser,
    State(state): State<AdminTrashState>,
    Query(query): Query<TrashListQuery>,
) -> Result<Json<TrashListResponse>, axum::response::Response> {
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);
    let app_id = query.app_id.as_deref().filter(|s| !s.is_empty());
    let bucket_id = query.bucket_id.as_deref().filter(|s| !s.is_empty());

    let objects = Object::list_trash(&state.pool, app_id, bucket_id, offset, limit)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list trash")
        })?;
    let total = Object::count_trash(&state.pool, app_id, bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to count trash")
        })?;
    let total_size = Object::total_trash_size(&state.pool, app_id, bucket_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to compute trash size")
        })?;

    let mut items = Vec::with_capacity(objects.len());
    for obj in objects {
        let app_name = state
            .pool
            .get_app(&obj.app_id)
            .await
            .ok()
            .flatten()
            .map(|a| a.name);
        let bucket_name = state
            .pool
            .get_bucket_by_id(&obj.bucket_id)
            .await
            .ok()
            .flatten()
            .map(|b| b.name);
        items.push(TrashItem {
            object_id: obj.object_id,
            app_id: obj.app_id,
            app_name,
            bucket_id: obj.bucket_id,
            bucket_name,
            key: obj.key,
            size: obj.size,
            created_at: obj.created_at,
            deleted_at: obj.deleted_at.unwrap_or_default(),
        });
    }

    Ok(Json(TrashListResponse {
        items,
        total,
        offset,
        limit,
        total_size,
    }))
}

/// 还原单个对象。
pub async fn restore_object(
    _admin: AdminUser,
    State(state): State<AdminTrashState>,
    Path(object_id): Path<String>,
    body: Option<Json<RestoreRequest>>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let overwrite = body.map(|b| b.0.overwrite).unwrap_or(false);
    match Object::restore(&state.pool, &object_id, overwrite).await {
        Ok(RestoreOutcome::Restored(obj)) => Ok(Json(serde_json::json!({
            "status": "restored",
            "object_id": obj.object_id,
            "bucket_id": obj.bucket_id,
            "key": obj.key,
        }))),
        Ok(RestoreOutcome::NotFound) => Err(error_response(StatusCode::NOT_FOUND, "Object not found")),
        Ok(RestoreOutcome::BucketMissing { bucket_id }) => Err(error_response(
            StatusCode::CONFLICT,
            format!(
                "Original bucket '{}' no longer exists; the object can only be purged",
                bucket_id
            ),
        )),
        Ok(RestoreOutcome::Conflict { existing_object_id }) => Err(error_response(
            StatusCode::CONFLICT,
            format!(
                "Key is already occupied by object '{}'; restore with overwrite=true to replace it",
                existing_object_id
            ),
        )),
        Err(e) => {
            tracing::error!("DB error: {}", e);
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to restore object",
            ))
        }
    }
}

/// 彻底删除单个对象：删除磁盘文件 + 删除元数据。
pub async fn purge_object(
    _admin: AdminUser,
    State(state): State<AdminTrashState>,
    Path(object_id): Path<String>,
) -> Result<StatusCode, axum::response::Response> {
    purge_one(&state, &object_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 批量还原 / 批量彻底删除。
pub async fn batch(
    _admin: AdminUser,
    State(state): State<AdminTrashState>,
    Json(body): Json<BatchRequest>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if body.object_ids.is_empty() {
        return Err(error_response(StatusCode::BAD_REQUEST, "object_ids is empty"));
    }
    let action = body.action.to_ascii_lowercase();
    if action != "restore" && action != "purge" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "action must be 'restore' or 'purge'",
        ));
    }

    let mut succeeded = 0i64;
    let mut failures: Vec<BatchFailure> = Vec::new();
    for object_id in &body.object_ids {
        let result = if action == "restore" {
            match Object::restore(&state.pool, object_id, body.overwrite).await {
                Ok(RestoreOutcome::Restored(_)) => Ok(()),
                Ok(RestoreOutcome::NotFound) => Err("Object not found".to_string()),
                Ok(RestoreOutcome::BucketMissing { bucket_id }) => {
                    Err(format!("Original bucket '{}' no longer exists", bucket_id))
                }
                Ok(RestoreOutcome::Conflict { existing_object_id }) => Err(format!(
                    "Key is already occupied by object '{}'",
                    existing_object_id
                )),
                Err(e) => Err(e.to_string()),
            }
        } else {
            purge_one_reason(&state, object_id)
                .await
                .map_err(|(_, message)| message)
        };

        match result {
            Ok(()) => succeeded += 1,
            Err(reason) => failures.push(BatchFailure {
                object_id: object_id.clone(),
                reason,
            }),
        }
    }

    Ok(Json(serde_json::json!({
        "action": action,
        "succeeded": succeeded,
        "failed": failures,
    })))
}

/// 彻底删除公共逻辑：先删文件（失败则保留元数据，下一轮清理重试），
/// 成功后再删元数据。只有这里会真正释放磁盘空间。
async fn purge_one(
    state: &AdminTrashState,
    object_id: &str,
) -> Result<(), axum::response::Response> {
    purge_one_reason(state, object_id)
        .await
        .map_err(|(status, message)| error_response(status, message))
}

/// 与 [`purge_one`] 相同，但以 `(状态码, 原因)` 返回，供批量操作汇总失败项。
async fn purge_one_reason(
    state: &AdminTrashState,
    object_id: &str,
) -> Result<(), (StatusCode, String)> {
    let obj = state
        .pool
        .get_object_any(object_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error".to_string())
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Object not found".to_string()))?;

    if obj.deleted_at.is_none() {
        return Err((
            StatusCode::CONFLICT,
            "Object is not in trash; delete it first".to_string(),
        ));
    }

    if let Err(e) = delete_file_with_retry(
        &state.storage,
        &obj.app_id,
        &obj.bucket_id,
        &obj.key,
        &obj.object_id,
    )
    .await
    {
        tracing::error!("Failed to delete file for {}: {}", obj.object_id, e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to delete the stored file; metadata kept for retry".to_string(),
        ));
    }

    state.pool.purge_object(object_id).await.map_err(|e| {
        tracing::error!("DB error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to purge object".to_string(),
        )
    })?;
    Ok(())
}

async fn delete_file_with_retry(
    storage: &StorageService,
    app_id: &str,
    bucket_id: &str,
    key: &str,
    object_id: &str,
) -> anyhow::Result<()> {
    let mut last_error = None;
    for attempt in 0..3u32 {
        match storage.delete(app_id, bucket_id, key, object_id).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1))
                    )
                    .await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("unknown delete error")))
}
