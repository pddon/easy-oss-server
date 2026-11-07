//! 管理端维护接口：自检 / 清理状态、手动触发。

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::config::StartupCheckMode;
use crate::handlers::error_response;
use crate::middleware::admin_auth::AdminUser;
use crate::models::maintenance::{KIND_CLEANUP, KIND_STARTUP_CHECK};
use crate::services::maintenance::MaintenanceService;

/// 维护 handler 状态。
#[derive(Clone)]
pub struct AdminMaintenanceState {
    pub maintenance: Arc<MaintenanceService>,
    pub startup_check_mode: StartupCheckMode,
}

/// 手动触发自检的可选参数。
#[derive(Debug, Deserialize, Default)]
pub struct StartupCheckRequest {
    /// 覆盖启动配置的模式（report / fix）。
    pub mode: Option<String>,
}

/// 维护状态总览。
pub async fn status(
    _admin: AdminUser,
    State(state): State<AdminMaintenanceState>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let latest_cleanup = state
        .maintenance
        .latest_run(KIND_CLEANUP)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load status")
        })?;
    let latest_check = state
        .maintenance
        .latest_run(KIND_STARTUP_CHECK)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load status")
        })?;

    let options = state.maintenance.options();
    Ok(Json(serde_json::json!({
        "config": {
            "trash_retention_days": options.trash_retention_days,
            "cleanup_interval_minutes": state.maintenance.interval_minutes(),
            "cleanup_max_objects": options.cleanup_max_objects,
            "cleanup_max_seconds": options.cleanup_max_seconds,
            "startup_check_mode": state.startup_check_mode.as_str(),
            "nonce_retention_seconds": options.nonce_retention_seconds,
        },
        "cleanup": latest_cleanup,
        "startup_check": latest_check,
    })))
}

/// 手动触发一次清理。
pub async fn trigger_cleanup(
    _admin: AdminUser,
    State(state): State<AdminMaintenanceState>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    match state.maintenance.run_cleanup("manual").await {
        Ok(Some(run)) => Ok(Json(serde_json::to_value(run).unwrap_or_default())),
        Ok(None) => Err(error_response(
            StatusCode::CONFLICT,
            "A cleanup run is already in progress",
        )),
        Err(e) => {
            tracing::error!("Cleanup failed: {}", e);
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cleanup failed",
            ))
        }
    }
}

/// 手动触发一次启动一致性自检。
pub async fn trigger_startup_check(
    _admin: AdminUser,
    State(state): State<AdminMaintenanceState>,
    body: Option<Json<StartupCheckRequest>>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let mode = match body
        .and_then(|b| b.0.mode)
        .filter(|m| !m.trim().is_empty())
    {
        Some(raw) => match StartupCheckMode::parse(&raw) {
            Ok(mode) => mode,
            Err(e) => {
                return Err(error_response(StatusCode::BAD_REQUEST, e.to_string()));
            }
        },
        None => state.startup_check_mode,
    };

    match state.maintenance.startup_check(mode, "manual").await {
        Ok(run) => Ok(Json(serde_json::to_value(run).unwrap_or_default())),
        Err(e) => {
            tracing::error!("Startup check failed: {}", e);
            Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Startup check failed",
            ))
        }
    }
}
