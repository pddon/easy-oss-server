//! OSS Server - 轻量级分布式对象存储服务器
//!
//! 基于 Axum 的对象存储服务，元数据通过统一的 `Store` 抽象层持久化
//! （默认 redb，可配置为 memory / MySQL），文件内容落本地磁盘。
//! 支持 HMAC-SHA256 签名鉴权，兼容 OSS/S3 风格 API。
//!
//! 以库形式暴露，便于集成测试直接复用路由、模型与服务。

// 处理器统一使用 `Result<_, Response>` 返回错误，Response 体积较大；
// 这是 axum 中常见的错误通道写法，真实错误已在 `error_response` 中收敛。
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, FromRef, State},
    routing::{delete, get, post, put},
    Json, Router,
};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

pub mod config;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod s3;
pub mod services;
pub mod store;
pub mod web;

use config::Config;
use services::maintenance::MaintenanceService;
use services::storage::StorageService;
use store::StoreHandle;

/// Application state shared across all handlers.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: StoreHandle,
    pub storage: StorageService,
}

// Unified state for admin API routes
#[derive(Clone)]
struct AdminApiState {
    app: handlers::app::AppState,
    bucket: handlers::admin_bucket::AdminBucketState,
    object: handlers::admin_object::AdminObjectState,
    trash: handlers::admin_trash::AdminTrashState,
    maintenance: handlers::admin_maintenance::AdminMaintenanceState,
}

impl FromRef<AdminApiState> for handlers::app::AppState {
    fn from_ref(state: &AdminApiState) -> Self {
        state.app.clone()
    }
}

impl FromRef<AdminApiState> for handlers::admin_bucket::AdminBucketState {
    fn from_ref(state: &AdminApiState) -> Self {
        state.bucket.clone()
    }
}

impl FromRef<AdminApiState> for handlers::admin_object::AdminObjectState {
    fn from_ref(state: &AdminApiState) -> Self {
        state.object.clone()
    }
}

impl FromRef<AdminApiState> for handlers::admin_trash::AdminTrashState {
    fn from_ref(state: &AdminApiState) -> Self {
        state.trash.clone()
    }
}

impl FromRef<AdminApiState> for handlers::admin_maintenance::AdminMaintenanceState {
    fn from_ref(state: &AdminApiState) -> Self {
        state.maintenance.clone()
    }
}

// Unified state for OSS API routes
#[derive(Clone)]
struct OssApiState {
    object: handlers::object::ObjectState,
    bucket: handlers::bucket::BucketState,
    s3: s3::S3State,
}

impl FromRef<OssApiState> for handlers::object::ObjectState {
    fn from_ref(state: &OssApiState) -> Self {
        state.object.clone()
    }
}

impl FromRef<OssApiState> for handlers::bucket::BucketState {
    fn from_ref(state: &OssApiState) -> Self {
        state.bucket.clone()
    }
}

impl FromRef<OssApiState> for s3::S3State {
    fn from_ref(state: &OssApiState) -> Self {
        state.s3.clone()
    }
}

/// Build the complete router with all routes and middleware.
///
/// 内部会创建一份 [`MaintenanceService`]（供管理端手动触发使用）。需要与
/// 启动自检 / 定时任务共享同一把清理锁时，请使用
/// [`build_router_with_maintenance`]。
pub fn build_router(config: Arc<Config>, pool: StoreHandle, storage: StorageService) -> Router {
    let maintenance = MaintenanceService::new(pool.clone(), storage.clone(), &config);
    build_router_with_maintenance(config, pool, storage, maintenance)
}

/// Build the router with a caller-supplied maintenance service.
pub fn build_router_with_maintenance(
    config: Arc<Config>,
    pool: StoreHandle,
    storage: StorageService,
    maintenance: Arc<MaintenanceService>,
) -> Router {
    let state = AppState {
        config: config.clone(),
        pool: pool.clone(),
        storage: storage.clone(),
    };

    // CORS layer
    let cors = if config.cors_enabled {
        CorsLayer::permissive()
    } else {
        CorsLayer::new()
    };

    // Base router with global middleware
    let router = Router::new()
        .route("/healthz", get(health_check))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(config.max_upload_size as usize))
        .with_state(state);

    // Nest admin API
    let admin_api = build_admin_api(&config, &pool, &storage, &maintenance);
    let router = router.nest("/api/admin", admin_api);

    // OSS API (the main object storage API). Its paths are already absolute,
    // so merge instead of nesting at the root (axum rejects nesting at "/").
    let oss_api = build_oss_api(&config, &pool, &storage);
    let router = router.merge(oss_api);

    // Admin web UI (mounted at root so `/admin/`, `/admin`, and static assets
    // all resolve correctly; a nested router cannot match `/admin/` reliably).
    router.merge(build_admin_ui())
}

/// 健康检查：返回后端类型与 schema 版本（Docker healthcheck / 部署脚本使用）。
async fn health_check(State(state): State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.pool.schema_version().await {
        Ok(version) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "backend": state.pool.backend_name(),
                "schema_version": version,
                "time": chrono::Utc::now().timestamp(),
            })),
        )
            .into_response(),
        Err(err) => {
            tracing::error!("Health check failed: {}", err);
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "unavailable",
                    "backend": state.pool.backend_name(),
                    "error": err.to_string(),
                })),
            )
                .into_response()
        }
    }
}

/// Build admin API routes (protected by admin token).
fn build_admin_api(
    config: &Config,
    pool: &StoreHandle,
    storage: &StorageService,
    maintenance: &Arc<MaintenanceService>,
) -> Router {
    use middleware::admin_auth::{admin_auth_middleware, AdminAuthState};

    let admin_auth_state = AdminAuthState {
        admin_token: config.admin_token.clone(),
    };

    let state = AdminApiState {
        app: handlers::app::AppState {
            pool: pool.clone(),
            master_key: config.master_key.clone(),
        },
        bucket: handlers::admin_bucket::AdminBucketState {
            pool: pool.clone(),
        },
        object: handlers::admin_object::AdminObjectState {
            pool: pool.clone(),
            storage: storage.clone(),
            max_upload_size: config.max_upload_size,
        },
        trash: handlers::admin_trash::AdminTrashState {
            pool: pool.clone(),
            storage: storage.clone(),
            maintenance: maintenance.clone(),
        },
        maintenance: handlers::admin_maintenance::AdminMaintenanceState {
            maintenance: maintenance.clone(),
            startup_check_mode: config.startup_check_mode,
        },
    };

    // App management
    let apps_router = Router::new()
        .route("/apps", get(handlers::app::list_apps).post(handlers::app::create_app))
        .route(
            "/apps/:app_id",
            get(handlers::app::get_app).delete(handlers::app::delete_app),
        )
        .route(
            "/apps/:app_id/reset-secret",
            axum::routing::post(handlers::app::reset_secret),
        );

    // Admin bucket management
    let admin_buckets_router = Router::new()
        .route(
            "/apps/:app_id/buckets",
            get(handlers::admin_bucket::admin_list_buckets)
                .post(handlers::admin_bucket::admin_create_bucket),
        )
        .route(
            "/apps/:app_id/buckets/:bucket_name",
            put(handlers::admin_bucket::admin_update_bucket)
                .delete(handlers::admin_bucket::admin_delete_bucket),
        );

    // Admin object management
    let admin_objects_router = Router::new()
        .route(
            "/buckets/:bucket_name/objects",
            get(handlers::admin_object::admin_list_objects),
        )
        .route(
            "/buckets/:bucket_name/objects/*key",
            put(handlers::admin_object::admin_put_object)
                .get(handlers::admin_object::admin_get_object)
                .delete(handlers::admin_object::admin_delete_object),
        );

    // 回收站 + 维护（自检 / 清理）
    let trash_router = Router::new()
        .route("/trash", get(handlers::admin_trash::list_trash))
        .route("/trash/batch", post(handlers::admin_trash::batch))
        .route(
            "/trash/:object_id/restore",
            post(handlers::admin_trash::restore_object),
        )
        .route(
            "/trash/:object_id",
            delete(handlers::admin_trash::purge_object),
        );

    let maintenance_router = Router::new()
        .route(
            "/maintenance/status",
            get(handlers::admin_maintenance::status),
        )
        .route(
            "/maintenance/cleanup",
            post(handlers::admin_maintenance::trigger_cleanup),
        )
        .route(
            "/maintenance/startup-check",
            post(handlers::admin_maintenance::trigger_startup_check),
        );

    // Combine all admin routes with unified state
    Router::new()
        .merge(apps_router)
        .merge(admin_buckets_router)
        .merge(admin_objects_router)
        .merge(trash_router)
        .merge(maintenance_router)
        .with_state(state)
        .route_layer(axum::middleware::from_fn_with_state(
            admin_auth_state,
            admin_auth_middleware,
        ))
}

/// Build the main OSS API routes (with optional HMAC signature auth).
fn build_oss_api(
    config: &Config,
    pool: &StoreHandle,
    storage: &StorageService,
) -> Router {
    use middleware::auth::{oss_auth_middleware, AuthState};

    let auth_state = AuthState {
        pool: pool.clone(),
        master_key: config.master_key.clone(),
    };

    let state = OssApiState {
        object: handlers::object::ObjectState {
            pool: pool.clone(),
            storage: storage.clone(),
            max_upload_size: config.max_upload_size,
        },
        bucket: handlers::bucket::BucketState {
            pool: pool.clone(),
        },
        s3: s3::S3State {
            pool: pool.clone(),
            storage: storage.clone(),
            max_upload_size: config.max_upload_size,
        },
    };

    // Auth middleware runs on every OSS route, but credentials are optional:
    // - public GET and signed-URL GET pass through with no credentials;
    // - handlers that require an app reject requests without `AuthenticatedApp`;
    // - a present-but-invalid signature is rejected.
    Router::new()
        .route("/", get(s3::root))
        .route(
            "/buckets",
            get(handlers::bucket::list_buckets).post(handlers::bucket::create_bucket),
        )
        .route(
            "/buckets/:bucket_name",
            get(handlers::bucket::get_bucket)
                .put(handlers::bucket::update_bucket)
                .delete(handlers::bucket::delete_bucket),
        )
        .route(
            "/:bucket",
            get(handlers::object::list_objects)
                .put(s3::bucket::create_bucket)
                .delete(s3::bucket::delete_bucket)
                .head(s3::bucket::head_bucket)
                .post(s3::bucket::post_bucket),
        )
        .route(
            "/:bucket/*key",
            get(handlers::object::get_object)
                .put(handlers::object::put_object)
                .delete(handlers::object::delete_object)
                .head(s3::object::head_object)
                .post(s3::object::post_object),
        )
        .with_state(state)
        .route_layer(axum::middleware::from_fn_with_state(
            auth_state,
            oss_auth_middleware,
        ))
}

/// Build admin web UI routes (static files).
fn build_admin_ui() -> Router {
    Router::new()
        .route("/admin", get(web::admin_handler::redirect_to_admin))
        .route("/admin/", get(web::admin_handler::serve_admin))
        .route("/admin/*path", get(web::admin_handler::serve_admin))
}
