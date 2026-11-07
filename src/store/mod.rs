//! 统一存储抽象层。
//!
//! 上层（models / handlers / middleware）只依赖 [`Store`] trait，不感知任何
//! 具体数据库类型。后端由 [`Backend`] 配置选择，默认 `redb`。
//!
//! # 接口契约（各后端必须一致）
//!
//! ## 通用
//! - **App 列表排序**：`created_at DESC`，同一时间戳时 `app_id ASC`。
//! - **Bucket 列表排序**：`created_at DESC`，同一时间戳时 `bucket_id ASC`。
//! - **Bucket 名称全局唯一**：检查与写入必须在同一个原子操作内完成，
//!   冲突返回 [`StoreError::DuplicateBucketName`]；不能依赖“先查后写”。
//! - **nonce 防重放**：`insert_nonce` 原子插入，首次返回 `true`，重复返回
//!   `false`；多实例并发下也必须由后端唯一约束保证。
//! - **schema 版本**：`migrate` 幂等，`schema_version` 返回当前已应用版本。
//!
//! ## 对象状态
//! - 对象只有两种可见状态：**active**（`deleted_at == None`）与
//!   **trashed**（`deleted_at == Some(ts)`）。
//! - `get_object_by_id` / `get_object_by_key` / `list_objects` /
//!   `count_objects_by_*` / `total_size_by_*` **只作用于 active 对象**。
//!   回收站对象必须通过 `list_trash` / `count_trash` / `total_trash_size` 读取。
//! - **Object 列出**：按 `key ASC`；`marker` 是排他游标（从严格大于 marker
//!   的第一个 key 开始）；`prefix` 为前缀过滤；`max_keys` 为上限。
//! - **Object 写入**：`(bucket_id, key)` 在 active 集合中唯一，写入是原子
//!   upsert。若同 key 已有 active 记录且 `object_id` 不同，旧记录会被**移入
//!   回收站**（`deleted_at=now`、记录原桶/原 key），**不删除磁盘文件**；
//!   返回被替换的旧记录。并发写同一 key 时最终只有一条 active 记录。
//! - **软删除**：`soft_delete_object` 只改元数据状态，绝不删除磁盘文件；
//!   记录写 `deleted_at`、`original_bucket_id`、`original_key`。重复软删
//!   保持首次的 `deleted_at` 不变，返回当前记录。
//! - **彻底删除**：`purge_object` / `purge_objects` 才删除元数据，磁盘文件
//!   由调用方删除（清理任务负责）。
//! - **还原**：`restore_object` 语义显式建模为 [`RestoreOutcome`]：
//!   - 目标桶仍存在才可还原，否则 `BucketMissing`；
//!   - 原 key 已被 active 对象占用时，`overwrite=false` 返回 `Conflict`，
//!     `overwrite=true` 则先把占用者移入回收站再还原。
//! - **回收站排序**：`deleted_at DESC`，同一时间戳时 `object_id ASC`；
//!   支持按 `app_id` / `bucket_id` 过滤，分页为 offset/limit。
//! - **级联删除**：
//!   - 删除 Bucket：其下所有 **active** 对象移入回收站，然后删除桶记录；
//!     回收站对象的 `bucket_id` 会成为悬空引用（原桶信息保留在
//!     `original_bucket_id`），此时还原会返回 `BucketMissing`。
//!   - 删除 App：其所有桶的 active 对象移入回收站，再删除桶与 App 记录。
//!   - 两种删除都**不删除任何磁盘文件**，不会留下无主文件。

pub mod memory_store;
pub mod redb_store;

#[cfg(feature = "mysql")]
pub mod mysql_store;

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::config::Config;
use crate::models::app::App;
use crate::models::bucket::Bucket;
use crate::models::maintenance::MaintenanceRun;
use crate::models::object::Object;

/// 当前 schema 版本，所有后端保持一致。
///
/// v2 引入：对象状态（软删除）、回收站、维护任务运行记录。
pub const SCHEMA_VERSION: u32 = 2;

/// 存储句柄，可在 handler / middleware 之间廉价克隆。
pub type StoreHandle = Arc<dyn Store>;

/// 存储后端选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Redb,
    Memory,
    Mysql,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Redb => "redb",
            Self::Memory => "memory",
            Self::Mysql => "mysql",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "redb" => Ok(Self::Redb),
            "memory" | "mem" => Ok(Self::Memory),
            "mysql" => Ok(Self::Mysql),
            other => Err(anyhow!(
                "Unsupported OSS_STORAGE_BACKEND '{}' (expected redb / memory / mysql)",
                other
            )),
        }
    }
}

/// 存储层可识别的业务错误，便于上层做状态码映射。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Bucket name '{0}' already exists")]
    DuplicateBucketName(String),
    #[error("Storage backend '{0}' is unavailable: {1}")]
    BackendUnavailable(&'static str, String),
}

/// 还原操作的结果（显式建模冲突，不允许后端各自夹带假设）。
#[derive(Debug, Clone)]
pub enum RestoreOutcome {
    /// 还原成功，返回 active 状态的对象。
    Restored(Object),
    /// 对象不存在（或已被彻底删除）。
    NotFound,
    /// 原 key 已被另一个 active 对象占用。
    Conflict { existing_object_id: String },
    /// 原桶已被删除，无法还原（只能彻底删除或另存）。
    BucketMissing { bucket_id: String },
}

/// 统一存储接口。
#[async_trait]
pub trait Store: Send + Sync {
    /// 后端名称，用于日志与测试断言。
    fn backend_name(&self) -> &'static str;

    // ---- schema ----
    /// 应用所有迁移（幂等）。旧版本库应在此自动升级。
    async fn migrate(&self) -> Result<()>;
    /// 当前 schema 版本。
    async fn schema_version(&self) -> Result<u32>;

    // ---- App ----
    async fn insert_app(&self, app: &App) -> Result<()>;
    async fn get_app(&self, app_id: &str) -> Result<Option<App>>;
    async fn list_apps(&self, offset: i64, limit: i64) -> Result<Vec<App>>;
    async fn count_apps(&self) -> Result<i64>;
    async fn update_app_secret(&self, app_id: &str, encrypted: &str) -> Result<bool>;
    /// 删除 App：其所有 Bucket 的 active 对象先移入回收站，再删除 Bucket 与
    /// App 记录；不删除磁盘文件。
    async fn delete_app(&self, app_id: &str) -> Result<bool>;

    // ---- Bucket ----
    /// 创建 Bucket；名称冲突必须返回 [`StoreError::DuplicateBucketName`]。
    async fn insert_bucket(&self, bucket: &Bucket) -> Result<()>;
    async fn get_bucket_by_id(&self, bucket_id: &str) -> Result<Option<Bucket>>;
    async fn get_bucket_by_name(&self, name: &str) -> Result<Option<Bucket>>;
    async fn get_bucket_by_app_and_name(
        &self,
        app_id: &str,
        name: &str,
    ) -> Result<Option<Bucket>>;
    async fn list_buckets_by_app(
        &self,
        app_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Bucket>>;
    async fn count_buckets_by_app(&self, app_id: &str) -> Result<i64>;
    async fn set_bucket_public(&self, bucket_id: &str, is_public: bool) -> Result<bool>;
    /// 删除 Bucket：其下 active 对象先移入回收站，再删除桶记录；不删除磁盘文件。
    async fn delete_bucket(&self, bucket_id: &str) -> Result<bool>;

    // ---- Object（active 语义）----
    /// 原子 upsert；同 key 的旧 active 记录会被移入回收站并作为返回值。
    async fn put_object(&self, object: &Object) -> Result<Option<Object>>;
    async fn get_object_by_id(&self, object_id: &str) -> Result<Option<Object>>;
    /// 按 ID 读取对象（**包含回收站**），用于彻底删除前定位磁盘文件。
    async fn get_object_any(&self, object_id: &str) -> Result<Option<Object>>;
    async fn get_object_by_key(&self, bucket_id: &str, key: &str) -> Result<Option<Object>>;
    async fn list_objects(
        &self,
        bucket_id: &str,
        prefix: &str,
        marker: &str,
        max_keys: i64,
    ) -> Result<Vec<Object>>;
    async fn count_objects_by_bucket(&self, bucket_id: &str) -> Result<i64>;
    async fn total_size_by_bucket(&self, bucket_id: &str) -> Result<i64>;
    async fn count_objects_by_app(&self, app_id: &str) -> Result<i64>;
    async fn total_size_by_app(&self, app_id: &str) -> Result<i64>;

    // ---- 回收站 ----
    /// 软删除：移入回收站，保留磁盘文件。返回更新后的记录；对象不存在返回 None。
    async fn soft_delete_object(&self, object_id: &str, deleted_at: i64)
        -> Result<Option<Object>>;
    /// 彻底删除元数据；返回被删除的记录（调用方据此删除磁盘文件）。
    async fn purge_object(&self, object_id: &str) -> Result<Option<Object>>;
    /// 批量彻底删除元数据，返回删除条数（用于自检修复失效记录）。
    async fn purge_objects(&self, object_ids: &[String]) -> Result<u64>;
    /// 还原回收站对象。
    async fn restore_object(
        &self,
        object_id: &str,
        overwrite: bool,
        now: i64,
    ) -> Result<RestoreOutcome>;
    /// 回收站分页列表，排序 `deleted_at DESC, object_id ASC`。
    async fn list_trash(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Object>>;
    async fn count_trash(&self, app_id: Option<&str>, bucket_id: Option<&str>) -> Result<i64>;
    async fn total_trash_size(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64>;
    /// 列出 `deleted_at < before` 的回收站对象，按 `deleted_at ASC` 排序（清理任务用）。
    async fn list_expired_trash(&self, before: i64, limit: i64) -> Result<Vec<Object>>;

    // ---- 维护（启动自检 / 清理任务）----
    /// 遍历所有对象元数据（含回收站），按 `object_id ASC` 分页。
    async fn list_all_objects(&self, offset: i64, limit: i64) -> Result<Vec<Object>>;
    async fn count_all_objects(&self) -> Result<i64>;

    async fn save_maintenance_run(&self, run: &MaintenanceRun) -> Result<()>;
    async fn latest_maintenance_run(&self, kind: &str) -> Result<Option<MaintenanceRun>>;
    async fn list_maintenance_runs(&self, kind: &str, limit: i64) -> Result<Vec<MaintenanceRun>>;

    // ---- nonce / 防重放 ----
    /// 原子插入 nonce；首次成功返回 `true`，重复返回 `false`。
    async fn insert_nonce(&self, app_id: &str, nonce: &str, created_at: i64) -> Result<bool>;
    /// 清理早于 `before` 的 nonce，返回删除条数。
    async fn purge_nonces_before(&self, before: i64) -> Result<u64>;
}

/// 根据配置装配存储后端。
pub async fn init(config: &Config) -> Result<StoreHandle> {
    match config.storage_backend {
        Backend::Redb => redb_store::RedbStore::open(&config.db_url).await,
        Backend::Memory => Ok(memory_store::MemoryStore::create()),
        Backend::Mysql => init_mysql(&config.db_url).await,
    }
}

#[cfg(feature = "mysql")]
async fn init_mysql(db_url: &str) -> Result<StoreHandle> {
    mysql_store::MysqlStore::connect(db_url).await
}

#[cfg(not(feature = "mysql"))]
async fn init_mysql(_db_url: &str) -> Result<StoreHandle> {
    Err(StoreError::BackendUnavailable(
        "mysql",
        "rebuild with `--features mysql`; default build is pure Rust and has no MySQL driver"
            .to_string(),
    )
    .into())
}

/// 测试用内存后端（不写磁盘、不依赖外部数据库）。
pub fn init_test_store() -> StoreHandle {
    memory_store::MemoryStore::create()
}
