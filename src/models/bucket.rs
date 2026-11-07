//! Bucket model。持久化通过 `store::Store` 抽象完成。

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::store::StoreHandle;

/// A storage bucket owned by an app.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bucket {
    /// Unique bucket ID.
    pub bucket_id: String,
    /// ID of the owning app.
    pub app_id: String,
    /// Bucket name (globally unique).
    pub name: String,
    /// Whether the bucket is publicly readable.
    pub is_public: bool,
    /// Creation timestamp (unix timestamp in seconds).
    pub created_at: i64,
}

impl Bucket {
    /// Create a new bucket.
    pub async fn create(
        store: &StoreHandle,
        app_id: &str,
        name: &str,
        is_public: bool,
    ) -> Result<Self> {
        let bucket = Self {
            bucket_id: Uuid::new_v4().to_string(),
            app_id: app_id.to_string(),
            name: name.to_string(),
            is_public,
            created_at: Utc::now().timestamp(),
        };

        // 名称唯一性由后端在写入事务内原子校验。
        store.insert_bucket(&bucket).await?;
        Ok(bucket)
    }

    /// Get a bucket by ID.
    pub async fn get_by_id(store: &StoreHandle, bucket_id: &str) -> Result<Option<Self>> {
        store.get_bucket_by_id(bucket_id).await
    }

    /// Get a bucket by name (globally unique).
    pub async fn get_by_name(store: &StoreHandle, name: &str) -> Result<Option<Self>> {
        store.get_bucket_by_name(name).await
    }

    /// Get a bucket by app_id and name.
    pub async fn get_by_app_and_name(
        store: &StoreHandle,
        app_id: &str,
        name: &str,
    ) -> Result<Option<Self>> {
        store.get_bucket_by_app_and_name(app_id, name).await
    }

    /// List buckets for an app with pagination (created_at desc).
    pub async fn list_by_app(
        store: &StoreHandle,
        app_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Self>> {
        store.list_buckets_by_app(app_id, offset, limit).await
    }

    /// Count buckets for an app.
    pub async fn count_by_app(store: &StoreHandle, app_id: &str) -> Result<i64> {
        store.count_buckets_by_app(app_id).await
    }

    /// Delete a bucket and all its objects.
    pub async fn delete(store: &StoreHandle, bucket_id: &str) -> Result<bool> {
        store.delete_bucket(bucket_id).await
    }

    /// Set bucket visibility.
    pub async fn set_public(
        store: &StoreHandle,
        bucket_id: &str,
        is_public: bool,
    ) -> Result<bool> {
        store.set_bucket_public(bucket_id, is_public).await
    }

    /// Get the created_at as a DateTime.
    pub fn created_at_dt(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_default()
    }
}

/// Validate a bucket name.
///
/// Rules:
/// - 3-63 characters
/// - Lowercase letters, numbers, and hyphens
/// - Must start and end with a letter or number
pub fn validate_bucket_name(name: &str) -> bool {
    if name.len() < 3 || name.len() > 63 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    let last = name.chars().last().unwrap();

    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    if !last.is_ascii_lowercase() && !last.is_ascii_digit() {
        return false;
    }

    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::init_test_store;
    use crate::models::app::App;

    const TEST_MASTER_KEY: &str = "test-master-key-for-bucket-tests";

    async fn setup_app(pool: &crate::store::StoreHandle) -> String {
        let (app, _) = App::create(pool, "test-app", TEST_MASTER_KEY)
            .await
            .unwrap();
        app.app_id
    }

    #[tokio::test]
    async fn test_create_and_get_bucket() {
        let pool = init_test_store();
        let app_id = setup_app(&pool).await;

        let bucket = Bucket::create(&pool, &app_id, "my-bucket", false)
            .await
            .unwrap();
        assert_eq!(bucket.name, "my-bucket");
        assert!(!bucket.is_public);

        let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.bucket_id, bucket.bucket_id);
        assert_eq!(fetched.app_id, app_id);
    }

    #[tokio::test]
    async fn test_get_by_app_and_name() {
        let pool = init_test_store();
        let app_id = setup_app(&pool).await;

        Bucket::create(&pool, &app_id, "test-bucket", true)
            .await
            .unwrap();

        let fetched = Bucket::get_by_app_and_name(&pool, &app_id, "test-bucket")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.name, "test-bucket");
        assert!(fetched.is_public);
    }

    #[tokio::test]
    async fn test_list_buckets() {
        let pool = init_test_store();
        let app_id = setup_app(&pool).await;

        for i in 0..5 {
            Bucket::create(&pool, &app_id, &format!("bucket-{}", i), false)
                .await
                .unwrap();
        }

        let buckets = Bucket::list_by_app(&pool, &app_id, 0, 3).await.unwrap();
        assert_eq!(buckets.len(), 3);
    }

    #[tokio::test]
    async fn test_delete_bucket() {
        let pool = init_test_store();
        let app_id = setup_app(&pool).await;

        let bucket = Bucket::create(&pool, &app_id, "to-delete", false)
            .await
            .unwrap();
        let deleted = Bucket::delete(&pool, &bucket.bucket_id).await.unwrap();
        assert!(deleted);

        let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id).await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn test_set_public() {
        let pool = init_test_store();
        let app_id = setup_app(&pool).await;

        let bucket = Bucket::create(&pool, &app_id, "pub-test", false)
            .await
            .unwrap();

        Bucket::set_public(&pool, &bucket.bucket_id, true)
            .await
            .unwrap();

        let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id)
            .await
            .unwrap()
            .unwrap();
        assert!(fetched.is_public);
    }

    #[test]
    fn test_validate_bucket_name() {
        assert!(validate_bucket_name("my-bucket"));
        assert!(validate_bucket_name("abc"));
        assert!(validate_bucket_name("a1-b2-c3"));
        assert!(validate_bucket_name("test-123-bucket"));

        assert!(!validate_bucket_name("ab")); // too short
        assert!(!validate_bucket_name(&"a".repeat(64))); // too long
        assert!(!validate_bucket_name("My-Bucket")); // uppercase
        assert!(!validate_bucket_name("-mybucket")); // starts with hyphen
        assert!(!validate_bucket_name("mybucket-")); // ends with hyphen
        assert!(!validate_bucket_name("my_bucket")); // underscore
        assert!(!validate_bucket_name("")); // empty
    }
}
