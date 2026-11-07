//! OSS Server entry point.
//!
//! The router and reusable components live in the `oss_server` library crate;
//! this binary only handles configuration, startup, maintenance scheduling and
//! serving.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use oss_server::build_router_with_maintenance;
use oss_server::config::Config;
use oss_server::services::maintenance::MaintenanceService;
use oss_server::services::storage::StorageService;
use oss_server::store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present
    let _ = dotenvy::dotenv();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oss_server=info,tower_http=info".into()),
        )
        .with_target(false)
        .init();

    // Load configuration
    let config = Arc::new(Config::from_env()?);
    tracing::info!("Starting OSS Server on {}", config.host);

    // Initialize metadata store according to OSS_STORAGE_BACKEND
    let pool = store::init(&config).await?;
    tracing::info!(
        "Metadata store initialized (backend = {}, schema = {})",
        pool.backend_name(),
        pool.schema_version().await.unwrap_or_default()
    );

    // Initialize storage
    let storage = StorageService::new(&config.data_dir);
    storage.init().await?;
    tracing::info!("Storage initialized at {}", config.data_dir);

    // 维护服务：启动自检与后续定时/手动清理共享同一实例（同一把清理锁）。
    let maintenance = MaintenanceService::new(pool.clone(), storage.clone(), &config);

    // 启动一致性自检（report 只报告，fix 自动修复）。
    match maintenance
        .startup_check(config.startup_check_mode, "startup")
        .await
    {
        Ok(run) => tracing::info!(
            mode = run.trigger,
            status = run.status,
            invalid_records = run.invalid_records,
            orphan_files = run.orphan_files,
            orphan_bytes = run.orphan_bytes,
            "startup consistency check recorded"
        ),
        Err(err) => tracing::error!("Startup consistency check failed: {}", err),
    }

    // 定时清理任务（OSS_CLEANUP_INTERVAL_MINUTES=0 时关闭）。
    if config.cleanup_interval_minutes > 0 {
        let scheduler = maintenance.clone();
        let interval = Duration::from_secs(config.cleanup_interval_minutes * 60);
        tracing::info!(
            "Cleanup task scheduled every {} minute(s)",
            config.cleanup_interval_minutes
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match scheduler.run_cleanup("scheduled").await {
                    Ok(Some(run)) => tracing::info!(
                        status = run.status,
                        purged_objects = run.purged_objects,
                        freed_bytes = run.freed_bytes,
                        purged_nonces = run.purged_nonces,
                        "scheduled cleanup finished"
                    ),
                    Ok(None) => tracing::info!("scheduled cleanup skipped: already running"),
                    Err(err) => tracing::error!("scheduled cleanup failed: {}", err),
                }
            }
        });
    }

    // Build the application
    let app = build_router_with_maintenance(config.clone(), pool.clone(), storage.clone(), maintenance);

    // Start server
    let addr: SocketAddr = config.host.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Server listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
