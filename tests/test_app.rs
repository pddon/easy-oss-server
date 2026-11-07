//! Integration tests for app management.

use oss_server::store::init_test_store;
use oss_server::models::app::App;

const MASTER_KEY: &str = "test-master-key-integration";

#[tokio::test]
async fn test_app_full_lifecycle() {
    let pool = init_test_store();

    // Create
    let (app, secret) = App::create(&pool, "integration-test-app", MASTER_KEY)
        .await
        .unwrap();
    assert_eq!(app.name, "integration-test-app");
    assert!(!secret.is_empty());

    // Get
    let fetched = App::get_by_id(&pool, &app.app_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.name, "integration-test-app");

    // List
    let apps = App::list(&pool, 0, 10).await.unwrap();
    assert_eq!(apps.len(), 1);

    // Count
    let count = App::count(&pool).await.unwrap();
    assert_eq!(count, 1);

    // Reset secret
    let new_secret = App::reset_secret(&pool, &app.app_id, MASTER_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(new_secret, secret);

    // Verify new secret works
    let fetched = App::get_by_id(&pool, &app.app_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.secret(MASTER_KEY).unwrap(), new_secret);

    // Delete
    let deleted = App::delete(&pool, &app.app_id).await.unwrap();
    assert!(deleted);

    let count = App::count(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_app_pagination() {
    let pool = init_test_store();

    for i in 0..25 {
        App::create(&pool, &format!("app-{:03}", i), MASTER_KEY)
            .await
            .unwrap();
    }

    let page1 = App::list(&pool, 0, 10).await.unwrap();
    assert_eq!(page1.len(), 10);

    let page2 = App::list(&pool, 10, 10).await.unwrap();
    assert_eq!(page2.len(), 10);

    let page3 = App::list(&pool, 20, 10).await.unwrap();
    assert_eq!(page3.len(), 5);

    let total = App::count(&pool).await.unwrap();
    assert_eq!(total, 25);
}

#[tokio::test]
async fn test_app_not_found() {
    let pool = init_test_store();

    let result = App::get_by_id(&pool, "non-existent-id").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_app_delete_nonexistent() {
    let pool = init_test_store();

    let result = App::delete(&pool, "non-existent-id").await.unwrap();
    assert!(!result);
}

#[tokio::test]
async fn test_app_secret_encryption() {
    let pool = init_test_store();

    let (app, secret) = App::create(&pool, "encryption-test", MASTER_KEY)
        .await
        .unwrap();

    // The stored value should NOT be the plaintext secret
    assert_ne!(app.app_secret_encrypted, secret);

    // But we should be able to decrypt it
    let decrypted = app.secret(MASTER_KEY).unwrap();
    assert_eq!(decrypted, secret);
}
