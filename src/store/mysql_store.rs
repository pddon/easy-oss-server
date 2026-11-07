//! MySQL 存储后端（可选 feature `mysql`，纯 Rust sqlx 驱动）。
//!
//! 启动时自动完成：
//! 1. 连接 DSN 中的服务器（不带库名），`CREATE DATABASE IF NOT EXISTS`；
//! 2. 连接目标库并执行 `migrations/001_init.sql`；
//! 3. 若检测到旧版（v1）库，逐条执行 `migrations/002_upgrade_v2.sql`
//!    （重复定义类错误被忽略，因此对全新库同样安全）；
//! 4. 写入 `_migrations` 版本记录。
//!
//! 并发语义：
//! - `insert_bucket` 依赖 `buckets.name` 的唯一索引；
//! - `put_object` 在事务内先把旧 active 记录移入回收站再 upsert，配合唯一
//!   索引 + 进程内按 key 串行化 + 有限重试兜底；
//! - `restore_object` 在事务内 `SELECT ... FOR UPDATE` 检查 key 占用；
//! - `insert_nonce` 依赖 `(app_id, nonce)` 主键。

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::Row;
use tokio::sync::Mutex as AsyncMutex;

use crate::models::app::App;
use crate::models::bucket::Bucket;
use crate::models::maintenance::MaintenanceRun;
use crate::models::object::Object;
use crate::store::{RestoreOutcome, Store, StoreError, StoreHandle, SCHEMA_VERSION};

const MIGRATION_V1: &str = "schema_v1_mysql";
const MIGRATION_V2: &str = "schema_v2_mysql";
const DDL: &str = include_str!("../../migrations/001_init.sql");
const UPGRADE_V2: &str = include_str!("../../migrations/002_upgrade_v2.sql");

/// 所有 SELECT 共用的对象列清单。
const OBJECT_COLUMNS: &str = "object_id, app_id, bucket_id, `key`, size, content_type, \
     etag, created_at, deleted_at, original_bucket_id, original_key";

/// MySQL 后端。
pub struct MysqlStore {
    pool: MySqlPool,
    /// 进程内按 `(bucket_id, key)` 串行化同一 key 的并发写，减少死锁。
    /// 跨进程仍依赖唯一索引 + 有限重试。
    key_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl MysqlStore {
    /// 连接 `mysql://user:pass@host:port/db`，按需建库、建表、升级。
    pub async fn connect(dsn: &str) -> Result<StoreHandle> {
        let parsed = url::Url::parse(dsn).context("Invalid MySQL URL")?;
        if parsed.scheme() != "mysql" {
            return Err(anyhow!(
                "MySQL DSN must start with mysql://, got '{}'",
                parsed.scheme()
            ));
        }
        let database = parsed.path().trim_start_matches('/').to_string();
        if database.is_empty() {
            return Err(anyhow!(
                "MySQL DSN must include a database name, e.g. mysql://user:pass@host:3306/oss_server"
            ));
        }

        // 1. 先连服务器（无库名）创建数据库。
        let mut server_url = parsed.clone();
        server_url.set_path("/");
        let server_pool = MySqlPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(10))
            .connect(server_url.as_str())
            .await
            .context("Failed to connect to MySQL server")?;
        let create_db = format!(
            "CREATE DATABASE IF NOT EXISTS `{}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
            database.replace('`', "``")
        );
        sqlx::query(&create_db)
            .execute(&server_pool)
            .await
            .with_context(|| format!("Failed to create database '{}'", database))?;
        server_pool.close().await;

        // 2. 连接目标库。
        let pool = MySqlPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(10))
            .connect(dsn)
            .await
            .with_context(|| format!("Failed to connect to MySQL database '{}'", database))?;

        let store = Self {
            pool,
            key_locks: StdMutex::new(HashMap::new()),
        };
        store.migrate().await?;
        Ok(Arc::new(store))
    }

    fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    async fn has_migration(&self, name: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _migrations WHERE name = ?")
            .bind(name)
            .fetch_one(self.pool())
            .await?;
        Ok(count > 0)
    }

    async fn record_migration(&self, name: &str) -> Result<()> {
        sqlx::query("INSERT IGNORE INTO _migrations (name, applied_at) VALUES (?, ?)")
            .bind(name)
            .bind(chrono::Utc::now().timestamp())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// 逐条执行 v1 -> v2 升级语句。列/索引/外键已存在或被删除时忽略。
    async fn apply_upgrade_v2(&self) -> Result<()> {
        // 先剔除整行注释再按 ';' 拆分：注释里可能包含分号，先拆分会产生
        // 半截注释被当成 SQL 执行。
        let without_comments: String = UPGRADE_V2
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for statement in without_comments.split(';') {
            let cleaned = statement.trim();
            if cleaned.is_empty() {
                continue;
            }
            if let Err(err) = sqlx::raw_sql(cleaned).execute(self.pool()).await {
                if is_ignorable_ddl_error(&err) {
                    continue;
                }
                return Err(err).with_context(|| {
                    format!("Failed to apply v2 upgrade statement: {}", cleaned)
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Store for MysqlStore {
    fn backend_name(&self) -> &'static str {
        "mysql"
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::raw_sql(DDL)
            .execute(self.pool())
            .await
            .context("Failed to apply MySQL DDL")?;
        if !self.has_migration(MIGRATION_V2).await? {
            self.apply_upgrade_v2().await?;
            self.record_migration(MIGRATION_V2).await?;
        }
        self.record_migration(MIGRATION_V1).await?;
        Ok(())
    }

    async fn schema_version(&self) -> Result<u32> {
        if self.has_migration(MIGRATION_V2).await? {
            Ok(SCHEMA_VERSION)
        } else if self.has_migration(MIGRATION_V1).await? {
            Ok(1)
        } else {
            Ok(0)
        }
    }

    async fn insert_app(&self, app: &App) -> Result<()> {
        sqlx::query(
            "INSERT INTO apps (app_id, app_secret_encrypted, name, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&app.app_id)
        .bind(&app.app_secret_encrypted)
        .bind(&app.name)
        .bind(app.created_at)
        .execute(self.pool())
        .await
        .context("Failed to insert app")?;
        Ok(())
    }

    async fn get_app(&self, app_id: &str) -> Result<Option<App>> {
        let row = sqlx::query(
            "SELECT app_id, app_secret_encrypted, name, created_at FROM apps WHERE app_id = ?",
        )
        .bind(app_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_app).transpose()
    }

    async fn list_apps(&self, offset: i64, limit: i64) -> Result<Vec<App>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT app_id, app_secret_encrypted, name, created_at FROM apps \
             ORDER BY created_at DESC, app_id ASC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(row_to_app).collect()
    }

    async fn count_apps(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM apps")
            .fetch_one(self.pool())
            .await?)
    }

    async fn update_app_secret(&self, app_id: &str, encrypted: &str) -> Result<bool> {
        let result = sqlx::query("UPDATE apps SET app_secret_encrypted = ? WHERE app_id = ?")
            .bind(encrypted)
            .bind(app_id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_app(&self, app_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "UPDATE objects SET deleted_at = ?, original_bucket_id = bucket_id, \
             original_key = `key` WHERE app_id = ? AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM buckets WHERE app_id = ?")
            .bind(app_id)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM apps WHERE app_id = ?")
            .bind(app_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    async fn insert_bucket(&self, bucket: &Bucket) -> Result<()> {
        let result = sqlx::query(
            "INSERT INTO buckets (bucket_id, app_id, name, is_public, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&bucket.bucket_id)
        .bind(&bucket.app_id)
        .bind(&bucket.name)
        .bind(i64::from(bucket.is_public))
        .bind(bucket.created_at)
        .execute(self.pool())
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(StoreError::DuplicateBucketName(bucket.name.clone()).into())
            }
            Err(e) => Err(e).context("Failed to insert bucket"),
        }
    }

    async fn get_bucket_by_id(&self, bucket_id: &str) -> Result<Option<Bucket>> {
        let row = sqlx::query(
            "SELECT bucket_id, app_id, name, is_public, created_at FROM buckets WHERE bucket_id = ?",
        )
        .bind(bucket_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_bucket).transpose()
    }

    async fn get_bucket_by_name(&self, name: &str) -> Result<Option<Bucket>> {
        let row = sqlx::query(
            "SELECT bucket_id, app_id, name, is_public, created_at FROM buckets WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_bucket).transpose()
    }

    async fn get_bucket_by_app_and_name(
        &self,
        app_id: &str,
        name: &str,
    ) -> Result<Option<Bucket>> {
        let row = sqlx::query(
            "SELECT bucket_id, app_id, name, is_public, created_at FROM buckets \
             WHERE app_id = ? AND name = ?",
        )
        .bind(app_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_bucket).transpose()
    }

    async fn list_buckets_by_app(
        &self,
        app_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Bucket>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT bucket_id, app_id, name, is_public, created_at FROM buckets \
             WHERE app_id = ? ORDER BY created_at DESC, bucket_id ASC LIMIT ? OFFSET ?",
        )
        .bind(app_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(row_to_bucket).collect()
    }

    async fn count_buckets_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM buckets WHERE app_id = ?")
                .bind(app_id)
                .fetch_one(self.pool())
                .await?,
        )
    }

    async fn set_bucket_public(&self, bucket_id: &str, is_public: bool) -> Result<bool> {
        let result = sqlx::query("UPDATE buckets SET is_public = ? WHERE bucket_id = ?")
            .bind(i64::from(is_public))
            .bind(bucket_id)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_bucket(&self, bucket_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "UPDATE objects SET deleted_at = ?, original_bucket_id = bucket_id, \
             original_key = `key` WHERE bucket_id = ? AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(bucket_id)
        .execute(&mut *tx)
        .await?;
        let result = sqlx::query("DELETE FROM buckets WHERE bucket_id = ?")
            .bind(bucket_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    async fn put_object(&self, object: &Object) -> Result<Option<Object>> {
        // 进程内按 bucket 串行化对象写入：同一 bucket 的 SELECT ... FOR UPDATE
        // 会在 (bucket_id, active_key) 索引上取间隙锁，不同 key 的并发插入也
        // 可能互相死锁；串行化后进程内不会死锁，跨进程竞争仍由有限重试兜底。
        let lock_key = object.bucket_id.clone();
        let lock = {
            let mut locks = self
                .key_locks
                .lock()
                .expect("mysql key lock map poisoned");
            locks
                .entry(lock_key.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;

        const MAX_RETRIES: usize = 20;
        let mut attempt = 0usize;
        let result = loop {
            match self.put_object_attempt(object).await {
                Ok(previous) => break Ok(previous),
                Err(err) if attempt < MAX_RETRIES && is_retryable(&err) => {
                    attempt += 1;
                    let backoff = (10u64 * (attempt as u64 + 1)).min(250);
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(err) => break Err(err),
            }
        };
        drop(guard);

        // 没有其他等待者时清理锁表项，避免长期运行内存增长。
        let mut locks = self
            .key_locks
            .lock()
            .expect("mysql key lock map poisoned");
        if Arc::strong_count(&lock) == 2 {
            locks.remove(&lock_key);
        }
        result
    }

    async fn get_object_by_id(&self, object_id: &str) -> Result<Option<Object>> {
        let sql = format!(
            "SELECT {} FROM objects WHERE object_id = ? AND deleted_at IS NULL",
            OBJECT_COLUMNS
        );
        let row = sqlx::query(&sql)
            .bind(object_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_object).transpose()
    }

    async fn get_object_any(&self, object_id: &str) -> Result<Option<Object>> {
        let sql = format!("SELECT {} FROM objects WHERE object_id = ?", OBJECT_COLUMNS);
        let row = sqlx::query(&sql)
            .bind(object_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_object).transpose()
    }

    async fn get_object_by_key(&self, bucket_id: &str, key: &str) -> Result<Option<Object>> {
        let sql = format!(
            "SELECT {} FROM objects WHERE bucket_id = ? AND active_key = ?",
            OBJECT_COLUMNS
        );
        let row = sqlx::query(&sql)
            .bind(bucket_id)
            .bind(key)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_object).transpose()
    }

    async fn list_objects(
        &self,
        bucket_id: &str,
        prefix: &str,
        marker: &str,
        max_keys: i64,
    ) -> Result<Vec<Object>> {
        if max_keys <= 0 {
            return Ok(Vec::new());
        }

        // 用半开区间代替 LIKE，避免 `_` / `%` 通配符转义问题。
        // 对象 key 仅允许 ASCII，`\x7f` 是安全上界。
        let prefix_upper = if prefix.is_empty() {
            None
        } else {
            Some(format!("{}\x7f", prefix))
        };
        let marker_filter = if marker.is_empty() {
            None
        } else {
            Some(marker.to_string())
        };

        let mut sql = format!(
            "SELECT {} FROM objects WHERE bucket_id = ? AND deleted_at IS NULL",
            OBJECT_COLUMNS
        );
        if prefix_upper.is_some() {
            sql.push_str(" AND `key` >= ? AND `key` < ?");
        }
        if marker_filter.is_some() {
            sql.push_str(" AND `key` > ?");
        }
        sql.push_str(" ORDER BY `key` ASC LIMIT ?");

        let mut query = sqlx::query(&sql).bind(bucket_id);
        if let Some(upper) = &prefix_upper {
            query = query.bind(prefix).bind(upper);
        }
        if let Some(marker) = &marker_filter {
            query = query.bind(marker);
        }
        query = query.bind(max_keys);

        let rows = query.fetch_all(self.pool()).await?;
        rows.iter().map(row_to_object).collect()
    }

    async fn count_objects_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM objects WHERE bucket_id = ? AND deleted_at IS NULL",
        )
        .bind(bucket_id)
        .fetch_one(self.pool())
        .await?)
    }

    async fn total_size_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(size), 0) AS SIGNED) FROM objects \
             WHERE bucket_id = ? AND deleted_at IS NULL",
        )
        .bind(bucket_id)
        .fetch_one(self.pool())
        .await?)
    }

    async fn count_objects_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM objects WHERE app_id = ? AND deleted_at IS NULL",
        )
        .bind(app_id)
        .fetch_one(self.pool())
        .await?)
    }

    async fn total_size_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(size), 0) AS SIGNED) FROM objects \
             WHERE app_id = ? AND deleted_at IS NULL",
        )
        .bind(app_id)
        .fetch_one(self.pool())
        .await?)
    }

    async fn soft_delete_object(&self, object_id: &str, deleted_at: i64) -> Result<Option<Object>> {
        let mut tx = self.pool.begin().await?;
        let select_sql = format!(
            "SELECT {} FROM objects WHERE object_id = ? FOR UPDATE",
            OBJECT_COLUMNS
        );
        let row = sqlx::query(&select_sql)
            .bind(object_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut obj = row_to_object(&row)?;
        if obj.deleted_at.is_none() {
            sqlx::query(
                "UPDATE objects SET deleted_at = ?, original_bucket_id = bucket_id, \
                 original_key = `key` WHERE object_id = ?",
            )
            .bind(deleted_at)
            .bind(object_id)
            .execute(&mut *tx)
            .await?;
            obj.deleted_at = Some(deleted_at);
            obj.original_bucket_id = Some(obj.bucket_id.clone());
            obj.original_key = Some(obj.key.clone());
        }
        tx.commit().await?;
        Ok(Some(obj))
    }

    async fn purge_object(&self, object_id: &str) -> Result<Option<Object>> {
        let mut tx = self.pool.begin().await?;
        let select_sql = format!(
            "SELECT {} FROM objects WHERE object_id = ? FOR UPDATE",
            OBJECT_COLUMNS
        );
        let row = sqlx::query(&select_sql)
            .bind(object_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let obj = row_to_object(&row)?;
        sqlx::query("DELETE FROM objects WHERE object_id = ?")
            .bind(object_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(obj))
    }

    async fn purge_objects(&self, object_ids: &[String]) -> Result<u64> {
        if object_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", object_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM objects WHERE object_id IN ({})", placeholders);
        let mut query = sqlx::query(&sql);
        for id in object_ids {
            query = query.bind(id);
        }
        let result = query.execute(self.pool()).await?;
        Ok(result.rows_affected())
    }

    async fn restore_object(
        &self,
        object_id: &str,
        overwrite: bool,
        now: i64,
    ) -> Result<RestoreOutcome> {
        let mut tx = self.pool.begin().await?;
        let select_sql = format!(
            "SELECT {} FROM objects WHERE object_id = ? FOR UPDATE",
            OBJECT_COLUMNS
        );
        let row = sqlx::query(&select_sql)
            .bind(object_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(RestoreOutcome::NotFound);
        };
        let mut obj = row_to_object(&row)?;
        if obj.deleted_at.is_none() {
            return Ok(RestoreOutcome::Restored(obj));
        }

        let target_bucket = obj
            .original_bucket_id
            .clone()
            .unwrap_or_else(|| obj.bucket_id.clone());
        let target_key = obj.original_key.clone().unwrap_or_else(|| obj.key.clone());

        let bucket_exists: Option<String> =
            sqlx::query_scalar("SELECT bucket_id FROM buckets WHERE bucket_id = ?")
                .bind(&target_bucket)
                .fetch_optional(&mut *tx)
                .await?;
        if bucket_exists.is_none() {
            return Ok(RestoreOutcome::BucketMissing {
                bucket_id: target_bucket,
            });
        }

        let occupant: Option<String> = sqlx::query_scalar(
            "SELECT object_id FROM objects WHERE bucket_id = ? AND active_key = ? FOR UPDATE",
        )
        .bind(&target_bucket)
        .bind(&target_key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing_id) = occupant {
            if existing_id != obj.object_id {
                if !overwrite {
                    return Ok(RestoreOutcome::Conflict {
                        existing_object_id: existing_id,
                    });
                }
                sqlx::query(
                    "UPDATE objects SET deleted_at = ?, original_bucket_id = bucket_id, \
                     original_key = `key` WHERE object_id = ?",
                )
                .bind(now)
                .bind(&existing_id)
                .execute(&mut *tx)
                .await?;
            }
        }

        sqlx::query(
            "UPDATE objects SET bucket_id = ?, `key` = ?, deleted_at = NULL WHERE object_id = ?",
        )
        .bind(&target_bucket)
        .bind(&target_key)
        .bind(object_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        obj.bucket_id = target_bucket;
        obj.key = target_key;
        obj.deleted_at = None;
        Ok(RestoreOutcome::Restored(obj))
    }

    async fn list_trash(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Object>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        let mut sql = format!(
            "SELECT {} FROM objects WHERE deleted_at IS NOT NULL",
            OBJECT_COLUMNS
        );
        if app_id.is_some() {
            sql.push_str(" AND app_id = ?");
        }
        if bucket_id.is_some() {
            sql.push_str(" AND (bucket_id = ? OR original_bucket_id = ?)");
        }
        sql.push_str(" ORDER BY deleted_at DESC, object_id ASC LIMIT ? OFFSET ?");

        let mut query = sqlx::query(&sql);
        if let Some(app_id) = app_id {
            query = query.bind(app_id);
        }
        if let Some(bucket_id) = bucket_id {
            query = query.bind(bucket_id).bind(bucket_id);
        }
        query = query.bind(limit).bind(offset);

        let rows = query.fetch_all(self.pool()).await?;
        rows.iter().map(row_to_object).collect()
    }

    async fn count_trash(&self, app_id: Option<&str>, bucket_id: Option<&str>) -> Result<i64> {
        let mut sql = String::from("SELECT COUNT(*) FROM objects WHERE deleted_at IS NOT NULL");
        if app_id.is_some() {
            sql.push_str(" AND app_id = ?");
        }
        if bucket_id.is_some() {
            sql.push_str(" AND (bucket_id = ? OR original_bucket_id = ?)");
        }
        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        if let Some(app_id) = app_id {
            query = query.bind(app_id);
        }
        if let Some(bucket_id) = bucket_id {
            query = query.bind(bucket_id).bind(bucket_id);
        }
        Ok(query.fetch_one(self.pool()).await?)
    }

    async fn total_trash_size(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64> {
        let mut sql = String::from(
            "SELECT CAST(COALESCE(SUM(size), 0) AS SIGNED) FROM objects \
             WHERE deleted_at IS NOT NULL",
        );
        if app_id.is_some() {
            sql.push_str(" AND app_id = ?");
        }
        if bucket_id.is_some() {
            sql.push_str(" AND (bucket_id = ? OR original_bucket_id = ?)");
        }
        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        if let Some(app_id) = app_id {
            query = query.bind(app_id);
        }
        if let Some(bucket_id) = bucket_id {
            query = query.bind(bucket_id).bind(bucket_id);
        }
        Ok(query.fetch_one(self.pool()).await?)
    }

    async fn list_expired_trash(&self, before: i64, limit: i64) -> Result<Vec<Object>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {} FROM objects WHERE deleted_at IS NOT NULL AND deleted_at < ? \
             ORDER BY deleted_at ASC, object_id ASC LIMIT ?",
            OBJECT_COLUMNS
        );
        let rows = sqlx::query(&sql)
            .bind(before)
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_object).collect()
    }

    async fn list_all_objects(&self, offset: i64, limit: i64) -> Result<Vec<Object>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {} FROM objects ORDER BY object_id ASC LIMIT ? OFFSET ?",
            OBJECT_COLUMNS
        );
        let rows = sqlx::query(&sql)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_object).collect()
    }

    async fn count_all_objects(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM objects")
            .fetch_one(self.pool())
            .await?)
    }

    async fn save_maintenance_run(&self, run: &MaintenanceRun) -> Result<()> {
        sqlx::query(
            "INSERT INTO maintenance_runs \
             (run_id, kind, `trigger`, started_at, finished_at, duration_ms, status, \
              scanned, purged_objects, freed_bytes, invalid_records, orphan_files, \
              orphan_bytes, purged_nonces, fixed, error, detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run.run_id)
        .bind(&run.kind)
        .bind(&run.trigger)
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(run.duration_ms)
        .bind(&run.status)
        .bind(run.scanned)
        .bind(run.purged_objects)
        .bind(run.freed_bytes)
        .bind(run.invalid_records)
        .bind(run.orphan_files)
        .bind(run.orphan_bytes)
        .bind(run.purged_nonces)
        .bind(run.fixed)
        .bind(&run.error)
        .bind(&run.detail)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn latest_maintenance_run(&self, kind: &str) -> Result<Option<MaintenanceRun>> {
        let rows = self.list_maintenance_runs(kind, 1).await?;
        Ok(rows.into_iter().next())
    }

    async fn list_maintenance_runs(&self, kind: &str, limit: i64) -> Result<Vec<MaintenanceRun>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT run_id, kind, `trigger`, started_at, finished_at, duration_ms, status, \
             scanned, purged_objects, freed_bytes, invalid_records, orphan_files, \
             orphan_bytes, purged_nonces, fixed, error, detail \
             FROM maintenance_runs WHERE kind = ? \
             ORDER BY started_at DESC, run_id ASC LIMIT ?",
        )
        .bind(kind)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(row_to_run).collect()
    }

    async fn insert_nonce(&self, app_id: &str, nonce: &str, created_at: i64) -> Result<bool> {
        let result = sqlx::query("INSERT INTO nonces (app_id, nonce, created_at) VALUES (?, ?, ?)")
            .bind(app_id)
            .bind(nonce)
            .bind(created_at)
            .execute(self.pool())
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(false),
            Err(err) => Err(err).context("Failed to insert nonce"),
        }
    }

    async fn purge_nonces_before(&self, before: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM nonces WHERE created_at < ?")
            .bind(before)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected())
    }
}

impl MysqlStore {
    async fn put_object_attempt(&self, object: &Object) -> Result<Option<Object>> {
        let mut tx = self.pool.begin().await?;
        let now = chrono::Utc::now().timestamp();

        // 事务内读取旧 active 记录；若被不同对象占用，先移入回收站（保留文件）。
        let select_sql = format!(
            "SELECT {} FROM objects WHERE bucket_id = ? AND active_key = ? FOR UPDATE",
            OBJECT_COLUMNS
        );
        let previous_row = sqlx::query(&select_sql)
            .bind(&object.bucket_id)
            .bind(&object.key)
            .fetch_optional(&mut *tx)
            .await?;
        let previous = previous_row.as_ref().map(row_to_object).transpose()?;

        if let Some(old) = &previous {
            if old.object_id != object.object_id {
                sqlx::query(
                    "UPDATE objects SET deleted_at = ?, original_bucket_id = bucket_id, \
                     original_key = `key` WHERE object_id = ?",
                )
                .bind(now)
                .bind(&old.object_id)
                .execute(&mut *tx)
                .await?;
            }
        }

        sqlx::query(
            "INSERT INTO objects \
             (object_id, app_id, bucket_id, `key`, size, content_type, etag, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
             app_id = VALUES(app_id), size = VALUES(size), \
             content_type = VALUES(content_type), etag = VALUES(etag), \
             created_at = VALUES(created_at), deleted_at = NULL",
        )
        .bind(&object.object_id)
        .bind(&object.app_id)
        .bind(&object.bucket_id)
        .bind(&object.key)
        .bind(object.size)
        .bind(&object.content_type)
        .bind(&object.etag)
        .bind(object.created_at)
        .execute(&mut *tx)
        .await
        .context("Failed to upsert object")?;

        tx.commit().await?;
        Ok(previous)
    }
}

fn row_to_app(row: &MySqlRow) -> Result<App> {
    Ok(App {
        app_id: row.try_get("app_id")?,
        app_secret_encrypted: row.try_get("app_secret_encrypted")?,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
    })
}

fn row_to_bucket(row: &MySqlRow) -> Result<Bucket> {
    let is_public: i64 = row.try_get("is_public")?;
    Ok(Bucket {
        bucket_id: row.try_get("bucket_id")?,
        app_id: row.try_get("app_id")?,
        name: row.try_get("name")?,
        is_public: is_public != 0,
        created_at: row.try_get("created_at")?,
    })
}

/// ascii_bin 列在 MySQL 协议中会被报告为 VARBINARY，需要手动解码。
fn decode_text(row: &MySqlRow, column: &str) -> Result<String> {
    let bytes: Vec<u8> = row.try_get(column)?;
    String::from_utf8(bytes).with_context(|| format!("Object column '{}' is not valid UTF-8", column))
}

fn row_to_object(row: &MySqlRow) -> Result<Object> {
    let original_key: Option<String> = match row.try_get::<Option<Vec<u8>>, _>("original_key") {
        Ok(Some(bytes)) => Some(
            String::from_utf8(bytes).context("Object original_key is not valid UTF-8")?,
        ),
        _ => None,
    };
    Ok(Object {
        object_id: row.try_get("object_id")?,
        app_id: row.try_get("app_id")?,
        bucket_id: row.try_get("bucket_id")?,
        key: decode_text(row, "key")?,
        size: row.try_get("size")?,
        content_type: row.try_get("content_type")?,
        etag: row.try_get("etag")?,
        created_at: row.try_get("created_at")?,
        deleted_at: row.try_get("deleted_at")?,
        original_bucket_id: row.try_get("original_bucket_id")?,
        original_key,
    })
}

fn row_to_run(row: &MySqlRow) -> Result<MaintenanceRun> {
    Ok(MaintenanceRun {
        run_id: row.try_get("run_id")?,
        kind: row.try_get("kind")?,
        trigger: row.try_get("trigger")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        duration_ms: row.try_get("duration_ms")?,
        status: row.try_get("status")?,
        scanned: row.try_get("scanned")?,
        purged_objects: row.try_get("purged_objects")?,
        freed_bytes: row.try_get("freed_bytes")?,
        invalid_records: row.try_get("invalid_records")?,
        orphan_files: row.try_get("orphan_files")?,
        orphan_bytes: row.try_get("orphan_bytes")?,
        purged_nonces: row.try_get("purged_nonces")?,
        fixed: row.try_get("fixed")?,
        error: row.try_get("error")?,
        detail: row.try_get("detail")?,
    })
}

/// 只对可安全重试的并发冲突重试；其他错误直接上抛。
fn is_retryable(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(sqlx::Error::Database(db)) = cause.downcast_ref::<sqlx::Error>() {
            if let Some(code) = db.code() {
                // MySQL: 1213 deadlock, 1205 lock wait timeout.
                return code == "1213" || code == "1205";
            }
        }
    }
    false
}

/// 升级脚本中“列/索引/外键已存在或已不存在”属于可忽略错误。
fn is_ignorable_ddl_error(err: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = err {
        // SQLSTATE（部分驱动/代理会把它映射成 42S21 等）。
        if let Some(code) = db.code() {
            if matches!(code.as_ref(), "1060" | "1061" | "1091" | "1826") {
                return true;
            }
        }
        // MariaDB / MySQL 的厂商错误码。
        if let Some(mysql) = db.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
            return matches!(mysql.number(), 1060 | 1061 | 1091 | 1826);
        }
    }
    false
}
