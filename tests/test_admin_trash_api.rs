//! 管理端回收站 / 维护接口的端到端测试（memory 后端 + 真实 HTTP + 真实磁盘）。

use std::net::SocketAddr;
use std::sync::Arc;

use oss_server::build_router;
use oss_server::config::{Config, StartupCheckMode};
use oss_server::services::storage::StorageService;
use oss_server::store::init_test_store;
use tempfile::tempdir;

const ADMIN_TOKEN: &str = "trash-api-admin-token";

async fn setup_server() -> (SocketAddr, tempfile::TempDir, String) {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let storage = StorageService::new(&data_dir);
    storage.init().await.unwrap();

    let config = Arc::new(Config {
        host: "127.0.0.1:0".to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
        storage_backend: oss_server::store::Backend::Memory,
        db_url: "memory://in-memory".to_string(),
        admin_token: ADMIN_TOKEN.to_string(),
        master_key: "trash-api-master-key".to_string(),
        public_base_url: "http://localhost".to_string(),
        signature_expire_seconds: 900,
        max_upload_size: 10 * 1024 * 1024,
        cache_size_bytes: 0,
        cors_allowed_origins: vec![],
        cors_enabled: false,
        trash_retention_days: 30,
        cleanup_interval_minutes: 0,
        cleanup_max_objects: 100,
        cleanup_max_seconds: 30,
        startup_check_mode: StartupCheckMode::Report,
        nonce_retention_seconds: 3600,
    });

    let pool = init_test_store();
    let app = build_router(config, pool, storage);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (addr, dir, ADMIN_TOKEN.to_string())
}

fn base(addr: SocketAddr) -> String {
    format!("http://{}", addr)
}

#[tokio::test]
async fn trash_and_maintenance_admin_api() {
    let (addr, _dir, token) = setup_server().await;
    let client = reqwest::Client::new();

    // ---- 准备 App / Bucket ----
    let resp = client
        .post(format!("{}/api/admin/apps", base(addr)))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"name": "trash-api-app"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let app_id = body["app_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{}/api/admin/apps/{}/buckets", base(addr), app_id))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"name": "trash-api-bucket"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let bucket_id = body["bucket_id"].as_str().unwrap().to_string();

    // ---- 上传对象 ----
    let resp = client
        .put(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .header("Content-Type", "text/plain")
        .body("hello soft delete")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 下载正常
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello soft delete");

    // ---- 普通删除 → 回收站 ----
    let resp = client
        .delete(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // 原对象 404（active 语义）
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // 回收站里能看到
    let resp = client
        .get(format!("{}/api/admin/trash", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["key"], "docs/a.txt");
    assert!(body["items"][0]["deleted_at"].as_i64().unwrap() > 0);
    assert_eq!(body["items"][0]["size"], "hello soft delete".len());
    let object_id = body["items"][0]["object_id"].as_str().unwrap().to_string();

    // 文件仍在磁盘（回收站对象保留文件）
    let file_exists = std::fs::read_dir(
        _dir.path()
            .join("data")
            .join(&app_id)
            .join(&bucket_id)
            .join("docs"),
    )
    .map(|entries| entries.count())
    .unwrap_or(0);
    assert_eq!(file_exists, 1, "软删除后磁盘文件必须保留");

    // ---- 还原 ----
    let resp = client
        .post(format!("{}/api/admin/trash/{}/restore", base(addr), object_id))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"overwrite": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "restored");

    // 还原后可下载
    let resp = client
        .get(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // ---- 覆盖写 → 旧版本进回收站；还原冲突 ----
    let resp = client
        .put(format!(
            "{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt",
            base(addr)
        ))
        .header("X-Admin-Token", &token)
        .header("Content-Type", "text/plain")
        .body("replacement")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{}/api/admin/trash/{}/restore", base(addr), object_id))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"overwrite": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "key 被占用时必须返回明确冲突");

    let resp = client
        .post(format!("{}/api/admin/trash/{}/restore", base(addr), object_id))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"overwrite": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // ---- 彻底删除：文件与元数据消失 ----
    let resp = client
        .delete(format!("{}/api/admin/buckets/trash-api-bucket/objects/docs/a.txt", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = client
        .delete(format!("{}/api/admin/trash/{}", base(addr), object_id))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // 彻底删除后该 object_id 的磁盘文件必须消失
    let purged_path = _dir
        .path()
        .join("data")
        .join(&app_id)
        .join(&bucket_id)
        .join("docs")
        .join(&object_id);
    assert!(!purged_path.exists(), "彻底删除后磁盘文件必须消失");

    let remaining: serde_json::Value = client
        .get(format!("{}/api/admin/trash", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // replacement 的旧版本仍在回收站，但 object_id 已彻底删除
    let items = remaining["items"].as_array().unwrap();
    assert!(!items
        .iter()
        .any(|item| item["object_id"].as_str() == Some(object_id.as_str())));

    // ---- 维护接口：自检（report）不修改数据 ----
    let resp = client
        .post(format!("{}/api/admin/maintenance/startup-check", base(addr)))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"mode": "report"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["invalid_records"], 0);
    assert_eq!(body["orphan_files"], 0);
    assert_eq!(body["fixed"], 0);

    // ---- 状态查询包含配置与最近一次运行 ----
    let resp = client
        .get(format!("{}/api/admin/maintenance/status", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["config"]["trash_retention_days"], 30);
    assert_eq!(body["config"]["startup_check_mode"], "report");
    assert!(body["startup_check"].is_object());

    // ---- 手动触发清理 ----
    let resp = client
        .post(format!("{}/api/admin/maintenance/cleanup", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["kind"], "cleanup");
    assert_eq!(body["status"], "success");

    // 清理后状态可查
    let body: serde_json::Value = client
        .get(format!("{}/api/admin/maintenance/status", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["cleanup"].is_object());

    // ---- healthz ----
    let resp = client
        .get(format!("{}/healthz", base(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["backend"], "memory");
    assert_eq!(body["schema_version"], 2);
}

#[tokio::test]
async fn trash_batch_restore() {
    let (addr, _dir, token) = setup_server().await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .post(format!("{}/api/admin/apps", base(addr)))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"name": "batch-app"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let app_id = body["app_id"].as_str().unwrap().to_string();

    client
        .post(format!("{}/api/admin/apps/{}/buckets", base(addr), app_id))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"name": "batch-bucket"}))
        .send()
        .await
        .unwrap();

    let mut ids = Vec::new();
    for key in ["a.txt", "b.txt"] {
        client
            .put(format!(
                "{}/api/admin/buckets/batch-bucket/objects/{}",
                base(addr),
                key
            ))
            .header("X-Admin-Token", &token)
            .body(format!("content-{}", key))
            .send()
            .await
            .unwrap();
    }
    for key in ["a.txt", "b.txt"] {
        assert_eq!(
            client
                .delete(format!(
                    "{}/api/admin/buckets/batch-bucket/objects/{}",
                    base(addr),
                    key
                ))
                .header("X-Admin-Token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
    }

    let trash: serde_json::Value = client
        .get(format!("{}/api/admin/trash", base(addr)))
        .header("X-Admin-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for item in trash["items"].as_array().unwrap() {
        ids.push(item["object_id"].as_str().unwrap().to_string());
    }
    assert_eq!(ids.len(), 2);

    let resp = client
        .post(format!("{}/api/admin/trash/batch", base(addr)))
        .header("X-Admin-Token", &token)
        .json(&serde_json::json!({"action": "restore", "object_ids": ids}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["succeeded"], 2);
    assert_eq!(body["failed"].as_array().unwrap().len(), 0);

    for key in ["a.txt", "b.txt"] {
        assert_eq!(
            client
                .get(format!(
                    "{}/api/admin/buckets/batch-bucket/objects/{}",
                    base(addr),
                    key
                ))
                .header("X-Admin-Token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
}
