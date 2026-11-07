//! 回收站 / 软删除 / 维护任务的行为测试（memory 后端 + 真实磁盘目录）。
//!
//! 覆盖：
//! - 普通删除只移入回收站，磁盘文件保留；
//! - 还原成功 / key 冲突 / 原桶已删除；
//! - 彻底删除后文件消失；
//! - 桶删除、App 删除与回收站语义一致（不产生无主文件）；
//! - 保留期到期被清理、未到期不被清理；
//! - 启动自检 report 不修改数据、fix 自动修复；
//! - 孤儿文件、失效记录识别；
//! - 清理任务与并发上传不冲突；
//! - 过期 nonce 清理。

use oss_server::config::{Config, StartupCheckMode};
use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::models::object::Object;
use oss_server::services::maintenance::MaintenanceService;
use oss_server::services::storage::StorageService;
use oss_server::store::{init_test_store, RestoreOutcome, StoreHandle};
use tempfile::tempdir;
use uuid::Uuid;

const MASTER_KEY: &str = "trash-test-master-key";

fn test_config(data_dir: &str, retention_days: i64) -> Config {
    Config {
        host: "127.0.0.1:0".to_string(),
        data_dir: data_dir.to_string(),
        storage_backend: oss_server::store::Backend::Memory,
        db_url: "memory://in-memory".to_string(),
        admin_token: "test-admin-token".to_string(),
        master_key: MASTER_KEY.to_string(),
        public_base_url: "http://localhost".to_string(),
        signature_expire_seconds: 900,
        max_upload_size: 10 * 1024 * 1024,
        cache_size_bytes: 0,
        cors_allowed_origins: vec![],
        cors_enabled: false,
        trash_retention_days: retention_days,
        cleanup_interval_minutes: 0,
        cleanup_max_objects: 1000,
        cleanup_max_seconds: 60,
        startup_check_mode: StartupCheckMode::Report,
        nonce_retention_seconds: 3600,
    }
}

struct Env {
    _dir: tempfile::TempDir,
    store: StoreHandle,
    storage: StorageService,
    app: App,
    bucket: Bucket,
}

async fn setup() -> Env {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let storage = StorageService::new(&data_dir);
    storage.init().await.unwrap();
    let store = init_test_store();
    let (app, _) = App::create(&store, "trash-app", MASTER_KEY).await.unwrap();
    let bucket = Bucket::create(&store, &app.app_id, "trash-bucket", false)
        .await
        .unwrap();
    Env {
        _dir: dir,
        store,
        storage,
        app,
        bucket,
    }
}

async fn upload(env: &Env, bucket: &Bucket, key: &str, body: &[u8]) -> Object {
    let object_id = Uuid::new_v4().to_string();
    let (etag, size) = env
        .storage
        .write_bytes(&bucket.app_id, &bucket.bucket_id, key, &object_id, body)
        .await
        .unwrap();
    Object::create_with_id(
        &env.store,
        &object_id,
        &bucket.bucket_id,
        key,
        size as i64,
        "text/plain",
        &etag,
    )
    .await
    .unwrap()
}

fn path_of(env: &Env, obj: &Object) -> std::path::PathBuf {
    env.storage
        .object_path(&obj.app_id, &obj.bucket_id, &obj.key, &obj.object_id)
}

#[tokio::test]
async fn soft_delete_keeps_file_restore_and_purge() {
    let env = setup().await;
    let obj = upload(&env, &env.bucket, "docs/a.txt", b"hello trash").await;
    let path = path_of(&env, &obj);
    assert!(path.exists(), "上传后文件应存在");

    // ---- 软删除：元数据进回收站，文件保留 ----
    assert!(Object::delete(&env.store, &obj.object_id).await.unwrap());
    assert!(path.exists(), "普通删除绝不能删除磁盘文件");
    assert!(
        Object::get_by_id(&env.store, &obj.object_id)
            .await
            .unwrap()
            .is_none()
    );
    let trashed = env
        .store
        .get_object_any(&obj.object_id)
        .await
        .unwrap()
        .unwrap();
    assert!(trashed.is_trashed());
    assert_eq!(trashed.original_key.as_deref(), Some("docs/a.txt"));
    assert_eq!(trashed.original_bucket_id.as_deref(), Some(env.bucket.bucket_id.as_str()));

    let trash = Object::list_trash(&env.store, None, None, 0, 10).await.unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(Object::count_trash(&env.store, None, None).await.unwrap(), 1);
    assert_eq!(
        Object::total_trash_size(&env.store, None, None).await.unwrap(),
        "hello trash".len() as i64
    );

    // ---- 还原成功 ----
    match Object::restore(&env.store, &obj.object_id, false).await.unwrap() {
        RestoreOutcome::Restored(restored) => {
            assert_eq!(restored.object_id, obj.object_id);
            assert_eq!(restored.key, "docs/a.txt");
        }
        other => panic!("expected Restored, got {:?}", other),
    }
    assert!(Object::get_by_id(&env.store, &obj.object_id)
        .await
        .unwrap()
        .is_some());

    // ---- key 被新对象占用：默认冲突，overwrite 时旧占用者进回收站 ----
    let replacement = upload(&env, &env.bucket, "docs/a.txt", b"new content").await;
    assert_ne!(replacement.object_id, obj.object_id);

    match Object::restore(&env.store, &obj.object_id, false).await.unwrap() {
        RestoreOutcome::Conflict { existing_object_id } => {
            assert_eq!(existing_object_id, replacement.object_id)
        }
        other => panic!("expected Conflict, got {:?}", other),
    }
    match Object::restore(&env.store, &obj.object_id, true).await.unwrap() {
        RestoreOutcome::Restored(restored) => assert_eq!(restored.object_id, obj.object_id),
        other => panic!("expected Restored with overwrite, got {:?}", other),
    }
    // 被覆盖的 replacement 进入回收站，其文件仍在。
    assert!(env
        .store
        .get_object_any(&replacement.object_id)
        .await
        .unwrap()
        .unwrap()
        .is_trashed());
    assert!(path_of(&env, &replacement).exists());

    // ---- 彻底删除：文件与元数据都消失（与 handler 相同的顺序：先删文件，再删元数据）----
    assert!(Object::delete(&env.store, &obj.object_id).await.unwrap());
    env.storage
        .delete(&obj.app_id, &obj.bucket_id, &obj.key, &obj.object_id)
        .await
        .unwrap();
    let record = Object::purge(&env.store, &obj.object_id).await.unwrap();
    assert!(record.is_some());
    assert!(!path.exists(), "彻底删除后磁盘文件必须消失");
    assert!(env
        .store
        .get_object_any(&obj.object_id)
        .await
        .unwrap()
        .is_none());
    assert!(env
        .store
        .get_object_any(&replacement.object_id)
        .await
        .unwrap()
        .unwrap()
        .is_trashed());
}

#[tokio::test]
async fn bucket_and_app_deletion_trash_objects_and_keep_files() {
    let env = setup().await;
    let obj = upload(&env, &env.bucket, "keep.txt", b"bucket delete").await;
    let path = path_of(&env, &obj);

    // 删除桶：active 对象进回收站，桶记录消失，文件保留。
    assert!(Bucket::delete(&env.store, &env.bucket.bucket_id)
        .await
        .unwrap());
    assert!(path.exists(), "桶删除不能留下无主文件（文件应仍在回收站）");
    assert_eq!(
        Object::count_by_bucket(&env.store, &env.bucket.bucket_id)
            .await
            .unwrap(),
        0
    );
    let trash = Object::list_trash(
        &env.store,
        Some(&env.app.app_id),
        Some(&env.bucket.bucket_id),
        0,
        10,
    )
    .await
    .unwrap();
    assert_eq!(trash.len(), 1);
    assert!(env
        .store
        .get_bucket_by_id(&env.bucket.bucket_id)
        .await
        .unwrap()
        .is_none());

    // 原桶已删除，还原必须给出明确冲突。
    match Object::restore(&env.store, &obj.object_id, false)
        .await
        .unwrap()
    {
        RestoreOutcome::BucketMissing { bucket_id } => {
            assert_eq!(bucket_id, env.bucket.bucket_id)
        }
        other => panic!("expected BucketMissing, got {:?}", other),
    }

    // ---- App 删除：所有 active 对象进回收站，文件保留，App/桶记录删除 ----
    let env2 = setup().await;
    let obj2 = upload(&env2, &env2.bucket, "app.txt", b"app delete").await;
    let path2 = path_of(&env2, &obj2);
    assert!(App::delete(&env2.store, &env2.app.app_id).await.unwrap());
    assert!(path2.exists());
    assert!(env2
        .store
        .get_app(&env2.app.app_id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        Bucket::count_by_app(&env2.store, &env2.app.app_id)
            .await
            .unwrap(),
        0
    );
    // 回收站仍可按 app_id 找到（对象自带 app_id）。
    assert_eq!(
        Object::count_trash(&env2.store, Some(&env2.app.app_id), None)
            .await
            .unwrap(),
        1
    );

    // 彻底删除（先删文件，再删元数据）后文件消失。
    env2.storage
        .delete(&obj2.app_id, &obj2.bucket_id, &obj2.key, &obj2.object_id)
        .await
        .unwrap();
    assert!(Object::purge(&env2.store, &obj2.object_id)
        .await
        .unwrap()
        .is_some());
    assert!(!path2.exists());
}

#[tokio::test]
async fn cleanup_purges_expired_trash_but_keeps_recent() {
    let env = setup().await;
    let old = upload(&env, &env.bucket, "old.txt", b"expired").await;
    let recent = upload(&env, &env.bucket, "recent.txt", b"recent").await;
    let old_path = path_of(&env, &old);
    let recent_path = path_of(&env, &recent);

    let now = chrono::Utc::now().timestamp();
    env.store
        .soft_delete_object(&old.object_id, now - 40 * 86400)
        .await
        .unwrap();
    env.store
        .soft_delete_object(&recent.object_id, now - 86400)
        .await
        .unwrap();

    let config = test_config(env.storage.base_dir().to_string_lossy().as_ref(), 30);
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);
    let run = maintenance.run_cleanup("manual").await.unwrap().unwrap();

    assert_eq!(run.purged_objects, 1, "只有超过 30 天的对象应被清理");
    assert!(run.freed_bytes > 0);
    assert!(!old_path.exists(), "过期对象文件必须被删除");
    assert!(recent_path.exists(), "未到期对象文件必须保留");
    assert_eq!(
        Object::count_trash(&env.store, None, None).await.unwrap(),
        1
    );
    assert!(env
        .store
        .latest_maintenance_run("cleanup")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn startup_check_report_does_not_modify_fix_repairs() {
    let env = setup().await;
    // 正常对象，然后手动删除磁盘文件 → 失效记录。
    let missing = upload(&env, &env.bucket, "missing.txt", b"will vanish").await;
    let missing_path = path_of(&env, &missing);
    tokio::fs::remove_file(&missing_path).await.unwrap();

    // 正常对象 + 手动制造孤儿文件。
    let ok = upload(&env, &env.bucket, "ok.txt", b"ok").await;
    let ok_path = path_of(&env, &ok);
    let orphan_path = env.storage.base_dir().join("orphan.bin");
    tokio::fs::write(&orphan_path, b"orphan-data").await.unwrap();

    let config = test_config(env.storage.base_dir().to_string_lossy().as_ref(), 30);
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);

    // ---- report：只报告，不修改 ----
    let report = maintenance
        .startup_check(StartupCheckMode::Report, "manual")
        .await
        .unwrap();
    assert_eq!(report.invalid_records, 1);
    assert_eq!(report.orphan_files, 1);
    assert_eq!(report.orphan_bytes, b"orphan-data".len() as i64);
    assert_eq!(report.fixed, 0);
    assert!(orphan_path.exists(), "report 模式不允许删除孤儿文件");
    assert!(
        env.store
            .get_object_any(&missing.object_id)
            .await
            .unwrap()
            .is_some(),
        "report 模式不允许删除失效记录"
    );
    assert!(ok_path.exists());

    // ---- fix：自动修复 ----
    let fixed = maintenance
        .startup_check(StartupCheckMode::Fix, "manual")
        .await
        .unwrap();
    assert_eq!(fixed.invalid_records, 1);
    assert_eq!(fixed.orphan_files, 1);
    assert_eq!(fixed.fixed, 2);
    assert!(fixed.freed_bytes > 0);
    assert!(!orphan_path.exists(), "fix 模式应删除孤儿文件");
    assert!(
        env.store
            .get_object_any(&missing.object_id)
            .await
            .unwrap()
            .is_none(),
        "fix 模式应删除失效记录"
    );
    assert!(ok_path.exists(), "正常文件不能被误删");
    assert!(Object::get_by_id(&env.store, &ok.object_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn startup_check_ignores_metadata_db_file() {
    let env = setup().await;
    let data_dir = env.storage.base_dir().to_path_buf();
    let db_path = data_dir.join("oss.db");
    tokio::fs::write(&db_path, b"not-an-object").await.unwrap();

    // redb 数据库默认就落在数据目录里，自检必须把它排除，否则 fix 会删库。
    let mut config = test_config(data_dir.to_string_lossy().as_ref(), 30);
    config.storage_backend = oss_server::store::Backend::Redb;
    config.db_url = format!("redb://{}", db_path.display());
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);

    let report = maintenance
        .startup_check(StartupCheckMode::Report, "startup")
        .await
        .unwrap();
    assert_eq!(report.orphan_files, 0, "元数据文件不能被当作孤儿文件");
    assert_eq!(report.orphan_bytes, 0);

    let fixed = maintenance
        .startup_check(StartupCheckMode::Fix, "manual")
        .await
        .unwrap();
    assert_eq!(fixed.orphan_files, 0);
    assert_eq!(fixed.fixed, 0);
    assert!(db_path.exists(), "fix 模式绝不能删除元数据文件");
}

#[tokio::test]
async fn cleanup_runs_without_corrupting_concurrent_uploads() {
    let env = setup().await;
    let config = test_config(env.storage.base_dir().to_string_lossy().as_ref(), 30);
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);

    // 并发上传 20 个对象，同时触发清理。
    let mut handles = Vec::new();
    for i in 0..20 {
        let store = env.store.clone();
        let storage = env.storage.clone();
        let bucket = env.bucket.clone();
        handles.push(tokio::spawn(async move {
            let object_id = Uuid::new_v4().to_string();
            let body = format!("payload-{}", i).into_bytes();
            let (etag, size) = storage
                .write_bytes(&bucket.app_id, &bucket.bucket_id, "concurrent/file.txt", &object_id, &body)
                .await
                .unwrap();
            Object::create_with_id(
                &store,
                &object_id,
                &bucket.bucket_id,
                "concurrent/file.txt",
                size as i64,
                "text/plain",
                &etag,
            )
            .await
            .ok()
        }));
    }
    let cleanup = maintenance.run_cleanup("manual").await.unwrap();

    let mut active = 0;
    for handle in handles {
        if handle.await.unwrap().is_some() {
            active += 1;
        }
    }
    // 同 key 并发写最终只有一条 active 记录，其余进入回收站，但不丢数据、不串数据。
    assert_eq!(
        Object::count_by_bucket(&env.store, &env.bucket.bucket_id)
            .await
            .unwrap(),
        1
    );
    let winner = Object::get_by_bucket_and_key(&env.store, &env.bucket.bucket_id, "concurrent/file.txt")
        .await
        .unwrap()
        .expect("active 记录必须存在");
    let body = env
        .storage
        .read_bytes(&winner.app_id, &winner.bucket_id, &winner.key, &winner.object_id)
        .await
        .unwrap();
    assert!(body.starts_with(b"payload-"));
    assert!(cleanup.is_some(), "清理任务应成功运行并写入记录");
    assert!(active >= 1);
    assert!(
        Object::count_trash(&env.store, None, None).await.unwrap() >= 1,
        "被覆盖的版本应进入回收站"
    );
}

#[tokio::test]
async fn cleanup_purges_expired_nonces() {
    let env = setup().await;
    let config = test_config(env.storage.base_dir().to_string_lossy().as_ref(), 30);
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);

    let now = chrono::Utc::now().timestamp();
    assert!(
        env.store
            .insert_nonce(&env.app.app_id, "stale-nonce", now - 7200)
            .await
            .unwrap()
    );
    assert!(
        env.store
            .insert_nonce(&env.app.app_id, "fresh-nonce", now)
            .await
            .unwrap()
    );
    assert!(
        !env.store
            .insert_nonce(&env.app.app_id, "fresh-nonce", now)
            .await
            .unwrap(),
        "nonce 必须防重放"
    );

    let run = maintenance.run_cleanup("manual").await.unwrap().unwrap();
    assert_eq!(run.purged_nonces, 1);
    // 新鲜 nonce 仍能阻止重放
    assert!(
        !env.store
            .insert_nonce(&env.app.app_id, "fresh-nonce", now)
            .await
            .unwrap()
    );
}
