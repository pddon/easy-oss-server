//! Object (file) model。
//!
//! 持久化通过 `store::Store` 抽象完成；本模块只保留业务规则、校验与转换。
//!
//! # 生命周期
//!
//! - **active**：`deleted_at == None`，出现在正常列表 / 按 key 查询 / 计数中。
//! - **trashed（回收站）**：`deleted_at == Some(ts)`，元数据与磁盘文件都保留，
//!   不参与正常列表与计数，只出现在回收站视图。
//! - **purged（彻底删除）**：元数据行被删除，磁盘文件由调用方（或清理任务）
//!   删除。这是唯一会真正释放磁盘空间的动作。

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::store::{RestoreOutcome, StoreHandle};

/// A stored object (file).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Object {
    /// Unique object ID (UUID, used as filename on disk).
    pub object_id: String,
    /// Owning app ID. Stored on the object itself so trash entries survive
    /// bucket / app deletion.
    pub app_id: String,
    /// ID of the containing bucket (may reference a deleted bucket for
    /// objects that were already in the trash when the bucket was removed).
    pub bucket_id: String,
    /// Object key (path within bucket).
    pub key: String,
    /// File size in bytes.
    pub size: i64,
    /// MIME content type.
    pub content_type: String,
    /// MD5 ETag (hex string).
    pub etag: String,
    /// Creation timestamp (unix timestamp in seconds).
    pub created_at: i64,
    /// Trash timestamp: `None` means active.
    pub deleted_at: Option<i64>,
    /// Original bucket ID recorded when the object entered the trash.
    pub original_bucket_id: Option<String>,
    /// Original key recorded when the object entered the trash.
    pub original_key: Option<String>,
}

impl Object {
    /// Create a new object record.
    pub async fn create(
        store: &StoreHandle,
        bucket_id: &str,
        key: &str,
        size: i64,
        content_type: &str,
        etag: &str,
    ) -> Result<Self> {
        Self::create_with_id(
            store,
            &Uuid::new_v4().to_string(),
            bucket_id,
            key,
            size,
            content_type,
            etag,
        )
        .await
    }

    /// Create an object record whose `object_id` is supplied by the caller.
    ///
    /// The upload handlers generate this ID before writing the file to disk,
    /// so the DB record and the on-disk filename must use the same value.
    ///
    /// `app_id` is resolved from the bucket so that trash entries remain
    /// attributable after the bucket or app is deleted.
    pub async fn create_with_id(
        store: &StoreHandle,
        object_id: &str,
        bucket_id: &str,
        key: &str,
        size: i64,
        content_type: &str,
        etag: &str,
    ) -> Result<Self> {
        let bucket = store
            .get_bucket_by_id(bucket_id)
            .await?
            .ok_or_else(|| anyhow!("Bucket '{}' does not exist", bucket_id))?;

        let obj = Self {
            object_id: object_id.to_string(),
            app_id: bucket.app_id,
            bucket_id: bucket_id.to_string(),
            key: key.to_string(),
            size,
            content_type: content_type.to_string(),
            etag: etag.to_string(),
            created_at: Utc::now().timestamp(),
            deleted_at: None,
            original_bucket_id: None,
            original_key: None,
        };

        // 后端在同一原子操作内完成 (bucket_id, key) 的 upsert；
        // 被覆盖的旧版本会进入回收站而不是被删除。
        store.put_object(&obj).await?;
        Ok(obj)
    }

    /// Get an active object by bucket_id and key.
    pub async fn get_by_bucket_and_key(
        store: &StoreHandle,
        bucket_id: &str,
        key: &str,
    ) -> Result<Option<Self>> {
        store.get_object_by_key(bucket_id, key).await
    }

    /// Get an active object by ID.
    pub async fn get_by_id(store: &StoreHandle, object_id: &str) -> Result<Option<Self>> {
        store.get_object_by_id(object_id).await
    }

    /// List active objects in a bucket with prefix, marker, and max-keys.
    ///
    /// Results are ordered by key ascending.
    pub async fn list(
        store: &StoreHandle,
        bucket_id: &str,
        prefix: &str,
        marker: &str,
        max_keys: i64,
    ) -> Result<Vec<Self>> {
        store
            .list_objects(bucket_id, prefix, marker, max_keys)
            .await
    }

    /// Move an object into the trash. The disk file is **never** touched here.
    pub async fn delete(store: &StoreHandle, object_id: &str) -> Result<bool> {
        Ok(store
            .soft_delete_object(object_id, Utc::now().timestamp())
            .await?
            .is_some())
    }

    /// Permanently delete metadata. The caller is responsible for deleting the
    /// disk file (that is the only operation that frees space).
    pub async fn purge(store: &StoreHandle, object_id: &str) -> Result<Option<Self>> {
        store.purge_object(object_id).await
    }

    /// Restore a trashed object to its original bucket/key.
    pub async fn restore(
        store: &StoreHandle,
        object_id: &str,
        overwrite: bool,
    ) -> Result<RestoreOutcome> {
        store
            .restore_object(object_id, overwrite, Utc::now().timestamp())
            .await
    }

    /// List trash entries, optionally filtered by app and/or bucket.
    ///
    /// Ordering contract: `deleted_at DESC`, then `object_id ASC`.
    pub async fn list_trash(
        store: &StoreHandle,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Self>> {
        store
            .list_trash(app_id, bucket_id, offset, limit)
            .await
    }

    /// Count trash entries matching the filters.
    pub async fn count_trash(
        store: &StoreHandle,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64> {
        store.count_trash(app_id, bucket_id).await
    }

    /// Total size of trash entries matching the filters.
    pub async fn total_trash_size(
        store: &StoreHandle,
        app_id: Option<&str>,
        bucket_id: Option<&str>,
    ) -> Result<i64> {
        store.total_trash_size(app_id, bucket_id).await
    }

    /// Count objects in a bucket (active only).
    pub async fn count_by_bucket(store: &StoreHandle, bucket_id: &str) -> Result<i64> {
        store.count_objects_by_bucket(bucket_id).await
    }

    /// Total size of all active objects in a bucket.
    pub async fn total_size_by_bucket(store: &StoreHandle, bucket_id: &str) -> Result<i64> {
        store.total_size_by_bucket(bucket_id).await
    }

    /// Total size of all active objects in an app.
    pub async fn total_size_by_app(store: &StoreHandle, app_id: &str) -> Result<i64> {
        store.total_size_by_app(app_id).await
    }

    /// Count active objects in an app.
    pub async fn count_by_app(store: &StoreHandle, app_id: &str) -> Result<i64> {
        store.count_objects_by_app(app_id).await
    }

    /// Whether this record is in the trash.
    pub fn is_trashed(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Get the created_at as a DateTime.
    pub fn created_at_dt(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_default()
    }

    /// Get the trash timestamp as a DateTime, if any.
    pub fn deleted_at_dt(&self) -> Option<DateTime<Utc>> {
        self.deleted_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
    }
}

/// Validate an object key.
pub fn validate_object_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 1024 {
        return false;
    }
    if key.starts_with('/') {
        return false;
    }
    // 允许中文、空格等 UTF-8 字符（S3 对象名语义），但仍拒绝空路径段与 "." / ".."。
    if key
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return false;
    }
    key.chars().all(|c| {
        !c.is_control() && !matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    })
}

/// Get the storage path prefix for an object key (first two directory levels for sharding).
pub fn key_prefix(key: &str) -> String {
    let parts: Vec<&str> = key.splitn(3, '/').collect();
    match parts.len() {
        1 => "root".to_string(),
        2 => parts[0].to_string(),
        _ => format!("{}/{}", parts[0], parts[1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::app::App;
    use crate::models::bucket::Bucket;
    use crate::store::{init_test_store, StoreHandle};

    const TEST_MASTER_KEY: &str = "test-master-key-for-object-tests";

    async fn setup(pool: &StoreHandle) -> String {
        let (app, _) = App::create(pool, "test-app", TEST_MASTER_KEY)
            .await
            .unwrap();
        let bucket = Bucket::create(pool, &app.app_id, "test-bucket", false)
            .await
            .unwrap();
        bucket.bucket_id
    }

    #[tokio::test]
    async fn test_create_and_get_object() {
        let pool = init_test_store();
        let bucket_id = setup(&pool).await;

        let obj = Object::create(
            &pool,
            &bucket_id,
            "photos/test.jpg",
            1024,
            "image/jpeg",
            "abc123def456",
        )
        .await
        .unwrap();

        assert_eq!(obj.key, "photos/test.jpg");
        assert_eq!(obj.size, 1024);
        assert!(!obj.is_trashed());

        let fetched = Object::get_by_bucket_and_key(&pool, &bucket_id, "photos/test.jpg")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.object_id, obj.object_id);
    }

    #[tokio::test]
    async fn test_list_objects() {
        let pool = init_test_store();
        let bucket_id = setup(&pool).await;

        let keys = ["a/1.txt", "a/2.txt", "b/1.txt", "b/c/3.txt", "root.txt"];
        for k in &keys {
            Object::create(&pool, &bucket_id, k, 100, "text/plain", "etag")
                .await
                .unwrap();
        }

        // List all
        let all = Object::list(&pool, &bucket_id, "", "", 100).await.unwrap();
        assert_eq!(all.len(), 5);

        // Prefix filter
        let prefixed = Object::list(&pool, &bucket_id, "a/", "", 100)
            .await
            .unwrap();
        assert_eq!(prefixed.len(), 2);

        // Marker
        let marked = Object::list(&pool, &bucket_id, "", "a/1.txt", 100)
            .await
            .unwrap();
        assert_eq!(marked.len(), 4);
    }

    #[tokio::test]
    async fn test_delete_object() {
        let pool = init_test_store();
        let bucket_id = setup(&pool).await;

        let obj = Object::create(&pool, &bucket_id, "to-delete.txt", 100, "text/plain", "etag")
            .await
            .unwrap();

        let deleted = Object::delete(&pool, &obj.object_id).await.unwrap();
        assert!(deleted);

        // 普通删除只进入回收站：按 ID 查询（active 语义）查不到。
        let fetched = Object::get_by_id(&pool, &obj.object_id).await.unwrap();
        assert!(fetched.is_none());

        // 但回收站里仍能看到，且记录了移入时间与原 key。
        let trash = Object::list_trash(&pool, None, None, 0, 100).await.unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].object_id, obj.object_id);
        assert!(trash[0].deleted_at.is_some());
        assert_eq!(trash[0].original_key.as_deref(), Some("to-delete.txt"));
    }

    #[tokio::test]
    async fn test_stats() {
        let pool = init_test_store();
        let (app, _) = App::create(&pool, "stats-app", TEST_MASTER_KEY)
            .await
            .unwrap();
        let bucket = Bucket::create(&pool, &app.app_id, "stats-bucket", false)
            .await
            .unwrap();

        for i in 0..10 {
            Object::create(
                &pool,
                &bucket.bucket_id,
                &format!("file-{}.txt", i),
                100,
                "text/plain",
                "etag",
            )
            .await
            .unwrap();
        }

        let count = Object::count_by_bucket(&pool, &bucket.bucket_id)
            .await
            .unwrap();
        assert_eq!(count, 10);

        let total = Object::total_size_by_bucket(&pool, &bucket.bucket_id)
            .await
            .unwrap();
        assert_eq!(total, 1000);

        let app_total = Object::total_size_by_app(&pool, &app.app_id)
            .await
            .unwrap();
        assert_eq!(app_total, 1000);
    }

    #[test]
    fn test_validate_object_key() {
        assert!(validate_object_key("hello.txt"));
        assert!(validate_object_key("path/to/file.txt"));
        assert!(validate_object_key("a/b/c/d/e.txt"));
        assert!(validate_object_key("file-name_1.2.3.txt"));
        assert!(validate_object_key("ABC/DEF/file.TXT"));
        // 中文、空格等 UTF-8 字符与 S3 语义一致，允许作为对象名。
        assert!(validate_object_key("路径/中文 文件名.txt"));
        assert!(validate_object_key("path/to file.txt"));

        assert!(!validate_object_key(""));
        assert!(!validate_object_key("/absolute/path"));
        assert!(!validate_object_key("../escape"));
        assert!(!validate_object_key("path/../escape"));
        assert!(!validate_object_key("path//double-slash.txt"));
        assert!(!validate_object_key("path/./current.txt"));
        assert!(!validate_object_key("path/to\\file.txt"));
        assert!(!validate_object_key(&"a".repeat(1025)));
    }

    #[test]
    fn test_key_prefix() {
        assert_eq!(key_prefix("file.txt"), "root");
        assert_eq!(key_prefix("dir/file.txt"), "dir");
        assert_eq!(key_prefix("dir/sub/file.txt"), "dir/sub");
        assert_eq!(key_prefix("a/b/c/d/e.txt"), "a/b");
    }
}
