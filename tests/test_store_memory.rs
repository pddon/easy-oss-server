//! 验证存储抽象层可以在 memory 后端完整运行核心流程：
//! App 创建 / 桶增删 / 对象上传(写盘+元数据)、下载(读盘)、列表 / 分页 /
//! nonce 防重放 / schema 版本。上层代码不包含任何 redb 类型。

use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::models::object::Object;
use oss_server::services::crypto::decrypt_secret;
use oss_server::services::storage::StorageService;
use oss_server::store::init_test_store;
use tempfile::tempdir;

const MASTER_KEY: &str = "memory-backend-test-master-key";

#[tokio::test]
async fn memory_backend_supports_full_core_flow() {
    let store = init_test_store();
    assert_eq!(store.backend_name(), "memory");
    assert_eq!(store.schema_version().await.unwrap(), 2);

    // ---- App ----
    let (app, plaintext) = App::create(&store, "memory-app", MASTER_KEY)
        .await
        .unwrap();
    assert_ne!(app.app_secret_encrypted, plaintext);
    assert_eq!(
        decrypt_secret(MASTER_KEY, &app.app_secret_encrypted).unwrap(),
        plaintext
    );
    assert_eq!(App::count(&store).await.unwrap(), 1);
    assert_eq!(App::list(&store, 0, 10).await.unwrap()[0].app_id, app.app_id);

    // ---- Bucket 增删 ----
    let bucket = Bucket::create(&store, &app.app_id, "memory-bucket", false)
        .await
        .unwrap();
    assert_eq!(Bucket::count_by_app(&store, &app.app_id).await.unwrap(), 1);

    let duplicate = Bucket::create(&store, &app.app_id, "memory-bucket", false).await;
    assert!(duplicate.is_err(), "同名桶必须被抽象层拒绝");

    let temp_bucket = Bucket::create(&store, &app.app_id, "memory-temp", true)
        .await
        .unwrap();
    assert!(Bucket::delete(&store, &temp_bucket.bucket_id).await.unwrap());
    assert!(Bucket::get_by_id(&store, &temp_bucket.bucket_id)
        .await
        .unwrap()
        .is_none());

    // ---- 对象上传：文件落盘 + 元数据入库 ----
    let dir = tempdir().unwrap();
    let storage = StorageService::new(dir.path());
    storage.init().await.unwrap();

    let bytes = b"hello memory backend";
    let object_id = uuid::Uuid::new_v4().to_string();
    let (etag, size) = storage
        .write_bytes(&app.app_id, &bucket.bucket_id, "docs/hello.txt", &object_id, bytes)
        .await
        .unwrap();
    assert_eq!(size as usize, bytes.len());

    let obj = Object::create_with_id(
        &store,
        &object_id,
        &bucket.bucket_id,
        "docs/hello.txt",
        size as i64,
        "text/plain",
        &etag,
    )
    .await
    .unwrap();

    // ---- 按 key 查询 / 下载 ----
    let found = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "docs/hello.txt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.object_id, obj.object_id);

    let downloaded = storage
        .read_bytes(
            &app.app_id,
            &bucket.bucket_id,
            "docs/hello.txt",
            &object_id,
        )
        .await
        .unwrap();
    assert_eq!(downloaded, bytes);

    // ---- 前缀分页列表 ----
    for key in ["docs/a.txt", "docs/b.txt", "other/c.txt"] {
        Object::create(&store, &bucket.bucket_id, key, 10, "text/plain", "etag")
            .await
            .unwrap();
    }
    let all = Object::list(&store, &bucket.bucket_id, "", "", 100)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);

    let prefixed = Object::list(&store, &bucket.bucket_id, "docs/", "", 100)
        .await
        .unwrap();
    assert_eq!(prefixed.len(), 3);
    assert_eq!(prefixed[0].key, "docs/a.txt");

    let page = Object::list(&store, &bucket.bucket_id, "docs/", "docs/a.txt", 1)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].key, "docs/b.txt");

    // ---- 覆盖写同一 key ----
    let old = Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "docs/a.txt")
        .await
        .unwrap()
        .unwrap();
    let new_id = uuid::Uuid::new_v4().to_string();
    let previous = store
        .put_object(&Object {
            object_id: new_id.clone(),
            app_id: app.app_id.clone(),
            bucket_id: bucket.bucket_id.clone(),
            key: "docs/a.txt".to_string(),
            size: 20,
            content_type: "text/plain".to_string(),
            etag: "new".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            deleted_at: None,
            original_bucket_id: None,
            original_key: None,
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(previous.object_id, old.object_id);
    assert_eq!(Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(), 4);
    assert_eq!(
        Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "docs/a.txt")
            .await
            .unwrap()
            .unwrap()
            .object_id,
        new_id
    );
    assert!(Object::get_by_id(&store, &old.object_id).await.unwrap().is_none());

    // ---- nonce 防重放 ----
    assert!(store.insert_nonce(&app.app_id, "n1", 100).await.unwrap());
    assert!(!store.insert_nonce(&app.app_id, "n1", 100).await.unwrap());

    // ---- 删除桶级联清理对象元数据 ----
    assert!(Bucket::delete(&store, &bucket.bucket_id).await.unwrap());
    assert_eq!(
        Object::count_by_bucket(&store, &bucket.bucket_id).await.unwrap(),
        0
    );
    assert!(Object::get_by_bucket_and_key(&store, &bucket.bucket_id, "docs/hello.txt")
        .await
        .unwrap()
        .is_none());

    storage.delete_app(&app.app_id).await.unwrap();
    assert!(App::delete(&store, &app.app_id).await.unwrap());
    assert!(App::get_by_id(&store, &app.app_id).await.unwrap().is_none());
}
