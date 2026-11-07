//! Integration tests for bucket management.

use oss_server::store::{init_test_store, StoreHandle};
use oss_server::models::app::App;
use oss_server::models::bucket::{validate_bucket_name, Bucket};

const MASTER_KEY: &str = "test-master-key-bucket";

async fn setup_app(pool: &StoreHandle) -> String {
    let (app, _) = App::create(pool, "bucket-test-app", MASTER_KEY)
        .await
        .unwrap();
    app.app_id
}

#[tokio::test]
async fn test_bucket_create_and_get() {
    let pool = init_test_store();
    let app_id = setup_app(&pool).await;

    let bucket = Bucket::create(&pool, &app_id, "my-test-bucket", false)
        .await
        .unwrap();

    assert_eq!(bucket.name, "my-test-bucket");
    assert!(!bucket.is_public);
    assert_eq!(bucket.app_id, app_id);

    let by_id = Bucket::get_by_id(&pool, &bucket.bucket_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_id.name, "my-test-bucket");

    let by_name = Bucket::get_by_name(&pool, "my-test-bucket")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_name.bucket_id, bucket.bucket_id);

    let by_app_name = Bucket::get_by_app_and_name(&pool, &app_id, "my-test-bucket")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_app_name.bucket_id, bucket.bucket_id);
}

#[tokio::test]
async fn test_bucket_uniqueness() {
    let pool = init_test_store();
    let app_id1 = setup_app(&pool).await;
    let (app2, _) = App::create(&pool, "app2", MASTER_KEY).await.unwrap();

    // Create bucket in first app
    Bucket::create(&pool, &app_id1, "unique-bucket", false)
        .await
        .unwrap();

    // Trying to create same bucket name in second app should fail (globally unique)
    let result = Bucket::create(&pool, &app2.app_id, "unique-bucket", false).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_bucket_public_toggle() {
    let pool = init_test_store();
    let app_id = setup_app(&pool).await;

    let bucket = Bucket::create(&pool, &app_id, "toggle-test", false)
        .await
        .unwrap();
    assert!(!bucket.is_public);

    Bucket::set_public(&pool, &bucket.bucket_id, true)
        .await
        .unwrap();

    let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id)
        .await
        .unwrap()
        .unwrap();
    assert!(fetched.is_public);

    Bucket::set_public(&pool, &bucket.bucket_id, false)
        .await
        .unwrap();

    let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!fetched.is_public);
}

#[tokio::test]
async fn test_bucket_list_and_count() {
    let pool = init_test_store();
    let app_id = setup_app(&pool).await;

    for i in 0..10 {
        Bucket::create(&pool, &app_id, &format!("bucket-{:02}", i), i % 2 == 0)
            .await
            .unwrap();
    }

    let count = Bucket::count_by_app(&pool, &app_id).await.unwrap();
    assert_eq!(count, 10);

    let page = Bucket::list_by_app(&pool, &app_id, 0, 3).await.unwrap();
    assert_eq!(page.len(), 3);

    let page2 = Bucket::list_by_app(&pool, &app_id, 5, 10).await.unwrap();
    assert_eq!(page2.len(), 5);
}

#[tokio::test]
async fn test_bucket_delete() {
    let pool = init_test_store();
    let app_id = setup_app(&pool).await;

    let bucket = Bucket::create(&pool, &app_id, "delete-me", false)
        .await
        .unwrap();

    let deleted = Bucket::delete(&pool, &bucket.bucket_id).await.unwrap();
    assert!(deleted);

    let fetched = Bucket::get_by_id(&pool, &bucket.bucket_id).await.unwrap();
    assert!(fetched.is_none());
}

#[tokio::test]
async fn test_cascade_delete_app_deletes_buckets() {
    let pool = init_test_store();
    let (app, _) = App::create(&pool, "cascade-test", MASTER_KEY).await.unwrap();

    Bucket::create(&pool, &app.app_id, "cascade-b1", false)
        .await
        .unwrap();
    Bucket::create(&pool, &app.app_id, "cascade-b2", false)
        .await
        .unwrap();

    assert_eq!(Bucket::count_by_app(&pool, &app.app_id).await.unwrap(), 2);

    App::delete(&pool, &app.app_id).await.unwrap();

    assert_eq!(Bucket::count_by_app(&pool, &app.app_id).await.unwrap(), 0);
}

#[test]
fn test_validate_bucket_name_various() {
    assert!(validate_bucket_name("abc"));
    assert!(validate_bucket_name("my-bucket"));
    assert!(validate_bucket_name("test-123"));
    assert!(validate_bucket_name("a-very-long-bucket-name-that-is-valid"));
    assert!(validate_bucket_name("123-bucket"));
    assert!(validate_bucket_name("bucket-123"));

    assert!(!validate_bucket_name(""));
    assert!(!validate_bucket_name("ab"));
    assert!(!validate_bucket_name(&"a".repeat(64)));
    assert!(!validate_bucket_name("My-Bucket"));
    assert!(!validate_bucket_name("-invalid"));
    assert!(!validate_bucket_name("invalid-"));
    assert!(!validate_bucket_name("under_score"));
    assert!(!validate_bucket_name("dot.bucket"));
}
