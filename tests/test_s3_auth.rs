//! S3（AWS Signature V4）鉴权端到端测试。
//!
//! 这里刻意**不复用**服务端的签名实现，而是在测试里按 AWS 文档独立拼出
//! canonical request 并计算签名，避免"自证正确"。签名算法本身另有
//! `services::s3_signature` 的官方测试向量单测把关。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Utc;
use hmac::{Hmac, Mac};
use oss_server::build_router;
use oss_server::config::Config;
use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::services::storage::StorageService;
use oss_server::store::init_test_store;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::ServiceExt;

const MASTER_KEY: &str = "s3-auth-test-master-key";
const REGION: &str = "us-east-1";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn test_config(data_dir: &str) -> Config {
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
        trash_retention_days: 30,
        cleanup_interval_minutes: 0,
        cleanup_max_objects: 1000,
        cleanup_max_seconds: 60,
        startup_check_mode: oss_server::config::StartupCheckMode::Report,
        nonce_retention_seconds: 3600,
    }
}

struct Env {
    _dir: tempfile::TempDir,
    router: axum::Router,
    app_id: String,
    secret: String,
    bucket: String,
}

async fn setup() -> Env {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let storage = StorageService::new(&data_dir);
    storage.init().await.unwrap();
    let store = init_test_store();
    let (app, secret) = App::create(&store, "s3-auth-app", MASTER_KEY).await.unwrap();
    let bucket = Bucket::create(&store, &app.app_id, "s3-auth-bucket", false)
        .await
        .unwrap();
    let router = build_router(
        Arc::new(test_config(data_dir.to_str().unwrap())),
        store,
        storage,
    );
    Env {
        _dir: dir,
        router,
        app_id: app.app_id,
        secret,
        bucket: bucket.name,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn signing_key(secret: &str, date: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date);
    let k_region = hmac(&k_date, REGION);
    let k_service = hmac(&k_region, "s3");
    hmac(&k_service, "aws4_request")
}

fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b'/' if !encode_slash => out.push('/'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn canonical_query(pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn scope(access_key: &str, date: &str) -> String {
    format!("{access_key}/{date}/{REGION}/s3/aws4_request")
}

/// 构造一个 header 签名的请求（GET /，空 body）。
fn signed_list_buckets(
    access_key: &str,
    secret: &str,
    host: &str,
    amz_date: &str,
) -> Request<Body> {
    let date = &amz_date[..8];
    let credential_scope = scope(access_key, date);
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical = format!(
        "GET\n/\n\nhost:{host}\nx-amz-content-sha256:{EMPTY_SHA256}\nx-amz-date:{amz_date}\n\n{signed_headers}\n{EMPTY_SHA256}"
    );
    // StringToSign 里的 scope 是 `date/region/service/aws4_request`，不含 AccessKey。
    let sts = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{date}/{REGION}/s3/aws4_request\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let signature = hmac(&signing_key(secret, date), &sts)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    Request::builder()
        .method("GET")
        .uri("/")
        .header(header::HOST, host)
        .header("x-amz-content-sha256", EMPTY_SHA256)
        .header("x-amz-date", amz_date)
        .header(
            "Authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
            ),
        )
        .body(Body::empty())
        .unwrap()
}

/// 构造预签名 GET / 请求。
fn presigned_list_buckets(
    access_key: &str,
    secret: &str,
    host: &str,
    amz_date: &str,
    expires: i64,
) -> Request<Body> {
    let date = &amz_date[..8];
    let scope = scope(access_key, date);
    let pairs = [
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", scope.clone()),
        ("X-Amz-Date", amz_date.to_string()),
        ("X-Amz-Expires", expires.to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
    ];
    let pair_refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let query = canonical_query(&pair_refs);
    let canonical = format!("GET\n/\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
    let sts = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{date}/{REGION}/s3/aws4_request\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let signature = hmac(&signing_key(secret, date), &sts)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    Request::builder()
        .method("GET")
        .uri(format!("/?{query}&X-Amz-Signature={signature}"))
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap()
}

fn now_amz() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

async fn body_string(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn signed_header_request_lists_buckets_as_xml() {
    let env = setup().await;
    let request = signed_list_buckets(&env.app_id, &env.secret, "localhost", &now_amz());
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(body.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(body.contains("<ListAllMyBucketsResult"));
    assert!(body.contains(&format!("<ID>{}</ID>", env.app_id)));
    assert!(body.contains(&format!("<Name>{}</Name>", env.bucket)));
}

#[tokio::test]
async fn presigned_request_lists_buckets() {
    let env = setup().await;
    let request = presigned_list_buckets(&env.app_id, &env.secret, "localhost", &now_amz(), 300);
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&format!("<Name>{}</Name>", env.bucket)));
}

#[tokio::test]
async fn wrong_secret_returns_signature_does_not_match() {
    let env = setup().await;
    let request = signed_list_buckets(&env.app_id, "wrong-secret", "localhost", &now_amz());
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("<Code>SignatureDoesNotMatch</Code>"));
}

#[tokio::test]
async fn unknown_access_key_returns_invalid_access_key_id() {
    let env = setup().await;
    let request = signed_list_buckets("not-an-app-id", &env.secret, "localhost", &now_amz());
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("<Code>InvalidAccessKeyId</Code>"));
}

#[tokio::test]
async fn expired_presigned_url_is_rejected() {
    let env = setup().await;
    let old = (Utc::now() - chrono::Duration::hours(2))
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let request = presigned_list_buckets(&env.app_id, &env.secret, "localhost", &old, 60);
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("<Code>AccessDenied</Code>"));
    assert!(body.contains("Request has expired"));
}

#[tokio::test]
async fn skewed_header_request_is_rejected() {
    let env = setup().await;
    let old = (Utc::now() - chrono::Duration::hours(2))
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let request = signed_list_buckets(&env.app_id, &env.secret, "localhost", &old);
    let response = env.router.clone().oneshot(request).await.unwrap();
    let (status, body) = body_string(response).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("<Code>RequestTimeTooSkewed</Code>"));
}

#[tokio::test]
async fn root_without_credentials_still_redirects_to_admin() {
    let env = setup().await;
    let request = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(request).await.unwrap();
    assert!(response.status().is_redirection());
}
