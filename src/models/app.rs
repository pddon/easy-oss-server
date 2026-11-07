//! Application model.
//!
//! App secrets are encrypted at rest using AES-256-GCM with a master key.
//! 持久化通过 `store::Store` 抽象完成，本模块只负责业务规则与加密。

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::services::crypto::{decrypt_secret, encrypt_secret};
use crate::store::StoreHandle;

/// An application (tenant) that owns buckets and objects.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct App {
    /// Unique application ID.
    pub app_id: String,
    /// Encrypted app secret (base64-encoded AES-GCM ciphertext).
    pub app_secret_encrypted: String,
    /// Display name of the app.
    pub name: String,
    /// Creation timestamp (unix timestamp in seconds).
    pub created_at: i64,
}

impl App {
    /// Create a new app with a random app_id and app_secret.
    /// Returns (app, plaintext_secret).
    pub async fn create(
        store: &StoreHandle,
        name: &str,
        master_key: &str,
    ) -> Result<(Self, String)> {
        let app_id = Uuid::new_v4().to_string();
        let app_secret = generate_secret();
        let app_secret_encrypted = encrypt_secret(master_key, &app_secret);

        let app = Self {
            app_id,
            app_secret_encrypted,
            name: name.to_string(),
            created_at: Utc::now().timestamp(),
        };

        store.insert_app(&app).await?;
        Ok((app, app_secret))
    }

    /// Get an app by ID.
    pub async fn get_by_id(store: &StoreHandle, app_id: &str) -> Result<Option<Self>> {
        store.get_app(app_id).await
    }

    /// List apps with pagination (ordered by created_at desc).
    pub async fn list(store: &StoreHandle, offset: i64, limit: i64) -> Result<Vec<Self>> {
        store.list_apps(offset, limit).await
    }

    /// Count total apps.
    pub async fn count(store: &StoreHandle) -> Result<i64> {
        store.count_apps().await
    }

    /// Delete an app and all its buckets/objects.
    pub async fn delete(store: &StoreHandle, app_id: &str) -> Result<bool> {
        store.delete_app(app_id).await
    }

    /// Reset the app secret, returning the new plaintext secret.
    pub async fn reset_secret(
        store: &StoreHandle,
        app_id: &str,
        master_key: &str,
    ) -> Result<Option<String>> {
        let Some(_app) = store.get_app(app_id).await? else {
            return Ok(None);
        };
        let new_secret = generate_secret();
        let encrypted = encrypt_secret(master_key, &new_secret);
        if !store.update_app_secret(app_id, &encrypted).await? {
            return Ok(None);
        }
        Ok(Some(new_secret))
    }

    /// Decrypt and return the plaintext app secret.
    pub fn secret(&self, master_key: &str) -> Result<String, String> {
        decrypt_secret(master_key, &self.app_secret_encrypted)
    }

    /// Get the created_at as a DateTime.
    pub fn created_at_dt(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.created_at, 0).unwrap_or_default()
    }
}

/// Generate a random app secret (32 bytes, base64 URL-safe encoded).
pub fn generate_secret() -> String {
    let bytes: [u8; 32] = rand_bytes();
    base64_encode_urlsafe(&bytes)
}

fn rand_bytes() -> [u8; 32] {
    let u1 = Uuid::new_v4();
    let u2 = Uuid::new_v4();
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(u1.as_bytes());
    bytes[16..].copy_from_slice(u2.as_bytes());
    bytes
}

fn base64_encode_urlsafe(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::init_test_store;

    const MASTER_KEY: &str = "test-master-key-for-unit-tests";

    #[tokio::test]
    async fn test_create_and_get_app() {
        let pool = init_test_store();
        let (app, secret) = App::create(&pool, "test-app", MASTER_KEY).await.unwrap();

        assert!(!app.app_id.is_empty());
        assert!(!secret.is_empty());
        assert_eq!(app.name, "test-app");
        let decrypted = app.secret(MASTER_KEY).unwrap();
        assert_eq!(decrypted, secret);

        let fetched = App::get_by_id(&pool, &app.app_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.app_id, app.app_id);
        assert_eq!(fetched.name, "test-app");
        assert_eq!(fetched.secret(MASTER_KEY).unwrap(), secret);
    }

    #[tokio::test]
    async fn test_list_apps() {
        let pool = init_test_store();
        for i in 0..5 {
            App::create(&pool, &format!("app-{}", i), MASTER_KEY)
                .await
                .unwrap();
        }

        let apps = App::list(&pool, 0, 3).await.unwrap();
        assert_eq!(apps.len(), 3);

        let apps2 = App::list(&pool, 3, 10).await.unwrap();
        assert_eq!(apps2.len(), 2);
    }

    #[tokio::test]
    async fn test_count_apps() {
        let pool = init_test_store();
        assert_eq!(App::count(&pool).await.unwrap(), 0);

        App::create(&pool, "a1", MASTER_KEY).await.unwrap();
        App::create(&pool, "a2", MASTER_KEY).await.unwrap();
        assert_eq!(App::count(&pool).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_delete_app() {
        let pool = init_test_store();
        let (app, _) = App::create(&pool, "to-delete", MASTER_KEY).await.unwrap();

        let deleted = App::delete(&pool, &app.app_id).await.unwrap();
        assert!(deleted);

        let fetched = App::get_by_id(&pool, &app.app_id).await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn test_reset_secret() {
        let pool = init_test_store();
        let (app, old_secret) = App::create(&pool, "reset-test", MASTER_KEY).await.unwrap();

        let new_secret = App::reset_secret(&pool, &app.app_id, MASTER_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(new_secret, old_secret);

        let fetched = App::get_by_id(&pool, &app.app_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.secret(MASTER_KEY).unwrap(), new_secret);
    }

    #[test]
    fn test_generate_secret_unique() {
        let s1 = generate_secret();
        let s2 = generate_secret();
        assert_ne!(s1, s2);
        assert!(s1.len() > 30);
    }

    #[test]
    fn test_secret_encryption() {
        let secret = "my-plain-secret-123";
        let encrypted = crate::services::crypto::encrypt_secret(MASTER_KEY, secret);
        assert_ne!(encrypted, secret);
        let decrypted = crate::services::crypto::decrypt_secret(MASTER_KEY, &encrypted).unwrap();
        assert_eq!(decrypted, secret);
    }
}
