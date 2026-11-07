//! MySQL 真库集成测试（仅在 `--features mysql` 时编译/运行）。
//!
//! 使用独立 schema `oss_server_test`，测试开始和结束都会清理该 schema 内的
//! 本项目表；不会触碰实例上的其他数据库或表。
//!
//! 可通过 `OSS_MYSQL_TEST_URL` 覆盖 DSN。

#![cfg(feature = "mysql")]

use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::models::maintenance::{MaintenanceRun, KIND_CLEANUP, KIND_STARTUP_CHECK};
use oss_server::models::object::Object;
use oss_server::services::crypto::decrypt_secret;
use oss_server::store::mysql_store::MysqlStore;
use oss_server::store::{RestoreOutcome, StoreError, StoreHandle};

const MASTER_KEY: &str = "mysql-real-db-test-master-key";

fn test_dsn() -> String {
    std::env::var("OSS_MYSQL_TEST_URL").unwrap_or_else(|_| {
        // 密码中的 @ 必须百分号编码。
        "mysql://root:Simo%4013579@192.168.56.100:3306/oss_server_test".to_string()
    })
}

async fn drop_project_tables() {
    // 只清理本项目在 oss_server_test 中创建的表。
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&test_dsn())
        .await
        .expect("connect test schema for cleanup");
    sqlx::raw_sql(
        "SET FOREIGN_KEY_CHECKS = 0; \
         DROP TABLE IF EXISTS objects; \
         DROP TABLE IF EXISTS buckets; \
         DROP TABLE IF EXISTS apps; \
         DROP TABLE IF EXISTS nonces; \
         DROP TABLE IF EXISTS maintenance_runs; \
         DROP TABLE IF EXISTS _migrations; \
         SET FOREIGN_KEY_CHECKS = 1;",
    )
    .execute(&pool)
    .await
    .expect("drop project tables");
    pool.close().await;
}

async fn fresh_store() -> StoreHandle {
    // 第一次 connect 负责建库；然后清表；再次 connect 重新建表并写入版本。
    MysqlStore::connect(&test_dsn())
        .await
        .expect("MySQL connect (create schema)");
    drop_project_tables().await;
    MysqlStore::connect(&test_dsn())
        .await
        .expect("MySQL connect (recreate tables)")
}

fn object_for(app_id: &str, bucket_id: &str, object_id: String, key: &str, size: i64) -> Object {
    Object {
        object_id,
        app_id: app_id.to_string(),
        bucket_id: bucket_id.to_string(),
        key: key.to_string(),
        size,
        content_type: "text/plain".to_string(),
        etag: format!("etag-{}", size),
        created_at: chrono::Utc::now().timestamp(),
        deleted_at: None,
        original_bucket_id: None,
        original_key: None,
    }
}

fn make_run(kind: &str, trigger: &str, seq: i64) -> MaintenanceRun {
    MaintenanceRun {
        run_id: format!("mysql-test-{kind}-{seq}"),
        kind: kind.to_string(),
        trigger: trigger.to_string(),
        started_at: seq,
        finished_at: seq,
        duration_ms: 1,
        status: "success".to_string(),
        scanned: 0,
        purged_objects: 0,
        freed_bytes: 0,
        invalid_records: 0,
        orphan_files: 0,
        orphan_bytes: 0,
        purged_nonces: 0,
        fixed: 0,
        error: None,
        detail: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mysql_backend_core_and_concurrency() {
    let store = fresh_store().await;
    assert_eq!(store.backend_name(), "mysql");
    assert_eq!(store.schema_version().await.unwrap(), 2, "schema 版本初始化");

    // ---- App 创建与 secret 密文存储 ----
    let (app, plaintext) = App::create(&store, "mysql-real-app", MASTER_KEY)
        .await
        .expect("create app");
    let secret: String = sqlx::query_scalar("SELECT app_secret_encrypted FROM apps WHERE app_id = ?")
        .bind(&app.app_id)
        .fetch_one(&sqlx::MySqlPool::connect(&test_dsn()).await.unwrap())
        .await
        .unwrap();
    assert_ne!(secret, plaintext, "数据库中必须是密文");
    assert_eq!(
        decrypt_secret(MASTER_KEY, &secret).unwrap(),
        plaintext,
        "密文可解密回原文"
    );

    let fetched = App::get_by_id(&store, &app.app_id).await.unwrap().unwrap();
    assert_eq!(fetched.name, "mysql-real-app");
    assert_eq!(App::count(&store).await.unwrap(), 1);
    assert_eq!(App::list(&store, 0, 10).await.unwrap().len(), 1);

    // ---- Bucket 创建 / 全局唯一 / 列出 / 删除 ----
    let bucket = Bucket::create(&store, &app.app_id, "mysql-real-bucket", false)
        .await
        .expect("create bucket");
    assert!(Bucket::get_by_name(&store, "mysql-real-bucket")
        .await
        .unwrap()
        .is_some());

    // 另一个 App 也不能复用全局同名桶。
    let (other_app, _) = App::create(&store, "mysql-other-app", MASTER_KEY)
        .await
        .unwrap();
    let duplicate = Bucket::create(&store, &other_app.app_id, "mysql-real-bucket", false).await;
    let duplicate_err = duplicate.expect_err("duplicate bucket name must fail");
    assert!(
        matches!(
            duplicate_err.downcast_ref::<StoreError>(),
            Some(StoreError::DuplicateBucketName(_))
        ),
        "unexpected error: {duplicate_err:?}"
    );
    assert_eq!(Bucket::count_by_app(&store, &app.app_id).await.unwrap(), 1);

    let temp = Bucket::create(&store, &app.app_id, "mysql-temp-bucket", true)
        .await
        .unwrap();
    assert!(Bucket::set_public(&store, &temp.bucket_id, false).await.unwrap());
    assert!(!Bucket::get_by_id(&store, &temp.bucket_id)
        .await
        .unwrap()
        .unwrap()
        .is_public);
    assert!(Bucket::delete(&store, &temp.bucket_id).await.unwrap());
    assert!(Bucket::get_by_id(&store, &temp.bucket_id).await.unwrap().is_none());

    // ---- Object 写入 / 按 key 查询 / 前缀分页 / 计数 / 删除 ----
    let keys = ["a/1.txt", "a/2.txt", "a/3.txt", "b/1.txt", "root.txt"];
    for (idx, key) in keys.iter().enumerate() {
        Object::create(&store, &bucket.bucket_id, key, 100 + idx as i64, "text/plain", "etag")
            .await
            .expect("put object");
    }

    assert_eq!(
        Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(),
        5
    );
    assert_eq!(
        Object::total_size_by_bucket(&store, &bucket.bucket_id).await.unwrap(),
        510
    );
    assert_eq!(Object::count_by_app(&store, &app.app_id).await.unwrap(), 5);
    assert_eq!(Object::total_size_by_app(&store, &app.app_id).await.unwrap(), 510);

    let by_key = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "a/2.txt")
        .await
        .unwrap()
        .expect("object by key");
    assert_eq!(by_key.size, 101);

    let prefixed = Object::list(&store, &bucket.bucket_id, "a/", "", 100).await.unwrap();
    assert_eq!(prefixed.len(), 3);
    assert!(prefixed.iter().all(|o| o.key.starts_with("a/")));

    let paged = Object::list(&store, &bucket.bucket_id, "a/", "", 2).await.unwrap();
    assert_eq!(paged.len(), 2);
    assert_eq!(paged[0].key, "a/1.txt");
    assert_eq!(paged[1].key, "a/2.txt");

    let after_marker = Object::list(&store, &bucket.bucket_id, "a/", "a/2.txt", 100)
        .await
        .unwrap();
    assert_eq!(after_marker.len(), 1);
    assert_eq!(after_marker[0].key, "a/3.txt");

    // 同 key 覆盖：旧版本进回收站，active 仍只有一条记录。
    let new_id = uuid::Uuid::new_v4().to_string();
    let previous = store
        .put_object(&object_for(
            &app.app_id,
            &bucket.bucket_id,
            new_id.clone(),
            "a/2.txt",
            999,
        ))
        .await
        .unwrap()
        .expect("old object returned");
    assert_eq!(previous.object_id, by_key.object_id);
    assert_eq!(
        Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(),
        5,
        "覆盖写不得产生重复 active 记录"
    );
    let replaced = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "a/2.txt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replaced.object_id, new_id);
    assert_eq!(replaced.size, 999);
    // 旧版本必须作为回收站记录保留，而不是被直接删除。
    let old = store
        .get_object_any(&previous.object_id)
        .await
        .unwrap()
        .expect("旧版本应保留在回收站");
    assert!(old.deleted_at.is_some(), "旧版本必须标记为回收站状态");
    assert_eq!(old.original_key.as_deref(), Some("a/2.txt"));
    assert_eq!(store.count_trash(None, None).await.unwrap(), 1);
    assert_eq!(store.total_trash_size(None, None).await.unwrap(), 101);
    assert_eq!(store.count_all_objects().await.unwrap(), 6);

    // ---- 软删除：active 消失，回收站保留 ----
    let target = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "a/1.txt")
        .await
        .unwrap()
        .unwrap();
    assert!(Object::delete(&store, &target.object_id).await.unwrap());
    assert_eq!(Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(), 4);
    assert!(Object::get_by_id(&store, &target.object_id).await.unwrap().is_none());
    let trashed = store
        .get_object_any(&target.object_id)
        .await
        .unwrap()
        .expect("软删后记录仍在");
    assert!(trashed.deleted_at.is_some());
    assert_eq!(
        trashed.original_bucket_id.as_deref(),
        Some(bucket.bucket_id.as_str())
    );
    assert_eq!(trashed.original_key.as_deref(), Some("a/1.txt"));

    // 回收站列表：排序 deleted_at DESC，支持过滤与分页。
    let trash = store.list_trash(None, None, 0, 10).await.unwrap();
    assert_eq!(trash.len(), 2);
    let by_bucket = store
        .list_trash(Some(&app.app_id), Some(&bucket.bucket_id), 0, 10)
        .await
        .unwrap();
    assert_eq!(by_bucket.len(), 2);
    assert_eq!(store.count_trash(Some(&app.app_id), None).await.unwrap(), 2);
    assert_eq!(
        store
            .total_trash_size(Some(&app.app_id), Some(&bucket.bucket_id))
            .await
            .unwrap(),
        201
    );
    let page = store.list_trash(None, None, 1, 1).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].object_id, trash[1].object_id);

    // ---- 还原：成功 / key 冲突 / overwrite 把占用者移入回收站 ----
    match store
        .restore_object(&target.object_id, false, chrono::Utc::now().timestamp())
        .await
        .unwrap()
    {
        RestoreOutcome::Restored(o) => {
            assert!(o.deleted_at.is_none());
            assert_eq!(o.key, "a/1.txt");
        }
        other => panic!("expected restored, got {other:?}"),
    }
    assert_eq!(Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(), 5);

    let now = chrono::Utc::now().timestamp();
    match store
        .restore_object(&previous.object_id, false, now)
        .await
        .unwrap()
    {
        RestoreOutcome::Conflict { existing_object_id } => {
            assert_eq!(existing_object_id, new_id);
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    match store
        .restore_object(&previous.object_id, true, now)
        .await
        .unwrap()
    {
        RestoreOutcome::Restored(o) => {
            assert_eq!(o.object_id, previous.object_id);
            assert!(o.deleted_at.is_none());
        }
        other => panic!("expected overwrite restore, got {other:?}"),
    }
    assert_eq!(Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(), 5);
    let occupant = store.get_object_any(&new_id).await.unwrap().unwrap();
    assert!(occupant.deleted_at.is_some(), "overwrite 还原必须把占用者移入回收站");

    // ---- 过期回收站列表：deleted_at ASC，受 limit 限制 ----
    let expired = store.list_expired_trash(now + 1, 1).await.unwrap();
    assert_eq!(expired.len(), 1);
    let expired_all = store.list_expired_trash(now + 1, 10).await.unwrap();
    assert!(
        expired_all
            .windows(2)
            .all(|w| w[0].deleted_at.unwrap() <= w[1].deleted_at.unwrap()),
        "过期回收站必须按 deleted_at 升序"
    );

    // ---- 彻底删除：元数据消失 ----
    assert!(store.purge_object(&new_id).await.unwrap().is_some());
    assert!(store.get_object_any(&new_id).await.unwrap().is_none());
    let batch_ids: Vec<String> = store
        .list_trash(None, None, 0, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.object_id)
        .collect();
    let purged = store.purge_objects(&batch_ids).await.unwrap();
    assert_eq!(purged as usize, batch_ids.len());
    assert_eq!(store.count_trash(None, None).await.unwrap(), 0);

    // ---- 维护任务运行记录 ----
    let run = make_run(KIND_STARTUP_CHECK, "startup", 1);
    store.save_maintenance_run(&run).await.unwrap();
    let latest = store
        .latest_maintenance_run(KIND_STARTUP_CHECK)
        .await
        .unwrap()
        .expect("startup check record");
    assert_eq!(latest.run_id, run.run_id);
    assert_eq!(latest.status, "success");

    let cleanup_run = make_run(KIND_CLEANUP, "manual", 2);
    store.save_maintenance_run(&cleanup_run).await.unwrap();
    assert_eq!(
        store
            .latest_maintenance_run(KIND_CLEANUP)
            .await
            .unwrap()
            .unwrap()
            .run_id,
        cleanup_run.run_id
    );
    assert!(
        store
            .latest_maintenance_run(KIND_STARTUP_CHECK)
            .await
            .unwrap()
            .is_some(),
        "不同 kind 的记录互不覆盖"
    );
    assert_eq!(
        store.list_maintenance_runs(KIND_CLEANUP, 10).await.unwrap().len(),
        1
    );

    // ---- nonce 防重放唯一性 ----
    assert!(store.insert_nonce(&app.app_id, "nonce-1", 100).await.unwrap());
    assert!(!store.insert_nonce(&app.app_id, "nonce-1", 100).await.unwrap());
    assert!(store.insert_nonce(&other_app.app_id, "nonce-1", 100).await.unwrap());
    assert!(store.insert_nonce(&app.app_id, "nonce-old", 10).await.unwrap());
    assert_eq!(store.purge_nonces_before(50).await.unwrap(), 1);
    assert!(!store.insert_nonce(&app.app_id, "nonce-1", 100).await.unwrap());

    // 并发插入同一 nonce：必须只有一个成功。
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        let app_id = app.app_id.clone();
        handles.push(tokio::spawn(async move {
            store.insert_nonce(&app_id, "nonce-concurrent", 200).await
        }));
    }
    let mut wins = 0;
    for handle in handles {
        if handle.await.unwrap().unwrap() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1, "并发 nonce 只能有一个成功");

    // ---- 并发写同一 key：唯一约束 + 重试，不串数据 ----
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        let app_id = app.app_id.clone();
        let bucket_id = bucket.bucket_id.clone();
        handles.push(tokio::spawn(async move {
            store
                .put_object(&object_for(
                    &app_id,
                    &bucket_id,
                    uuid::Uuid::new_v4().to_string(),
                    "concurrent/same.txt",
                    i,
                ))
                .await
        }));
    }
    let before_same_key = Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap();
    for handle in handles {
        let result = handle.await.unwrap();
        result.expect("concurrent put must not fail");
    }
    let after_same_key = Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap();
    assert_eq!(after_same_key, before_same_key + 1, "同一 key 只应留下一条记录");
    let final_obj = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "concurrent/same.txt")
        .await
        .unwrap()
        .expect("final object");
    assert!(
        final_obj.key == "concurrent/same.txt" && final_obj.bucket_id == bucket.bucket_id,
        "最终记录不得串数据"
    );

    // ---- 并发上传不同 key：每条记录都对应正确的 object_id/size ----
    let mut handles = Vec::new();
    for i in 0..20 {
        let store = store.clone();
        let app_id = app.app_id.clone();
        let bucket_id = bucket.bucket_id.clone();
        let object_id = format!("concurrent-object-{i:02}");
        let key = format!("parallel/{i:02}.bin");
        handles.push(tokio::spawn(async move {
            let key_inner = key.clone();
            store
                .put_object(&object_for(
                    &app_id,
                    &bucket_id,
                    object_id.clone(),
                    &key_inner,
                    i * 10,
                ))
                .await
                .map(|_| (key, object_id, i * 10))
        }));
    }
    for handle in handles {
        let (key, object_id, size) = handle.await.unwrap().unwrap();
        let stored = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, &key)
            .await
            .unwrap()
            .expect("parallel object");
        assert_eq!(stored.object_id, object_id);
        assert_eq!(stored.size, size);
    }

    // ---- 级联删除：active 进回收站，桶记录删除，回收站记录保留 ----
    let active_before = Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap();
    assert!(Bucket::delete(&store, &bucket.bucket_id).await.unwrap());
    assert_eq!(
        Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(),
        0,
        "删除桶后 active 对象必须清零"
    );
    assert!(Bucket::get_by_id(&store, &bucket.bucket_id).await.unwrap().is_none());
    assert!(
        store.count_trash(Some(&app.app_id), None).await.unwrap() >= active_before,
        "桶删除必须把 active 对象移入回收站而不是丢元数据"
    );
    // 原桶已删 → 还原返回 BucketMissing。
    let bucket_missing = store.list_trash(Some(&app.app_id), None, 0, 1).await.unwrap();
    match store
        .restore_object(&bucket_missing[0].object_id, false, now)
        .await
        .unwrap()
    {
        RestoreOutcome::BucketMissing { bucket_id } => assert_eq!(bucket_id, bucket.bucket_id),
        other => panic!("expected bucket missing, got {other:?}"),
    }

    assert_eq!(App::count(&store).await.unwrap(), 2);
    assert!(App::delete(&store, &app.app_id).await.unwrap());
    assert!(App::get_by_id(&store, &app.app_id).await.unwrap().is_none());
    assert_eq!(App::count(&store).await.unwrap(), 1); // other_app 仍在
    assert_eq!(Bucket::count_by_app(&store, &app.app_id).await.unwrap(), 0);

    // ---- 测试后清理本项目表 ----
    drop_project_tables().await;

    // ---- 配置驱动的真实装配：OSS_STORAGE_BACKEND=mysql -> store::init ----
    std::env::set_var("OSS_STORAGE_BACKEND", "mysql");
    std::env::set_var("OSS_ADMIN_TOKEN", "mysql-test-admin-token");
    std::env::set_var(
        "OSS_DB_URL",
        test_dsn().replace("oss_server_test", "oss_server"),
    );
    let config = oss_server::config::Config::from_env().expect("config from env");
    assert_eq!(config.storage_backend, oss_server::store::Backend::Mysql);
    let prod_store = oss_server::store::init(&config)
        .await
        .expect("store factory must assemble MySQL backend");
    assert_eq!(prod_store.backend_name(), "mysql");
    assert_eq!(prod_store.schema_version().await.unwrap(), 2);
}
