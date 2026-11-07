//! Configuration loading from environment variables.

use std::env;

use anyhow::{anyhow, Result};

use crate::store::Backend;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Host and port to bind.
    pub host: String,
    /// Directory for storing file data.
    pub data_dir: String,
    /// Storage backend: redb / memory / mysql.
    pub storage_backend: Backend,
    /// redb 文件路径，或 `mysql://user:pass@host:port/db` 连接串。
    pub db_url: String,
    /// Admin token for management API.
    pub admin_token: String,
    /// Master key for encrypting app secrets at rest.
    pub master_key: String,
    /// Public base URL for generating signed URLs.
    pub public_base_url: String,
    /// Signature URL expiration in seconds.
    pub signature_expire_seconds: u64,
    /// Maximum upload size in bytes.
    pub max_upload_size: u64,
    /// LRU cache size in bytes (0 disables cache).
    pub cache_size_bytes: usize,
    /// CORS allowed origins (comma-separated).
    pub cors_allowed_origins: Vec<String>,
    /// Whether CORS is enabled.
    pub cors_enabled: bool,
    /// 回收站保留天数（到期后由清理任务彻底删除），<=0 表示不自动清理。
    pub trash_retention_days: i64,
    /// 清理任务周期（分钟），0 表示关闭定时任务（仍可手动触发）。
    pub cleanup_interval_minutes: u64,
    /// 单次清理最多处理的条目数（防止一次清理拖死服务）。
    pub cleanup_max_objects: i64,
    /// 单次清理的时间上限（秒）。
    pub cleanup_max_seconds: u64,
    /// 启动一致性自检模式：report（只报告）/ fix（自动修复）。
    pub startup_check_mode: StartupCheckMode,
    /// nonce 保留秒数，超过后由清理任务删除。
    pub nonce_retention_seconds: i64,
}

/// 启动自检模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupCheckMode {
    /// 只报告，不修改任何数据。
    Report,
    /// 自动修复：删除失效记录与孤儿文件。
    Fix,
}

impl StartupCheckMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Fix => "fix",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "report" => Ok(Self::Report),
            "fix" => Ok(Self::Fix),
            other => Err(anyhow!(
                "Unsupported OSS_STARTUP_CHECK_MODE '{}' (expected report / fix)",
                other
            )),
        }
    }
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let host = env::var("OSS_HOST").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
        let data_dir = env::var("OSS_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
        let db_url = env::var("OSS_DB_URL").unwrap_or_else(|_| "redb://./oss.db".to_string());
        // OSS_STORAGE_BACKEND 优先；未设置时根据 URL 推断（mysql:// -> mysql，其余 -> redb）。
        let storage_backend = match env::var("OSS_STORAGE_BACKEND") {
            Ok(value) if !value.trim().is_empty() => Backend::parse(&value)?,
            _ if db_url.starts_with("mysql://") => Backend::Mysql,
            _ => Backend::Redb,
        };
        let admin_token = env::var("OSS_ADMIN_TOKEN")
            .map_err(|_| anyhow!("OSS_ADMIN_TOKEN environment variable is required"))?;
        let master_key = env::var("OSS_MASTER_KEY")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                // Fallback: derive from admin token if not explicitly set
                admin_token.clone() + "-master-key-salt"
            });
        let public_base_url =
            env::var("OSS_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let signature_expire_seconds = env::var("OSS_SIGNATURE_EXPIRE_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(900);
        let max_upload_size = env::var("OSS_MAX_UPLOAD_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10 * 1024 * 1024 * 1024); // 10 GB
        let cache_size_bytes = env::var("OSS_CACHE_SIZE_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 * 1024 * 1024); // 64 MB
        let cors_allowed_origins = env::var("OSS_CORS_ALLOWED_ORIGINS")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        let cors_enabled = env::var("OSS_CORS_ENABLED")
            .ok()
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);
        let trash_retention_days = env::var("OSS_TRASH_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let cleanup_interval_minutes = env::var("OSS_CLEANUP_INTERVAL_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let cleanup_max_objects = env::var("OSS_CLEANUP_MAX_OBJECTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let cleanup_max_seconds = env::var("OSS_CLEANUP_MAX_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let startup_check_mode = match env::var("OSS_STARTUP_CHECK_MODE") {
            Ok(value) if !value.trim().is_empty() => StartupCheckMode::parse(&value)?,
            _ => StartupCheckMode::Report,
        };
        let nonce_retention_seconds = env::var("OSS_NONCE_RETENTION_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);

        Ok(Self {
            host,
            data_dir,
            storage_backend,
            db_url,
            admin_token,
            master_key,
            public_base_url,
            signature_expire_seconds,
            max_upload_size,
            cache_size_bytes,
            cors_allowed_origins,
            cors_enabled,
            trash_retention_days,
            cleanup_interval_minutes,
            cleanup_max_objects,
            cleanup_max_seconds,
            startup_check_mode,
            nonce_retention_seconds,
        })
    }
}
