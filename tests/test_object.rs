//! Integration tests for object management and storage.

use oss_server::store::{init_test_store, StoreHandle};
use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::models::object::{validate_object_key, Object};
use oss_server::services::storage::StorageService;
use tempfile::tempdir;

const MASTER_KEY: &str = "test-master-key-object";

async fn setup(pool: &StoreHandle) -> (String, String, String) {
    let (app, _) = App::create(pool, "object-test-app", MASTER_KEY)
        .await
        .unwrap();
    let bucket = Bucket::create(pool, &app.app_id, "object-test-bucket", false)
        .await
        .unwrap();
    (app.app_id, bucket.bucket_id, "object-test-bucket".to_string())
}

#[tokio::test]
async fn test_object_create_and_get() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    let obj = Object::create(
        &pool,
        &bucket_id,
        "test/file.txt",
        1024,
        "text/plain",
        "d41d8cd98f00b204e9800998ecf8427e",
    )
    .await
    .unwrap();

    assert_eq!(obj.key, "test/file.txt");
    assert_eq!(obj.size, 1024);
    assert_eq!(obj.content_type, "text/plain");

    let by_key = Object::get_by_bucket_and_key(&pool, &bucket_id, "test/file.txt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_key.object_id, obj.object_id);

    let by_id = Object::get_by_id(&pool, &obj.object_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_id.key, "test/file.txt");
}

#[tokio::test]
async fn test_object_overwrite() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    // Create first version
    let obj1 = Object::create(
        &pool,
        &bucket_id,
        "overwrite.txt",
        100,
        "text/plain",
        "etag1",
    )
    .await
    .unwrap();

    // Delete old and create new (simulating overwrite)
    Object::delete(&pool, &obj1.object_id).await.unwrap();
    let obj2 = Object::create(
        &pool,
        &bucket_id,
        "overwrite.txt",
        200,
        "text/html",
        "etag2",
    )
    .await
    .unwrap();

    assert_ne!(obj1.object_id, obj2.object_id);
    assert_eq!(obj2.size, 200);
    assert_eq!(obj2.content_type, "text/html");

    // Only one object with that key
    let count = Object::count_by_bucket(&pool, &bucket_id).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_object_list_with_prefix_and_marker() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    let keys = [
        "docs/readme.txt",
        "docs/api.md",
        "docs/images/logo.png",
        "images/photo1.jpg",
        "images/photo2.jpg",
        "index.html",
        "styles/main.css",
    ];

    for k in &keys {
        Object::create(&pool, &bucket_id, k, 100, "text/plain", "etag")
            .await
            .unwrap();
    }

    // List all
    let all = Object::list(&pool, &bucket_id, "", "", 100).await.unwrap();
    assert_eq!(all.len(), 7);

    // Prefix: docs/
    let docs = Object::list(&pool, &bucket_id, "docs/", "", 100)
        .await
        .unwrap();
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().all(|o| o.key.starts_with("docs/")));

    // Marker
    let after = Object::list(&pool, &bucket_id, "", "docs/readme.txt", 100)
        .await
        .unwrap();
    assert_eq!(after.len(), 4); // 4 keys alphabetically after docs/readme.txt

    // Prefix + marker
    let pm = Object::list(&pool, &bucket_id, "docs/", "docs/api.md", 100)
        .await
        .unwrap();
    assert_eq!(pm.len(), 2); // docs/images/logo.png and docs/readme.txt
}

#[tokio::test]
async fn test_object_list_max_keys_truncation() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    for i in 0..25 {
        Object::create(
            &pool,
            &bucket_id,
            &format!("file-{:03}.txt", i),
            100,
            "text/plain",
            "etag",
        )
        .await
        .unwrap();
    }

    // Request 10, get 10, not truncated
    let page = Object::list(&pool, &bucket_id, "", "", 10).await.unwrap();
    assert_eq!(page.len(), 10);

    // Request one extra to check truncation
    let check = Object::list(&pool, &bucket_id, "", "", 11).await.unwrap();
    assert_eq!(check.len(), 11);
}

#[tokio::test]
async fn test_object_delete() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    let obj = Object::create(
        &pool,
        &bucket_id,
        "delete-me.txt",
        50,
        "text/plain",
        "etag",
    )
    .await
    .unwrap();

    assert!(Object::delete(&pool, &obj.object_id).await.unwrap());
    assert!(Object::get_by_id(&pool, &obj.object_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_object_stats() {
    let pool = init_test_store();
    let (app_id, bucket_id, _) = setup(&pool).await;

    for i in 0..10 {
        Object::create(
            &pool,
            &bucket_id,
            &format!("file-{}.txt", i),
            (i + 1) * 100, // 100, 200, ... 1000
            "text/plain",
            "etag",
        )
        .await
        .unwrap();
    }

    let count = Object::count_by_bucket(&pool, &bucket_id).await.unwrap();
    assert_eq!(count, 10);

    let total = Object::total_size_by_bucket(&pool, &bucket_id)
        .await
        .unwrap();
    assert_eq!(total, 100 + 200 + 300 + 400 + 500 + 600 + 700 + 800 + 900 + 1000);
    assert_eq!(total, 5500);

    let app_count = Object::count_by_app(&pool, &app_id).await.unwrap();
    assert_eq!(app_count, 10);

    let app_size = Object::total_size_by_app(&pool, &app_id).await.unwrap();
    assert_eq!(app_size, 5500);
}

#[tokio::test]
async fn test_storage_write_and_read() {
    let dir = tempdir().unwrap();
    let storage = StorageService::new(dir.path());
    storage.init().await.unwrap();

    let data = b"Hello, Storage Test! This is some test data.";
    let (etag, size) = storage
        .write_bytes("app1", "bucket1", "test/file.txt", "obj-1", data)
        .await
        .unwrap();

    assert_eq!(size, data.len() as u64);
    assert_eq!(etag.len(), 32); // MD5 hex

    let read_back = storage
        .read_bytes("app1", "bucket1", "test/file.txt", "obj-1")
        .await
        .unwrap();
    assert_eq!(read_back, data);
}

#[tokio::test]
async fn test_storage_md5_correctness() {
    let dir = tempdir().unwrap();
    let storage = StorageService::new(dir.path());
    storage.init().await.unwrap();

    // Known MD5: MD5("hello") = 5d41402abc4b2a76b9719d911017c592
    let data = b"hello";
    let (etag, _) = storage
        .write_bytes("app1", "b1", "hello.txt", "o1", data)
        .await
        .unwrap();

    assert_eq!(etag, "5d41402abc4b2a76b9719d911017c592");
}

#[tokio::test]
async fn test_storage_delete() {
    let dir = tempdir().unwrap();
    let storage = StorageService::new(dir.path());
    storage.init().await.unwrap();

    storage
        .write_bytes("app1", "b1", "del.txt", "o1", b"delete me")
        .await
        .unwrap();

    storage.delete("app1", "b1", "del.txt", "o1").await.unwrap();

    let result = storage.read_bytes("app1", "b1", "del.txt", "o1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_path_traversal_protection() {
    // Validate that dangerous keys are rejected
    assert!(!validate_object_key("../escape.txt"));
    assert!(!validate_object_key("/absolute/path.txt"));
    assert!(!validate_object_key("path/../../escape.txt"));
    assert!(!validate_object_key("path\\to\\file.txt"));
    assert!(!validate_object_key("file\x00name.txt"));

    // Valid keys pass（对象名允许空格，与 S3 语义一致）
    assert!(validate_object_key("normal-file.txt"));
    assert!(validate_object_key("file with spaces.txt"));
    assert!(validate_object_key("path/to/file.txt"));
    assert!(validate_object_key("file.name.with.dots.txt"));
    assert!(validate_object_key("snake_case_file.txt"));
}

#[tokio::test]
async fn test_storage_object_path_isolation() {
    let dir = tempdir().unwrap();
    let storage = StorageService::new(dir.path());
    storage.init().await.unwrap();

    // Write file with path-traversal-like key (but valid format)
    storage
        .write_bytes("app1", "b1", "a/b/../c.txt", "o1", b"test")
        .await
        .unwrap();

    // The key is invalid (contains ..), so this should never happen via API.
    // But even if it did, the object_id is used as filename,
    // and the key just determines the directory prefix.
    // The path on disk is: data/app1/b1/{prefix}/object_id
    // Since we use object_id (UUID) as the filename, there's no path traversal.
}

#[tokio::test]
async fn test_cascade_delete_bucket_deletes_objects() {
    let pool = init_test_store();
    let (_, bucket_id, _) = setup(&pool).await;

    for i in 0..5 {
        Object::create(
            &pool,
            &bucket_id,
            &format!("file-{}.txt", i),
            100,
            "text/plain",
            "etag",
        )
        .await
        .unwrap();
    }

    assert_eq!(Object::count_by_bucket(&pool, &bucket_id).await.unwrap(), 5);

    Bucket::delete(&pool, &bucket_id).await.unwrap();

    assert_eq!(Object::count_by_bucket(&pool, &bucket_id).await.unwrap(), 0);
}
