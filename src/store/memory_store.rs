//! 纯内存存储后端：用于测试，以及“不依赖数据库”的部署形态。
//!
//! 所有操作在 `RwLock` 内完成，天然满足 trait 契约要求的原子性与排序。
//!
//! 回收站实现：`objects` 保存全部记录（active + trashed），`object_keys`
//! 只保存 active 记录，因此正常查询天然看不到回收站对象。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use async_trait::async_trait;

use crate::models::app::App;
use crate::models::bucket::Bucket;
use crate::models::maintenance::MaintenanceRun;
use crate::models::object::Object;
use crate::store::{RestoreOutcome, Store, StoreError, StoreHandle, SCHEMA_VERSION};

const MIGRATION_V1: &str = "schema_v1_memory";
const MIGRATION_V2: &str = "schema_v2_memory";

#[derive(Default)]
struct Inner {
    apps: HashMap<String, App>,
    buckets: HashMap<String, Bucket>,
    /// object_id -> Object（含回收站对象）
    objects: HashMap<String, Object>,
    /// (bucket_id, key) -> object_id（只含 active 对象）
    object_keys: HashMap<(String, String), String>,
    nonces: HashMap<(String, String), i64>,
    maintenance_runs: HashMap<String, MaintenanceRun>,
    applied_migrations: Vec<String>,
}

/// 内存后端。
pub struct MemoryStore {
    inner: RwLock<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        let mut inner = Inner::default();
        // 内存后端无需真实迁移，启动即视为已应用最新版本。
        inner.applied_migrations.push(MIGRATION_V1.to_string());
        inner.applied_migrations.push(MIGRATION_V2.to_string());
        Self {
            inner: RwLock::new(inner),
        }
    }

    pub fn create() -> StoreHandle {
        Arc::new(Self::new())
    }

    fn with_read<T>(&self, f: impl FnOnce(&Inner) -> T) -> T {
        let guard = self.inner.read().expect("memory store lock poisoned");
        f(&guard)
    }

    fn with_write<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self.inner.write().expect("memory store lock poisoned");
        f(&mut guard)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Store for MemoryStore {
    fn backend_name(&self) -> &'static str {
        "memory"
    }

    async fn migrate(&self) -> Result<()> {
        self.with_write(|inner| {
            for name in [MIGRATION_V1, MIGRATION_V2] {
                if !inner.applied_migrations.iter().any(|m| m == name) {
                    inner.applied_migrations.push(name.to_string());
                }
            }
        });
        Ok(())
    }

    async fn schema_version(&self) -> Result<u32> {
        Ok(
            if self.with_read(|inner| inner.applied_migrations.iter().any(|m| m == MIGRATION_V2)) {
                SCHEMA_VERSION
            } else {
                1
            },
        )
    }

    async fn insert_app(&self, app: &App) -> Result<()> {
        self.with_write(|inner| {
            inner.apps.insert(app.app_id.clone(), app.clone());
        });
        Ok(())
    }

    async fn get_app(&self, app_id: &str) -> Result<Option<App>> {
        Ok(self.with_read(|inner| inner.apps.get(app_id).cloned()))
    }

    async fn list_apps(&self, offset: i64, limit: i64) -> Result<Vec<App>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        Ok(self.with_read(|inner| {
            let mut apps: Vec<App> = inner.apps.values().cloned().collect();
            apps.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.app_id.cmp(&b.app_id))
            });
            apps.into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect()
        }))
    }

    async fn count_apps(&self) -> Result<i64> {
        Ok(self.with_read(|inner| inner.apps.len() as i64))
    }

    async fn update_app_secret(&self, app_id: &str, encrypted: &str) -> Result<bool> {
        Ok(self.with_write(|inner| {
            let Some(app) = inner.apps.get_mut(app_id) else {
                return false;
            };
            app.app_secret_encrypted = encrypted.to_string();
            true
        }))
    }

    async fn delete_app(&self, app_id: &str) -> Result<bool> {
        Ok(self.with_write(|inner| {
            if inner.apps.remove(app_id).is_none() {
                return false;
            }
            let bucket_ids: Vec<String> = inner
                .buckets
                .values()
                .filter(|b| b.app_id == app_id)
                .map(|b| b.bucket_id.clone())
                .collect();
            let now = chrono::Utc::now().timestamp();
            for bucket_id in bucket_ids {
                remove_bucket(inner, &bucket_id, now);
            }
            true
        }))
    }

    async fn insert_bucket(&self, bucket: &Bucket) -> Result<()> {
        self.with_write(|inner| {
            if let Some(existing) = inner.buckets.values().find(|b| b.name == bucket.name) {
                return Err(StoreError::DuplicateBucketName(existing.name.clone()).into());
            }
            inner
                .buckets
                .insert(bucket.bucket_id.clone(), bucket.clone());
            Ok(())
        })
    }

    async fn get_bucket_by_id(&self, bucket_id: &str) -> Result<Option<Bucket>> {
        Ok(self.with_read(|inner| inner.buckets.get(bucket_id).cloned()))
    }

    async fn get_bucket_by_name(&self, name: &str) -> Result<Option<Bucket>> {
        Ok(self.with_read(|inner| {
            inner.buckets.values().find(|b| b.name == name).cloned()
        }))
    }

    async fn get_bucket_by_app_and_name(
        &self,
        app_id: &str,
        name: &str,
    ) -> Result<Option<Bucket>> {
        Ok(self.with_read(|inner| {
            inner
                .buckets
                .values()
                .find(|b| b.app_id == app_id && b.name == name)
                .cloned()
        }))
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
        Ok(self.with_read(|inner| {
            let mut buckets: Vec<Bucket> = inner
                .buckets
                .values()
                .filter(|b| b.app_id == app_id)
                .cloned()
                .collect();
            buckets.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then_with(|| a.bucket_id.cmp(&b.bucket_id))
            });
            buckets
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect()
        }))
    }

    async fn count_buckets_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(self.with_read(|inner| {
            inner.buckets.values().filter(|b| b.app_id == app_id).count() as i64
        }))
    }

    async fn set_bucket_public(&self, bucket_id: &str, is_public: bool) -> Result<bool> {
        Ok(self.with_write(|inner| {
            let Some(bucket) = inner.buckets.get_mut(bucket_id) else {
                return false;
            };
            bucket.is_public = is_public;
            true
        }))
    }

    async fn delete_bucket(&self, bucket_id: &str) -> Result<bool> {
        Ok(self.with_write(|inner| {
            if !inner.buckets.contains_key(bucket_id) {
                return false;
            }
            remove_bucket(inner, bucket_id, chrono::Utc::now().timestamp());
            true
        }))
    }

    async fn put_object(&self, object: &Object) -> Result<Option<Object>> {
        Ok(self.with_write(|inner| {
            let index_key = (object.bucket_id.clone(), object.key.clone());
            let previous = inner
                .object_keys
                .get(&index_key)
                .and_then(|id| inner.objects.get(id))
                .cloned();

            let mut previous = previous;
            let now = chrono::Utc::now().timestamp();
            if let Some(old) = previous.as_mut() {
                if old.object_id != object.object_id {
                    trash_object(inner, old, now);
                }
            }

            inner.objects.insert(object.object_id.clone(), object.clone());
            inner
                .object_keys
                .insert(index_key, object.object_id.clone());
            previous
        }))
    }

    async fn get_object_by_id(&self, object_id: &str) -> Result<Option<Object>> {
        Ok(self.with_read(|inner| {
            inner
                .objects
                .get(object_id)
                .filter(|o| o.deleted_at.is_none())
                .cloned()
        }))
    }

    async fn get_object_any(&self, object_id: &str) -> Result<Option<Object>> {
        Ok(self.with_read(|inner| inner.objects.get(object_id).cloned()))
    }

    async fn get_object_by_key(&self, bucket_id: &str, key: &str) -> Result<Option<Object>> {
        Ok(self.with_read(|inner| {
            inner
                .object_keys
                .get(&(bucket_id.to_string(), key.to_string()))
                .and_then(|id| inner.objects.get(id))
                .filter(|o| o.deleted_at.is_none())
                .cloned()
        }))
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
        Ok(self.with_read(|inner| {
            let mut objects: Vec<Object> = inner
                .object_keys
                .iter()
                .filter(|((bid, key), _)| {
                    bid == bucket_id
                        && key.starts_with(prefix)
                        && (marker.is_empty() || key.as_str() > marker)
                })
                .filter_map(|(_, id)| inner.objects.get(id))
                .cloned()
                .collect();
            objects.sort_by(|a, b| a.key.cmp(&b.key));
            objects.truncate(max_keys as usize);
            objects
        }))
    }

    async fn count_objects_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        Ok(self.with_read(|inner| {
            inner
                .object_keys
                .keys()
                .filter(|(bid, _)| bid == bucket_id)
                .count() as i64
        }))
    }

    async fn total_size_by_bucket(&self, bucket_id: &str) -> Result<i64> {
        Ok(self.with_read(|inner| {
            inner
                .object_keys
                .values()
                .filter_map(|id| inner.objects.get(id))
                .filter(|o| o.bucket_id == bucket_id)
                .map(|o| o.size)
                .sum()
        }))
    }

    async fn count_objects_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(self.with_read(|inner| {
            inner
                .objects
                .values()
                .filter(|o| o.deleted_at.is_none() && o.app_id == app_id)
                .count() as i64
        }))
    }

    async fn total_size_by_app(&self, app_id: &str) -> Result<i64> {
        Ok(self.with_read(|inner| {
            inner
                .objects
                .values()
                .filter(|o| o.deleted_at.is_none() && o.app_id == app_id)
                .map(|o| o.size)
                .sum()
        }))
    }

    async fn soft_delete_object(&self, object_id: &str, deleted_at: i64) -> Result<Option<Object>> {
        Ok(self.with_write(|inner| {
            let mut obj = inner.objects.get(object_id).cloned();
            if let Some(o) = obj.as_mut() {
                if o.deleted_at.is_none() {
                    trash_object(inner, o, deleted_at);
                }
            }
            obj
        }))
    }

    async fn purge_object(&self, object_id: &str) -> Result<Option<Object>> {
        Ok(self.with_write(|inner| {
            let obj = inner.objects.remove(object_id);
            if let Some(o) = &obj {
                if o.deleted_at.is_none() {
                    remove_active_index(inner, o);
                }
            }
            obj
        }))
    }

    async fn purge_objects(&self, object_ids: &[String]) -> Result<u64> {
        Ok(self.with_write(|inner| {
            let mut removed = 0u64;
            for id in object_ids {
                if let Some(obj) = inner.objects.remove(id) {
                    if obj.deleted_at.is_none() {
                        remove_active_index(inner, &obj);
                    }
                    removed += 1;
                }
            }
            removed
        }))
    }

    async fn restore_object(
        &self,
        object_id: &str,
        overwrite: bool,
        now: i64,
    ) -> Result<RestoreOutcome> {
        Ok(self.with_write(|inner| {
            let Some(mut obj) = inner.objects.get(object_id).cloned() else {
                return RestoreOutcome::NotFound;
            };
            if obj.deleted_at.is_none() {
                return RestoreOutcome::Restored(obj);
            }
            let target_bucket = obj
                .original_bucket_id
                .clone()
                .unwrap_or_else(|| obj.bucket_id.clone());
            let target_key = obj.original_key.clone().unwrap_or_else(|| obj.key.clone());
            if !inner.buckets.contains_key(&target_bucket) {
                return RestoreOutcome::BucketMissing {
                    bucket_id: target_bucket,
                };
            }

            let index_key = (target_bucket.clone(), target_key.clone());
            if let Some(occupant_id) = inner.object_keys.get(&index_key).cloned() {
                if occupant_id != obj.object_id {
                    if !overwrite {
                        return RestoreOutcome::Conflict {
                            existing_object_id: occupant_id,
                        };
                    }
                    if let Some(mut occupant) = inner.objects.get(&occupant_id).cloned() {
                        trash_object(inner, &mut occupant, now);
                    }
                }
            }

            obj.bucket_id = target_bucket;
            obj.key = target_key;
            obj.deleted_at = None;
            inner.objects.insert(obj.object_id.clone(), obj.clone());
            inner
                .object_keys
                .insert((obj.bucket_id.clone(), obj.key.clone()), obj.object_id.clone());
            RestoreOutcome::Restored(obj)
        }))
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
        Ok(self.with_read(|inner| {
            let mut items = trash_items(inner, app_id, bucket_id);
            items.sort_by(|a, b| {
                b.deleted_at
                    .cmp(&a.deleted_at)
                    .then_with(|| a.object_id.cmp(&b.object_id))
            });
            items
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect()
        }))
    }

    async fn count_trash(&self, app_id: Option<&str>, bucket_id: Option<&str>) -> Result<i64> {
        Ok(self.with_read(|inner| trash_items(inner, app_id, bucket_id).len() as i64))
    }

    async fn total_trash_size(
        &self,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64> {
        Ok(self.with_read(|inner| {
            trash_items(inner, app_id, bucket_id)
                .iter()
                .map(|o| o.size)
                .sum()
        }))
    }

    async fn list_expired_trash(&self, before: i64, limit: i64) -> Result<Vec<Object>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        Ok(self.with_read(|inner| {
            let mut items: Vec<Object> = inner
                .objects
                .values()
                .filter(|o| o.deleted_at.map(|ts| ts < before).unwrap_or(false))
                .cloned()
                .collect();
            items.sort_by(|a, b| {
                a.deleted_at
                    .cmp(&b.deleted_at)
                    .then_with(|| a.object_id.cmp(&b.object_id))
            });
            items.truncate(limit as usize);
            items
        }))
    }

    async fn list_all_objects(&self, offset: i64, limit: i64) -> Result<Vec<Object>> {
        if offset < 0 || limit <= 0 {
            return Ok(Vec::new());
        }
        Ok(self.with_read(|inner| {
            let mut items: Vec<Object> = inner.objects.values().cloned().collect();
            items.sort_by(|a, b| a.object_id.cmp(&b.object_id));
            items
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect()
        }))
    }

    async fn count_all_objects(&self) -> Result<i64> {
        Ok(self.with_read(|inner| inner.objects.len() as i64))
    }

    async fn save_maintenance_run(&self, run: &MaintenanceRun) -> Result<()> {
        self.with_write(|inner| {
            inner
                .maintenance_runs
                .insert(run.run_id.clone(), run.clone());
        });
        Ok(())
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
        if limit <= 0 {
            return Ok(Vec::new());
        }
        Ok(self.with_read(|inner| {
            let mut runs: Vec<MaintenanceRun> = inner
                .maintenance_runs
                .values()
                .filter(|r| r.kind == kind)
                .cloned()
                .collect();
            runs.sort_by(|a, b| {
                b.started_at
                    .cmp(&a.started_at)
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            runs.truncate(limit as usize);
            runs
        }))
    }

    async fn insert_nonce(&self, app_id: &str, nonce: &str, created_at: i64) -> Result<bool> {
        Ok(self.with_write(|inner| {
            let key = (app_id.to_string(), nonce.to_string());
            if inner.nonces.contains_key(&key) {
                return false;
            }
            inner.nonces.insert(key, created_at);
            true
        }))
    }

    async fn purge_nonces_before(&self, before: i64) -> Result<u64> {
        Ok(self.with_write(|inner| {
            let stale: Vec<(String, String)> = inner
                .nonces
                .iter()
                .filter(|(_, ts)| **ts < before)
                .map(|(k, _)| k.clone())
                .collect();
            for key in &stale {
                inner.nonces.remove(key);
            }
            stale.len() as u64
        }))
    }
}

/// 把对象移入回收站（保留磁盘文件），并移除 active 索引。
fn trash_object(inner: &mut Inner, obj: &mut Object, now: i64) {
    if obj.deleted_at.is_some() {
        return;
    }
    obj.deleted_at = Some(now);
    obj.original_bucket_id = Some(obj.bucket_id.clone());
    obj.original_key = Some(obj.key.clone());
    let index_key = (obj.bucket_id.clone(), obj.key.clone());
    if inner
        .object_keys
        .get(&index_key)
        .map(|id| id == &obj.object_id)
        .unwrap_or(false)
    {
        inner.object_keys.remove(&index_key);
    }
    inner.objects.insert(obj.object_id.clone(), obj.clone());
}

fn remove_active_index(inner: &mut Inner, obj: &Object) {
    let index_key = (obj.bucket_id.clone(), obj.key.clone());
    if inner
        .object_keys
        .get(&index_key)
        .map(|id| id == &obj.object_id)
        .unwrap_or(false)
    {
        inner.object_keys.remove(&index_key);
    }
}

fn trash_items(inner: &Inner, app_id: Option<&str>, bucket_id: Option<&str>) -> Vec<Object> {
    inner
        .objects
        .values()
        .filter(|o| o.deleted_at.is_some())
        .filter(|o| app_id.map(|a| o.app_id == a).unwrap_or(true))
        .filter(|o| {
            bucket_id
                .map(|b| o.bucket_id == b || o.original_bucket_id.as_deref() == Some(b))
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// 删除桶：先把它下面的 active 对象移入回收站，再删除桶记录。
fn remove_bucket(inner: &mut Inner, bucket_id: &str, now: i64) {
    let object_ids: Vec<String> = inner
        .object_keys
        .keys()
        .filter(|(bid, _)| bid == bucket_id)
        .filter_map(|k| inner.object_keys.get(k).cloned())
        .collect();
    for id in object_ids {
        if let Some(mut obj) = inner.objects.get(&id).cloned() {
            trash_object(inner, &mut obj, now);
        }
    }
    inner.buckets.remove(bucket_id);
}
