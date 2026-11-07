//! App management handlers (admin API).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::store::StoreHandle;
use crate::handlers::{error_response, PaginatedResponse};
use crate::middleware::admin_auth::AdminUser;
use crate::models::app::App;

/// App state for app handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: StoreHandle,
    pub master_key: String,
}

/// Request body for creating an app.
#[derive(Deserialize)]
pub struct CreateAppRequest {
    pub name: String,
}

/// Response body for app info.
#[derive(Serialize)]
pub struct AppResponse {
    pub app_id: String,
    pub name: String,
    pub created_at: i64,
}

/// Response body for app creation (includes secret).
#[derive(Serialize)]
pub struct CreateAppResponse {
    pub app_id: String,
    pub app_secret: String,
    pub name: String,
    pub created_at: i64,
}

/// Response body for secret reset.
#[derive(Serialize)]
pub struct ResetSecretResponse {
    pub app_secret: String,
}

/// Query params for listing apps.
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

/// Create a new app (admin only).
pub async fn create_app(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(body): Json<CreateAppRequest>,
) -> Result<(StatusCode, Json<CreateAppResponse>), axum::response::Response> {
    if body.name.trim().is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "App name cannot be empty",
        ));
    }

    if body.name.len() > 100 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "App name too long (max 100 characters)",
        ));
    }

    let (app, secret) = App::create(&state.pool, &body.name, &state.master_key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create app: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to create app")
        })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateAppResponse {
            app_id: app.app_id,
            app_secret: secret,
            name: app.name,
            created_at: app.created_at,
        }),
    ))
}

/// List apps (admin only).
pub async fn list_apps(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<PaginatedResponse<AppResponse>>, axum::response::Response> {
    let limit = query.limit.clamp(1, 100);
    let offset = query.offset.max(0);

    let apps = App::list(&state.pool, offset, limit)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list apps: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list apps")
        })?;

    let total = App::count(&state.pool).await.map_err(|e| {
        tracing::error!("Failed to count apps: {}", e);
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to count apps")
    })?;

    let items: Vec<AppResponse> = apps
        .into_iter()
        .map(|a| AppResponse {
            app_id: a.app_id,
            name: a.name,
            created_at: a.created_at,
        })
        .collect();

    Ok(Json(PaginatedResponse {
        items,
        total,
        offset,
        limit,
    }))
}

/// Get app by ID (admin only).
pub async fn get_app(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> Result<Json<AppResponse>, axum::response::Response> {
    let app = App::get_by_id(&state.pool, &app_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get app: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get app")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "App not found"))?;

    Ok(Json(AppResponse {
        app_id: app.app_id,
        name: app.name,
        created_at: app.created_at,
    }))
}

/// Delete an app (admin only).
pub async fn delete_app(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> Result<StatusCode, axum::response::Response> {
    let deleted = App::delete(&state.pool, &app_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to delete app: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete app")
        })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(error_response(StatusCode::NOT_FOUND, "App not found"))
    }
}

/// Reset app secret (admin only).
pub async fn reset_secret(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> Result<Json<ResetSecretResponse>, axum::response::Response> {
    let new_secret = App::reset_secret(&state.pool, &app_id, &state.master_key)
        .await
        .map_err(|e| {
            tracing::error!("Failed to reset secret: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset secret")
        })?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "App not found"))?;

    Ok(Json(ResetSecretResponse {
        app_secret: new_secret,
    }))
}
