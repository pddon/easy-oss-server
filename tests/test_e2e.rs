//! End-to-end tests with a running server.

use std::net::SocketAddr;
use std::sync::Arc;

use oss_server::config::Config;
use oss_server::store::init_test_store;
use oss_server::services::storage::StorageService;
use oss_server::build_router;
use tempfile::tempdir;

/// Helper to create a test server and return its address.
async fn setup_test_server() -> (SocketAddr, tempfile::TempDir, String) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_test_writer()
        .try_init();

    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");

    let config = Arc::new(Config {
        host: "127.0.0.1:0".to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
        storage_backend: oss_server::store::Backend::Memory,
        db_url: "memory://in-memory".to_string(),
        admin_token: "test-admin-token".to_string(),
        master_key: "test-master-key-for-e2e".to_string(),
        public_base_url: "http://localhost".to_string(),
        signature_expire_seconds: 900,
        max_upload_size: 10 * 1024 * 1024, // 10MB for tests
        cache_size_bytes: 0,
        cors_allowed_origins: vec![],
        cors_enabled: false,
        trash_retention_days: 30,
        cleanup_interval_minutes: 30,
        cleanup_max_objects: 1000,
        cleanup_max_seconds: 60,
        startup_check_mode: oss_server::config::StartupCheckMode::Report,
        nonce_retention_seconds: 3600,
    });

    let pool = init_test_store();
    let storage = StorageService::new(&data_dir);
    storage.init().await.unwrap();

    let app = build_router(config, pool.clone(), storage.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (addr, dir, "test-admin-token".to_string())
}

fn base_url(addr: SocketAddr) -> String {
    format!("http://{}", addr)
}

#[tokio::test]
async fn test_e2e_admin_app_crud() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Create app
    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "e2e-test-app"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap();
    let app_secret = body["app_secret"].as_str().unwrap();
    assert!(!app_id.is_empty());
    assert!(!app_secret.is_empty());

    // List apps
    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "e2e-test-app");

    // Get app
    let resp = client
        .get(format!("{}/api/admin/apps/{}", base_url(addr), app_id))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Reset secret
    let resp = client
        .post(format!(
            "{}/api/admin/apps/{}/reset-secret",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let new_secret = body["app_secret"].as_str().unwrap();
    assert_ne!(new_secret, app_secret);

    // Delete app
    let resp = client
        .delete(format!("{}/api/admin/apps/{}", base_url(addr), app_id))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify deleted
    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn test_e2e_admin_unauthorized() {
    let (addr, _dir, _) = setup_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_e2e_admin_wrong_token() {
    let (addr, _dir, _) = setup_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", "wrong-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_e2e_bucket_and_object_flow() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Create app
    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "bucket-test"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap();

    // Create bucket
    let resp = client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "e2e-bucket", "is_public": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Upload file via admin API
    let file_content = b"Hello, OSS E2E test!";
    let resp = client
        .put(format!(
            "{}/api/admin/buckets/e2e-bucket/objects/test.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .header("Content-Type", "text/plain")
        .body(file_content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Download file
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/e2e-bucket/objects/test.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), file_content);

    // List objects
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/e2e-bucket/objects",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    assert_eq!(body["contents"][0]["key"], "test.txt");

    // Delete file
    let resp = client
        .delete(format!(
            "{}/api/admin/buckets/e2e-bucket/objects/test.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn test_e2e_public_bucket_access() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Create app + public bucket
    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "public-test"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap();

    // Create public bucket
    let resp = client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "public-bucket", "is_public": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Upload file
    let content = b"public file content";
    client
        .put(format!(
            "{}/api/admin/buckets/public-bucket/objects/public.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .header("Content-Type", "text/plain")
        .body(content.to_vec())
        .send()
        .await
        .unwrap();

    // Download without auth (public access)
    let resp = client
        .get(format!("{}/public-bucket/public.txt", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), content);
}

#[tokio::test]
async fn test_e2e_private_bucket_denied() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Create app + private bucket
    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "private-test"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap();

    // Create private bucket
    client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "private-bucket", "is_public": false}))
        .send()
        .await
        .unwrap();

    // Upload file
    client
        .put(format!(
            "{}/api/admin/buckets/private-bucket/objects/secret.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .body(b"secret data".to_vec())
        .send()
        .await
        .unwrap();

    // Try to download without auth (should be denied)
    let resp = client
        .get(format!("{}/private-bucket/secret.txt", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_e2e_signed_url_private_bucket() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "signed-url-app"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap().to_string();
    let app_secret = body["app_secret"].as_str().unwrap().to_string();

    client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "signed-bucket", "is_public": false}))
        .send()
        .await
        .unwrap();

    client
        .put(format!(
            "{}/api/admin/buckets/signed-bucket/objects/signed.txt",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .body(b"signed content".to_vec())
        .send()
        .await
        .unwrap();

    let expires = chrono::Utc::now().timestamp() + 600;
    let string_to_sign = format!("GET\n\n\n{expires}\n/signed-bucket/signed.txt");
    let signature = oss_server::services::signature::compute_signature(&app_secret, &string_to_sign);
    let signature_encoded = urlencoding::encode(&signature);

    let resp = client
        .get(format!(
            "{}/signed-bucket/signed.txt?OSSAccessKeyId={}&Expires={}&Signature={}",
            base_url(addr),
            app_id,
            expires,
            signature_encoded
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"signed content");
}

#[tokio::test]
async fn test_e2e_admin_ui() {
    let (addr, _dir, _admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/admin/", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("<html") || body.contains("<!DOCTYPE"));

    let resp = client
        .get(format!("{}/admin/style.css", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .map(|v| v.to_str().unwrap()),
        Some("nosniff")
    );
}

#[tokio::test]
async fn test_e2e_range_request() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Setup
    client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "range-test"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["items"][0]["app_id"].as_str().unwrap();

    client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "range-bucket", "is_public": true}))
        .send()
        .await
        .unwrap();

    // Upload 100 bytes
    let data: Vec<u8> = (0..100u8).collect();
    client
        .put(format!(
            "{}/api/admin/buckets/range-bucket/objects/range.bin",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .body(data)
        .send()
        .await
        .unwrap();

    // Range request: bytes=10-19
    let resp = client
        .get(format!("{}/range-bucket/range.bin", base_url(addr)))
        .header("Range", "bytes=10-19")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 206);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 10);
    assert_eq!(body.as_ref(), &(10..20u8).collect::<Vec<_>>());
}

#[tokio::test]
async fn test_e2e_invalid_bucket_name() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "inv-app"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap();

    // Invalid bucket name (uppercase)
    let resp = client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "Invalid-Bucket"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_e2e_path_traversal_rejected() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Setup app + bucket
    client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "traversal-test"}))
        .send()
        .await
        .unwrap();
    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["items"][0]["app_id"].as_str().unwrap();

    client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "trav-bucket", "is_public": false}))
        .send()
        .await
        .unwrap();

    // Try to upload with path traversal key
    let resp = client
        .put(format!(
            "{}/api/admin/buckets/trav-bucket/objects/../../../etc/passwd",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .body(b"bad".to_vec())
        .send()
        .await
        .unwrap();
    // The URL path normalization by axum may already strip .. before it reaches handler
    // But our validation would also catch it
    assert!(resp.status().is_client_error());
}

#[tokio::test]
async fn test_e2e_concurrent_uploads() {
    let (addr, _dir, admin_token) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Setup
    client
        .post(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "concurrent-test"}))
        .send()
        .await
        .unwrap();
    let resp = client
        .get(format!("{}/api/admin/apps", base_url(addr)))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["items"][0]["app_id"].as_str().unwrap();

    client
        .post(format!(
            "{}/api/admin/apps/{}/buckets",
            base_url(addr),
            app_id
        ))
        .header("X-Admin-Token", &admin_token)
        .json(&serde_json::json!({"name": "concurrent-bucket", "is_public": false}))
        .send()
        .await
        .unwrap();

    // Upload 10 files concurrently
    let mut handles = vec![];
    for i in 0..10 {
        let c = client.clone();
        let url = format!(
            "{}/api/admin/buckets/concurrent-bucket/objects/file-{}.txt",
            base_url(addr),
            i
        );
        let token = admin_token.clone();
        let content = format!("content of file {}", i);

        handles.push(tokio::spawn(async move {
            c.put(&url)
                .header("X-Admin-Token", &token)
                .header("Content-Type", "text/plain")
                .body(content.into_bytes())
                .send()
                .await
                .unwrap()
                .status()
        }));
    }

    let results = futures_util::future::join_all(handles).await;
    for r in results {
        assert_eq!(r.unwrap(), 200);
    }

    // Verify all files exist
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/concurrent-bucket/objects?max-keys=100",
            base_url(addr)
        ))
        .header("X-Admin-Token", &admin_token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["contents"].as_array().unwrap().len(), 10);
}
