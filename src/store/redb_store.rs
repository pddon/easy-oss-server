//! redb 存储后端（纯 Rust 嵌入式数据库，默认后端）。
//!
//! 索引设计：所有复合键用空字节（`\0`）拼接，便于范围查询。
//! 同步的 redb 调用全部放在 `spawn_blocking` 中，避免阻塞异步运行时。
//!
//! 回收站实现要点：
//! - `objects` 表保存全部记录（active + trashed），`objects_by_bucket_key` 只
//!   保存 active 记录，因此列表/计数/按 key 查询天然只看到 active 对象。
//! - 回收站列表通过扫描 `objects` 过滤 `deleted_at`，受限于单机规模，可接受。

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use redb::{
    Database, MultimapTableDefinition, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};

use crate::models::app::App;
use crate::models::bucket::Bucket;
use crate::models::maintenance::MaintenanceRun;
use crate::models::object::Object;
use crate::store::{RestoreOutcome, Store, StoreError, StoreHandle, SCHEMA_VERSION};

/// Apps: key = app_id, value = bincode(App)。
const APPS: TableDefinition<&str, &[u8]> = TableDefinition::new("apps");
/// Apps 按 created_at DESC 排序：key = `{neg_created}\0{app_id}`。
const APPS_BY_CREATED: TableDefinition<&str, ()> = TableDefinition::new("apps_by_created");
const BUCKETS: TableDefinition<&str, &[u8]> = TableDefinition::new("buckets");
/// key = `{app_id}\0{neg_created}\0{bucket_id}`。
const BUCKETS_BY_APP_CREATED: TableDefinition<&str, ()> =
    TableDefinition::new("buckets_by_app_created");
const BUCKETS_BY_NAME: TableDefinition<&str, &str> = TableDefinition::new("buckets_by_name");
/// key = `{app_id}\0{name}`。
const BUCKETS_BY_APP_NAME: TableDefinition<&str, &str> =
    TableDefinition::new("buckets_by_app_name");
const OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("objects");
/// key = `{bucket_id}\0{key}`，value = object_id。**只包含 active 对象**。
const OBJECTS_BY_BUCKET_KEY: TableDefinition<&str, &str> =
    TableDefinition::new("objects_by_bucket_key");
/// 防重放 nonce：key = `{app_id}\0{nonce}`，value = 写入时间。
const NONCES: MultimapTableDefinition<&str, i64> = MultimapTableDefinition::new("nonces");
const MIGRATIONS: TableDefinition<&str, i64> = TableDefinition::new("_migrations");
/// 维护任务运行记录：key = run_id，value = bincode(MaintenanceRun)。
const MAINTENANCE_RUNS: TableDefinition<&str, &[u8]> = TableDefinition::new("maintenance_runs");

const MIGRATION_V1: &str = "schema_v1_redb";
const MIGRATION_V2: &str = "schema_v2_redb";
const KEY_SEP: &str = "\0";
const KEY_HIGH: &str = "\u{10FFFF}\u{10FFFF}";

/// redb 后端。
pub struct RedbStore {
    db: Arc<Database>,
}

impl RedbStore {
    /// 打开（或创建）redb 数据库并应用迁移。
    pub async fn open(db_url: &str) -> Result<StoreHandle> {
        let path = parse_db_url(db_url)?;
        let path_ref = Path::new(&path);
        if let Some(parent) = path_ref.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }

        let db = Database::create(path_ref).context("Failed to open redb database")?;
        let db = Arc::new(db);
        create_tables(&db)?;

        let store = Arc::new(Self { db });
        store.migrate().await?;
        Ok(store)
    }
}

fn parse_db_url(url: &str) -> Result<String> {
    if let Some(path) = url.strip_prefix("redb://") {
        return Ok(path.to_string());
    }
    if !url.contains("://") {
        return Ok(url.to_string());
    }
    Err(anyhow!(
        "redb backend only accepts a file path or redb:// URL, got '{}'",
        url
    ))
}

fn create_tables(db: &Database) -> Result<()> {
    let tx = db.begin_write()?;
    {
        tx.open_table(APPS)?;
        tx.open_table(APPS_BY_CREATED)?;
        tx.open_table(BUCKETS)?;
        tx.open_table(BUCKETS_BY_APP_CREATED)?;
        tx.open_table(BUCKETS_BY_NAME)?;
        tx.open_table(BUCKETS_BY_APP_NAME)?;
        tx.open_table(OBJECTS)?;
        tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
        tx.open_multimap_table(NONCES)?;
        tx.open_table(MIGRATIONS)?;
        tx.open_table(MAINTENANCE_RUNS)?;
    }
    tx.commit()?;
    Ok(())
}

fn serialize<T: serde::Serialize>(val: &T) -> Result<Vec<u8>> {
    bincode::serialize(val).context("Failed to serialize metadata")
}

fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    bincode::deserialize(bytes).context("Failed to deserialize metadata")
}

fn table_get<T>(table: &impl ReadableTable<&'static str, &'static [u8]>, key: &str) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    let Some(guard) = table.get(key)? else {
        return Ok(None);
    };
    Ok(Some(deserialize::<T>(guard.value())?))
}

fn is_active(obj: &Object) -> bool {
    obj.deleted_at.is_none()
}

/// v1 的对象结构（v2 新增 app_id / 回收站字段），用于旧库自动升级。
#[derive(serde::Deserialize)]
struct LegacyObject {
    object_id: String,
    bucket_id: String,
    key: String,
    size: i64,
    content_type: String,
    etag: String,
    created_at: i64,
}

/// 把 created_at 编码成字典序 DESC 的零填充字符串。
fn encode_created_desc(ts: i64) -> String {
    let neg = (u64::MAX as i128) - (ts as i128);
    format!("{:020}", neg as u64)
}

fn composite_key(parts: &[&str]) -> String {
    parts.join(KEY_SEP)
}

fn range_prefix(prefix: &str) -> (String, String) {
    (
        format!("{}{}", prefix, KEY_SEP),
        format!("{}{}", prefix, KEY_HIGH),
    )
}

/// 在写事务内把一个对象移入回收站（保留磁盘文件）。
fn trash_object_in_tx(tx: &redb::WriteTransaction, obj: &mut Object, now: i64) -> Result<()> {
    if obj.deleted_at.is_some() {
        return Ok(());
    }
    obj.deleted_at = Some(now);
    obj.original_bucket_id = Some(obj.bucket_id.clone());
    obj.original_key = Some(obj.key.clone());

    let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
    let index_key = composite_key(&[&obj.bucket_id, &obj.key]);
    idx.remove(index_key.as_str())?;
    drop(idx);

    let mut table = tx.open_table(OBJECTS)?;
    table.insert(obj.object_id.as_str(), serialize(obj)?.as_slice())?;
    Ok(())
}

/// 读取全部回收站记录（调用方负责过滤与排序）。
fn collect_trash(tx: &redb::ReadTransaction) -> Result<Vec<Object>> {
    let table = tx.open_table(OBJECTS)?;
    let mut out = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        let obj: Object = deserialize(value.value())?;
        if obj.is_trashed() {
            out.push(obj);
        }
    }
    Ok(out)
}

fn sort_trash(items: &mut [Object]) {
    items.sort_by(|a, b| {
        b.deleted_at
            .cmp(&a.deleted_at)
            .then_with(|| a.object_id.cmp(&b.object_id))
    });
}

#[async_trait]
impl Store for RedbStore {
    fn backend_name(&self) -> &'static str {
        "redb"
    }

    async fn migrate(&self) -> Result<()> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            {
                let tx = db.begin_read()?;
                let table = tx.open_table(MIGRATIONS)?;
                if table.get(MIGRATION_V2)?.is_some() {
                    return Ok(());
                }
            }

            let tx = db.begin_write()?;
            {
                // 旧库（v1）自动升级：把旧格式对象记录改写为新格式。
                let object_ids: Vec<String> = {
                    let table = tx.open_table(OBJECTS)?;
                    let mut ids = Vec::new();
                    for entry in table.iter()? {
                        let (key, value) = entry?;
                        if deserialize::<Object>(value.value()).is_err() {
                            ids.push(key.value().to_string());
                        }
                    }
                    ids
                };

                for object_id in object_ids {
                    let raw: Option<Vec<u8>> = {
                        let table = tx.open_table(OBJECTS)?;
                        let mut bytes = None;
                        if let Some(guard) = table.get(object_id.as_str())? {
                            bytes = Some(guard.value().to_vec());
                        }
                        bytes
                    };
                    let legacy: Option<LegacyObject> = raw
                        .and_then(|bytes| bincode::deserialize::<LegacyObject>(&bytes).ok());
                    let Some(legacy) = legacy else {
                        continue;
                    };
                    let app_id = {
                        let buckets = tx.open_table(BUCKETS)?;
                        table_get::<Bucket>(&buckets, &legacy.bucket_id)?
                            .map(|b| b.app_id)
                            .unwrap_or_default()
                    };
                    let upgraded = Object {
                        object_id: legacy.object_id,
                        app_id,
                        bucket_id: legacy.bucket_id,
                        key: legacy.key,
                        size: legacy.size,
                        content_type: legacy.content_type,
                        etag: legacy.etag,
                        created_at: legacy.created_at,
                        deleted_at: None,
                        original_bucket_id: None,
                        original_key: None,
                    };
                    let mut table = tx.open_table(OBJECTS)?;
                    table.insert(
                        upgraded.object_id.as_str(),
                        serialize(&upgraded)?.as_slice(),
                    )?;
                }

                let now = chrono::Utc::now().timestamp();
                let mut table = tx.open_table(MIGRATIONS)?;
                table.insert(MIGRATION_V1, now)?;
                table.insert(MIGRATION_V2, now)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn schema_version(&self) -> Result<u32> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<u32> {
            let tx = db.begin_read()?;
            let table = tx.open_table(MIGRATIONS)?;
            if table.get(MIGRATION_V2)?.is_some() {
                Ok(SCHEMA_VERSION)
            } else if table.get(MIGRATION_V1)?.is_some() {
                Ok(1)
            } else {
                Ok(0)
            }
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn insert_app(&self, app: &App) -> Result<()> {
        let db = self.db.clone();
        let app = app.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let tx = db.begin_write()?;
            {
                let mut table = tx.open_table(APPS)?;
                let bytes = serialize(&app)?;
                table.insert(app.app_id.as_str(), bytes.as_slice())?;

                let mut idx = tx.open_table(APPS_BY_CREATED)?;
                let idx_key =
                    composite_key(&[&encode_created_desc(app.created_at), &app.app_id]);
                idx.insert(idx_key.as_str(), ())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_app(&self, app_id: &str) -> Result<Option<App>> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<App>> {
            let tx = db.begin_read()?;
            let table = tx.open_table(APPS)?;
            table_get::<App>(&table, &app_id)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_apps(&self, offset: i64, limit: i64) -> Result<Vec<App>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<App>> {
            if offset < 0 || limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let idx = tx.open_table(APPS_BY_CREATED)?;
            let apps = tx.open_table(APPS)?;
            let mut results = Vec::new();
            let mut skipped = 0i64;
            for entry in idx.range(""..=KEY_HIGH)? {
                if results.len() as i64 >= limit {
                    break;
                }
                let (key_guard, _) = entry?;
                let key = key_guard.value();
                let app_id = key.split_once(KEY_SEP).map(|(_, rest)| rest).unwrap_or("");
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if let Some(app) = table_get::<App>(&apps, app_id)? {
                    results.push(app);
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_apps(&self) -> Result<i64> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let table = tx.open_table(APPS)?;
            Ok(table.len()? as i64)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn update_app_secret(&self, app_id: &str, encrypted: &str) -> Result<bool> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        let encrypted = encrypted.to_string();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let tx = db.begin_write()?;
            let found = {
                let mut table = tx.open_table(APPS)?;
                let Some(mut app) = table_get::<App>(&table, &app_id)? else {
                    return Ok(false);
                };
                app.app_secret_encrypted = encrypted;
                table.insert(app_id.as_str(), serialize(&app)?.as_slice())?;
                true
            };
            tx.commit()?;
            Ok(found)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn delete_app(&self, app_id: &str) -> Result<bool> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let tx = db.begin_write()?;
            let exists = {
                let table = tx.open_table(APPS)?;
                let found = table.get(app_id.as_str())?;
                found.is_some()
            };
            if !exists {
                return Ok(false);
            }

            let bucket_ids: Vec<String> = {
                let idx = tx.open_table(BUCKETS_BY_APP_CREATED)?;
                let (start, end) = range_prefix(&app_id);
                let mut ids = Vec::new();
                for entry in idx.range(start.as_str()..=end.as_str())? {
                    let (key_guard, _) = entry?;
                    let parts: Vec<&str> = key_guard.value().splitn(3, KEY_SEP).collect();
                    if parts.len() == 3 {
                        ids.push(parts[2].to_string());
                    }
                }
                ids
            };

            let now = chrono::Utc::now().timestamp();
            for bucket_id in &bucket_ids {
                delete_bucket_rows(&tx, bucket_id, now)?;
            }

            {
                let mut idx = tx.open_table(APPS_BY_CREATED)?;
                if let Some(app) = {
                    let apps = tx.open_table(APPS)?;
                    table_get::<App>(&apps, &app_id)?
                } {
                    let idx_key = composite_key(&[&encode_created_desc(app.created_at), &app_id]);
                    idx.remove(idx_key.as_str())?;
                }
                let mut apps = tx.open_table(APPS)?;
                apps.remove(app_id.as_str())?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn insert_bucket(&self, bucket: &Bucket) -> Result<()> {
        let db = self.db.clone();
        let bucket = bucket.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let tx = db.begin_write()?;
            {
                let idx_name = tx.open_table(BUCKETS_BY_NAME)?;
                if idx_name.get(bucket.name.as_str())?.is_some() {
                    return Err(StoreError::DuplicateBucketName(bucket.name.clone()).into());
                }
            }
            {
                let mut table = tx.open_table(BUCKETS)?;
                table.insert(
                    bucket.bucket_id.as_str(),
                    serialize(&bucket)?.as_slice(),
                )?;
                let mut idx = tx.open_table(BUCKETS_BY_APP_CREATED)?;
                let key = composite_key(&[
                    &bucket.app_id,
                    &encode_created_desc(bucket.created_at),
                    &bucket.bucket_id,
                ]);
                idx.insert(key.as_str(), ())?;
                let mut idx_name = tx.open_table(BUCKETS_BY_NAME)?;
                idx_name.insert(bucket.name.as_str(), bucket.bucket_id.as_str())?;
                let mut idx_app_name = tx.open_table(BUCKETS_BY_APP_NAME)?;
                let key = composite_key(&[&bucket.app_id, &bucket.name]);
                idx_app_name.insert(key.as_str(), bucket.bucket_id.as_str())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_bucket_by_id(&self, bucket_id: &str) -> Result<Option<Bucket>> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Bucket>> {
            let tx = db.begin_read()?;
            let table = tx.open_table(BUCKETS)?;
            table_get::<Bucket>(&table, &bucket_id)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_bucket_by_name(&self, name: &str) -> Result<Option<Bucket>> {
        let db = self.db.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Bucket>> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(BUCKETS_BY_NAME)?;
            let Some(id) = idx.get(name.as_str())? else {
                return Ok(None);
            };
            let bucket_id = id.value().to_string();
            let table = tx.open_table(BUCKETS)?;
            table_get::<Bucket>(&table, &bucket_id)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_bucket_by_app_and_name(
        &self,
        app_id: &str,
        name: &str,
    ) -> Result<Option<Bucket>> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Bucket>> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(BUCKETS_BY_APP_NAME)?;
            let key = composite_key(&[&app_id, &name]);
            let Some(id) = idx.get(key.as_str())? else {
                return Ok(None);
            };
            let bucket_id = id.value().to_string();
            let table = tx.open_table(BUCKETS)?;
            table_get::<Bucket>(&table, &bucket_id)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_buckets_by_app(
        &self,
        app_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Bucket>> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<Bucket>> {
            if offset < 0 || limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let idx = tx.open_table(BUCKETS_BY_APP_CREATED)?;
            let buckets = tx.open_table(BUCKETS)?;
            let (start, end) = range_prefix(&app_id);
            let mut results = Vec::new();
            let mut skipped = 0i64;
            for entry in idx.range(start.as_str()..=end.as_str())? {
                if results.len() as i64 >= limit {
                    break;
                }
                let (key_guard, _) = entry?;
                let parts: Vec<&str> = key_guard.value().splitn(3, KEY_SEP).collect();
                let bucket_id = if parts.len() == 3 { parts[2] } else { "" };
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if let Some(bucket) = table_get::<Bucket>(&buckets, bucket_id)? {
                    results.push(bucket);
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_buckets_by_app(&self, app_id: &str) -> Result<i64> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(BUCKETS_BY_APP_CREATED)?;
            let (start, end) = range_prefix(&app_id);
            let mut count = 0i64;
            for entry in idx.range(start.as_str()..=end.as_str())? {
                let _ = entry?;
                count += 1;
            }
            Ok(count)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn set_bucket_public(&self, bucket_id: &str, is_public: bool) -> Result<bool> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let tx = db.begin_write()?;
            let found = {
                let mut table = tx.open_table(BUCKETS)?;
                let Some(mut bucket) = table_get::<Bucket>(&table, &bucket_id)? else {
                    return Ok(false);
                };
                bucket.is_public = is_public;
                table.insert(bucket_id.as_str(), serialize(&bucket)?.as_slice())?;
                true
            };
            tx.commit()?;
            Ok(found)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn delete_bucket(&self, bucket_id: &str) -> Result<bool> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let tx = db.begin_write()?;
            let exists = {
                let table = tx.open_table(BUCKETS)?;
                let found = table.get(bucket_id.as_str())?;
                found.is_some()
            };
            if !exists {
                return Ok(false);
            }
            delete_bucket_rows(&tx, &bucket_id, chrono::Utc::now().timestamp())?;
            tx.commit()?;
            Ok(true)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn put_object(&self, object: &Object) -> Result<Option<Object>> {
        let db = self.db.clone();
        let object = object.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_write()?;
            let now = chrono::Utc::now().timestamp();
            let previous = {
                let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                let index_key = composite_key(&[&object.bucket_id, &object.key]);
                let previous_id = idx
                    .get(index_key.as_str())?
                    .map(|guard| guard.value().to_string());
                drop(idx);
                match previous_id {
                    Some(object_id) => {
                        let objects = tx.open_table(OBJECTS)?;
                        table_get::<Object>(&objects, &object_id)?
                    }
                    None => None,
                }
            };
            // 被覆盖的旧版本进入回收站（保留磁盘文件），而不是被删除。
            let mut previous = previous;
            if let Some(old) = previous.as_mut() {
                if old.object_id != object.object_id {
                    trash_object_in_tx(&tx, old, now)?;
                }
            }
            {
                let mut objects = tx.open_table(OBJECTS)?;
                objects.insert(
                    object.object_id.as_str(),
                    serialize(&object)?.as_slice(),
                )?;
                drop(objects);
                let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                let index_key = composite_key(&[&object.bucket_id, &object.key]);
                idx.insert(index_key.as_str(), object.object_id.as_str())?;
            }
            tx.commit()?;
            Ok(previous)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_object_by_id(&self, object_id: &str) -> Result<Option<Object>> {
        let db = self.db.clone();
        let object_id = object_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            Ok(table_get::<Object>(&table, &object_id)?.filter(is_active))
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_object_any(&self, object_id: &str) -> Result<Option<Object>> {
        let db = self.db.clone();
        let object_id = object_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            table_get::<Object>(&table, &object_id)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn get_object_by_key(&self, bucket_id: &str, key: &str) -> Result<Option<Object>> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
            let index_key = composite_key(&[&bucket_id, &key]);
            let Some(id) = idx.get(index_key.as_str())? else {
                return Ok(None);
            };
            let object_id = id.value().to_string();
            let table = tx.open_table(OBJECTS)?;
            Ok(table_get::<Object>(&table, &object_id)?.filter(is_active))
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_objects(
        &self,
        bucket_id: &str,
        prefix: &str,
        marker: &str,
        max_keys: i64,
    ) -> Result<Vec<Object>> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        let prefix = prefix.to_string();
        let marker = marker.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<Object>> {
            if max_keys <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
            let objects = tx.open_table(OBJECTS)?;

            let end_key = if prefix.is_empty() {
                format!("{}{}", bucket_id, KEY_HIGH)
            } else {
                format!("{}{}{}{}", bucket_id, KEY_SEP, prefix, KEY_HIGH)
            };
            let suffix = if !marker.is_empty() {
                marker.clone()
            } else {
                prefix.clone()
            };
            let start_key = format!("{}{}{}", bucket_id, KEY_SEP, suffix);

            let mut results = Vec::new();
            for entry in idx.range(start_key.as_str()..=end_key.as_str())? {
                if results.len() as i64 >= max_keys {
                    break;
                }
                let (key_guard, val_guard) = entry?;
                let full_key = key_guard.value();
                let object_id = val_guard.value().to_string();
                let object_key = full_key
                    .split_once(KEY_SEP)
                    .map(|(_, rest)| rest)
                    .unwrap_or("");

                if !marker.is_empty() && object_key <= marker.as_str() {
                    continue;
                }
                if !prefix.is_empty() && !object_key.starts_with(&prefix) {
                    continue;
                }
                if let Some(obj) = table_get::<Object>(&objects, &object_id)? {
                    if is_active(&obj) {
                        results.push(obj);
                    }
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_objects_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
            let (start, end) = range_prefix(&bucket_id);
            let mut count = 0i64;
            for entry in idx.range(start.as_str()..=end.as_str())? {
                let _ = entry?;
                count += 1;
            }
            Ok(count)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn total_size_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        let db = self.db.clone();
        let bucket_id = bucket_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
            let objects = tx.open_table(OBJECTS)?;
            let (start, end) = range_prefix(&bucket_id);
            let mut total = 0i64;
            for entry in idx.range(start.as_str()..=end.as_str())? {
                let (_, val) = entry?;
                if let Some(obj) = table_get::<Object>(&objects, val.value())? {
                    total += obj.size;
                }
            }
            Ok(total)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_objects_by_app(&self, app_id: &str) -> Result<i64> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            let mut count = 0i64;
            for entry in table.iter()? {
                let (_, value) = entry?;
                let obj: Object = deserialize(value.value())?;
                if is_active(&obj) && obj.app_id == app_id {
                    count += 1;
                }
            }
            Ok(count)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn total_size_by_app(&self, app_id: &str) -> Result<i64> {
        let db = self.db.clone();
        let app_id = app_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            let mut total = 0i64;
            for entry in table.iter()? {
                let (_, value) = entry?;
                let obj: Object = deserialize(value.value())?;
                if is_active(&obj) && obj.app_id == app_id {
                    total += obj.size;
                }
            }
            Ok(total)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn soft_delete_object(&self, object_id: &str, deleted_at: i64) -> Result<Option<Object>> {
        let db = self.db.clone();
        let object_id = object_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_write()?;
            let Some(mut obj) = ({
                let table = tx.open_table(OBJECTS)?;
                table_get::<Object>(&table, &object_id)?
            }) else {
                return Ok(None);
            };
            trash_object_in_tx(&tx, &mut obj, deleted_at)?;
            tx.commit()?;
            Ok(Some(obj))
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn purge_object(&self, object_id: &str) -> Result<Option<Object>> {
        let db = self.db.clone();
        let object_id = object_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Object>> {
            let tx = db.begin_write()?;
            let Some(obj) = ({
                let table = tx.open_table(OBJECTS)?;
                table_get::<Object>(&table, &object_id)?
            }) else {
                return Ok(None);
            };
            if is_active(&obj) {
                let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                let index_key = composite_key(&[&obj.bucket_id, &obj.key]);
                idx.remove(index_key.as_str())?;
            }
            {
                let mut table = tx.open_table(OBJECTS)?;
                table.remove(object_id.as_str())?;
            }
            tx.commit()?;
            Ok(Some(obj))
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn purge_objects(&self, object_ids: &[String]) -> Result<u64> {
        let db = self.db.clone();
        let object_ids = object_ids.to_vec();
        tokio::task::spawn_blocking(move || -> Result<u64> {
            let tx = db.begin_write()?;
            let mut removed = 0u64;
            for object_id in &object_ids {
                let Some(obj) = ({
                    let table = tx.open_table(OBJECTS)?;
                    table_get::<Object>(&table, object_id)?
                }) else {
                    continue;
                };
                if is_active(&obj) {
                    let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                    let index_key = composite_key(&[&obj.bucket_id, &obj.key]);
                    idx.remove(index_key.as_str())?;
                }
                {
                    let mut table = tx.open_table(OBJECTS)?;
                    table.remove(object_id.as_str())?;
                }
                removed += 1;
            }
            tx.commit()?;
            Ok(removed)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn restore_object(
        &self,
        object_id: &str,
        overwrite: bool,
        now: i64,
    ) -> Result<RestoreOutcome> {
        let db = self.db.clone();
        let object_id = object_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<RestoreOutcome> {
            let tx = db.begin_write()?;
            let Some(mut obj) = ({
                let table = tx.open_table(OBJECTS)?;
                table_get::<Object>(&table, &object_id)?
            }) else {
                return Ok(RestoreOutcome::NotFound);
            };
            if is_active(&obj) {
                return Ok(RestoreOutcome::Restored(obj));
            }

            let target_bucket = obj
                .original_bucket_id
                .clone()
                .unwrap_or_else(|| obj.bucket_id.clone());
            let target_key = obj.original_key.clone().unwrap_or_else(|| obj.key.clone());

            let bucket_exists = {
                let buckets = tx.open_table(BUCKETS)?;
                let mut exists = false;
                if buckets.get(target_bucket.as_str())?.is_some() {
                    exists = true;
                }
                exists
            };
            if !bucket_exists {
                return Ok(RestoreOutcome::BucketMissing {
                    bucket_id: target_bucket,
                });
            }

            let index_key = composite_key(&[&target_bucket, &target_key]);
            let occupant = {
                let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                let mut occupant = None;
                if let Some(guard) = idx.get(index_key.as_str())? {
                    occupant = Some(guard.value().to_string());
                }
                occupant
            };
            if let Some(existing_id) = occupant {
                if existing_id != obj.object_id {
                    if !overwrite {
                        return Ok(RestoreOutcome::Conflict {
                            existing_object_id: existing_id,
                        });
                    }
                    let mut occupant_obj = {
                        let table = tx.open_table(OBJECTS)?;
                        table_get::<Object>(&table, &existing_id)?
                    };
                    if let Some(occupant_obj) = occupant_obj.as_mut() {
                        trash_object_in_tx(&tx, occupant_obj, now)?;
                    }
                }
            }

            obj.bucket_id = target_bucket;
            obj.key = target_key;
            obj.deleted_at = None;
            {
                let mut table = tx.open_table(OBJECTS)?;
                table.insert(obj.object_id.as_str(), serialize(&obj)?.as_slice())?;
                drop(table);
                let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
                let index_key = composite_key(&[&obj.bucket_id, &obj.key]);
                idx.insert(index_key.as_str(), obj.object_id.as_str())?;
            }
            tx.commit()?;
            Ok(RestoreOutcome::Restored(obj))
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_trash(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Object>> {
        let db = self.db.clone();
        let app_id = app_id.map(|s| s.to_string());
        let bucket_id = bucket_id.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || -> Result<Vec<Object>> {
            if offset < 0 || limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let mut items = collect_trash(&tx)?;
            items.retain(|o| {
                app_id.as_deref().map(|a| o.app_id == a).unwrap_or(true)
                    && bucket_id
                        .as_deref()
                        .map(|b| o.bucket_id == b || o.original_bucket_id.as_deref() == Some(b))
                        .unwrap_or(true)
            });
            sort_trash(&mut items);
            Ok(items
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_trash(&self, app_id: Option<&str>, bucket_id: Option<&str>) -> Result<i64> {
        let db = self.db.clone();
        let app_id = app_id.map(|s| s.to_string());
        let bucket_id = bucket_id.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let items = collect_trash(&tx)?;
            Ok(items
                .iter()
                .filter(|o| {
                    app_id.as_deref().map(|a| o.app_id == a).unwrap_or(true)
                        && bucket_id
                            .as_deref()
                            .map(|b| {
                                o.bucket_id == b || o.original_bucket_id.as_deref() == Some(b)
                            })
                            .unwrap_or(true)
                })
                .count() as i64)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn total_trash_size(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64> {
        let db = self.db.clone();
        let app_id = app_id.map(|s| s.to_string());
        let bucket_id = bucket_id.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let items = collect_trash(&tx)?;
            Ok(items
                .iter()
                .filter(|o| {
                    app_id.as_deref().map(|a| o.app_id == a).unwrap_or(true)
                        && bucket_id
                            .as_deref()
                            .map(|b| {
                                o.bucket_id == b || o.original_bucket_id.as_deref() == Some(b)
                            })
                            .unwrap_or(true)
                })
                .map(|o| o.size)
                .sum())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_expired_trash(&self, before: i64, limit: i64) -> Result<Vec<Object>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Object>> {
            if limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let mut items: Vec<Object> = collect_trash(&tx)?
                .into_iter()
                .filter(|o| o.deleted_at.map(|ts| ts < before).unwrap_or(false))
                .collect();
            items.sort_by(|a, b| {
                a.deleted_at
                    .cmp(&b.deleted_at)
                    .then_with(|| a.object_id.cmp(&b.object_id))
            });
            Ok(items.into_iter().take(limit as usize).collect())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn list_all_objects(&self, offset: i64, limit: i64) -> Result<Vec<Object>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Object>> {
            if offset < 0 || limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            let mut out = Vec::new();
            let mut skipped = 0i64;
            for entry in table.iter()? {
                let (_, value) = entry?;
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if out.len() as i64 >= limit {
                    break;
                }
                out.push(deserialize::<Object>(value.value())?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn count_all_objects(&self) -> Result<i64> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let tx = db.begin_read()?;
            let table = tx.open_table(OBJECTS)?;
            Ok(table.len()? as i64)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn save_maintenance_run(&self, run: &MaintenanceRun) -> Result<()> {
        let db = self.db.clone();
        let run = run.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let tx = db.begin_write()?;
            {
                let mut table = tx.open_table(MAINTENANCE_RUNS)?;
                table.insert(run.run_id.as_str(), serialize(&run)?.as_slice())?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn latest_maintenance_run(&self, kind: &str) -> Result<Option<MaintenanceRun>> {
        let mut runs = self.list_maintenance_runs(kind, 1).await?;
        Ok(if runs.is_empty() {
            None
        } else {
            Some(runs.remove(0))
        })
    }

    async fn list_maintenance_runs(&self, kind: &str, limit: i64) -> Result<Vec<MaintenanceRun>> {
        let db = self.db.clone();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<MaintenanceRun>> {
            if limit <= 0 {
                return Ok(Vec::new());
            }
            let tx = db.begin_read()?;
            let table = tx.open_table(MAINTENANCE_RUNS)?;
            let mut runs = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                let run: MaintenanceRun = deserialize(value.value())?;
                if run.kind == kind {
                    runs.push(run);
                }
            }
            runs.sort_by(|a, b| {
                b.started_at
                    .cmp(&a.started_at)
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            Ok(runs.into_iter().take(limit as usize).collect())
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn insert_nonce(&self, app_id: &str, nonce: &str, created_at: i64) -> Result<bool> {
        let db = self.db.clone();
        let key = composite_key(&[app_id, nonce]);
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let tx = db.begin_write()?;
            let inserted = {
                let mut table = tx.open_multimap_table(NONCES)?;
                if !table.get(key.as_str())?.is_empty() {
                    false
                } else {
                    table.insert(key.as_str(), created_at)?;
                    true
                }
            };
            tx.commit()?;
            Ok(inserted)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }

    async fn purge_nonces_before(&self, before: i64) -> Result<u64> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<u64> {
            let tx = db.begin_write()?;
            let mut removed = 0u64;
            {
                let mut table = tx.open_multimap_table(NONCES)?;
                let mut keys = Vec::new();
                for entry in table.iter()? {
                    let (key_guard, values) = entry?;
                    let stale: Vec<i64> = values
                        .into_iter()
                        .filter_map(|v| v.ok())
                        .map(|v| v.value())
                        .filter(|v| *v < before)
                        .collect();
                    if !stale.is_empty() {
                        keys.push((key_guard.value().to_string(), stale));
                    }
                }
                for (key, values) in keys {
                    for value in values {
                        table.remove(key.as_str(), value)?;
                        removed += 1;
                    }
                }
            }
            tx.commit()?;
            Ok(removed)
        })
        .await
        .map_err(|e| anyhow!("Storage task panicked: {}", e))?
    }
}

/// 在已有写事务中删除桶：先把桶内 active 对象移入回收站，再删除桶记录。
/// 回收站对象保留（`bucket_id` 成为悬空引用，原桶信息在 original_* 字段）。
fn delete_bucket_rows(tx: &redb::WriteTransaction, bucket_id: &str, now: i64) -> Result<()> {
    let (bucket_app_id, bucket_name) = {
        let table = tx.open_table(BUCKETS)?;
        match table_get::<Bucket>(&table, bucket_id)? {
            Some(b) => (b.app_id, b.name),
            None => return Ok(()),
        }
    };

    let object_ids: Vec<String> = {
        let idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
        let (start, end) = range_prefix(bucket_id);
        let mut ids = Vec::new();
        for entry in idx.range(start.as_str()..=end.as_str())? {
            let (_, val) = entry?;
            ids.push(val.value().to_string());
        }
        ids
    };
    for id in &object_ids {
        let mut obj = {
            let objects = tx.open_table(OBJECTS)?;
            table_get::<Object>(&objects, id)?
        };
        if let Some(obj) = obj.as_mut() {
            trash_object_in_tx(tx, obj, now)?;
        }
    }
    {
        let mut idx = tx.open_table(OBJECTS_BY_BUCKET_KEY)?;
        let (start, end) = range_prefix(bucket_id);
        let mut keys = Vec::new();
        for entry in idx.range(start.as_str()..=end.as_str())? {
            let (key_guard, _) = entry?;
            keys.push(key_guard.value().to_string());
        }
        for key in keys {
            idx.remove(key.as_str())?;
        }
    }
    {
        let bucket = {
            let table = tx.open_table(BUCKETS)?;
            table_get::<Bucket>(&table, bucket_id)?
        };
        if let Some(bucket) = bucket {
            let mut idx = tx.open_table(BUCKETS_BY_APP_CREATED)?;
            let key = composite_key(&[
                &bucket_app_id,
                &encode_created_desc(bucket.created_at),
                bucket_id,
            ]);
            idx.remove(key.as_str())?;
            let mut idx_name = tx.open_table(BUCKETS_BY_NAME)?;
            idx_name.remove(bucket_name.as_str())?;
            let mut idx_app_name = tx.open_table(BUCKETS_BY_APP_NAME)?;
            let key = composite_key(&[&bucket_app_id, &bucket_name]);
            idx_app_name.remove(key.as_str())?;
            let mut table = tx.open_table(BUCKETS)?;
            table.remove(bucket_id)?;
        }
    }
    Ok(())
}
