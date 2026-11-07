//! S3 兼容层端到端测试：真实 SigV4 签名 + 主要业务链路。
//!
//! 覆盖：建桶/删桶/列桶、上传/下载/删除/复制、ListObjectsV2 分页与前缀、
//! 批量删除、分片上传全流程、预签名 GET/PUT、Content-MD5、元数据 sidecar。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use oss_server::build_router;
use oss_server::config::{Config, StartupCheckMode};
use oss_server::models::app::App;
use oss_server::models::bucket::Bucket;
use oss_server::services::maintenance::MaintenanceService;
use oss_server::services::storage::StorageService;
use oss_server::store::{init_test_store, StoreHandle};

const MASTER_KEY: &str = "s3-api-test-master-key";
const REGION: &str = "us-east-1";

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
        startup_check_mode: StartupCheckMode::Report,
        nonce_retention_seconds: 3600,
    }
}

// ---------------------------------------------------------------------------
// 测试用 SigV4 客户端（不复用服务端签名实现）
// ---------------------------------------------------------------------------

struct S3 {
    _dir: tempfile::TempDir,
    router: axum::Router,
    store: StoreHandle,
    storage: StorageService,
    app_id: String,
    secret: String,
    bucket: String,
    bucket_id: String,
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn md5_base64(data: &[u8]) -> String {
    use base64::Engine;
    let digest = md5::Md5::digest(data);
    base64::engine::general_purpose::STANDARD.encode(digest)
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

fn canonical_query(pairs: &[(String, String)]) -> String {
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

fn now_amz() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn setup() -> S3 {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let storage = StorageService::new(&data_dir);
    storage.init().await.unwrap();
    let store = init_test_store();
    let (app, secret) = App::create(&store, "s3-api-app", MASTER_KEY).await.unwrap();
    let bucket = Bucket::create(&store, &app.app_id, "s3-api-bucket", false)
        .await
        .unwrap();
    let router = build_router(
        Arc::new(test_config(data_dir.to_str().unwrap())),
        store.clone(),
        storage.clone(),
    );
    // 临时目录随 S3 结构体一起释放。
    S3 {
        _dir: dir,
        router,
        store,
        storage,
        app_id: app.app_id,
        secret,
        bucket: bucket.name,
        bucket_id: bucket.bucket_id,
    }
}

impl S3 {
    /// 发送一个 header 签名的请求。
    async fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> (StatusCode, HeaderMap, String) {
        let amz_date = now_amz();
        let date = amz_date[..8].to_string();
        let payload_hash = sha256_hex(body);
        let pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let query_string = canonical_query(&pairs);

        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), "localhost".to_string()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        for (name, value) in extra_headers {
            headers.push((name.to_ascii_lowercase(), value.to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        let signed_headers = headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}:{}", normalize(value)))
            .collect::<Vec<_>>()
            .join("\n");
        let canonical = format!(
            "{method}\n{path}\n{query_string}\n{canonical_headers}\n\n{signed_headers}\n{payload_hash}"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{date}/{REGION}/s3/aws4_request\n{}",
            sha256_hex(canonical.as_bytes())
        );
        let signature: String = hmac(&signing_key(&self.secret, &date), &string_to_sign)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{REGION}/s3/aws4_request, SignedHeaders={signed_headers}, Signature={signature}",
            self.app_id, date
        );

        let uri = if query_string.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query_string}")
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", authorization);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        let request = builder.body(Body::from(body.to_vec())).unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 发送一个预签名 URL 请求（只签 host）。
    async fn send_presigned(
        &self,
        method: &str,
        path: &str,
        expires: i64,
        body: &[u8],
    ) -> (StatusCode, HeaderMap, String) {
        let amz_date = now_amz();
        let date = amz_date[..8].to_string();
        let scope = format!("{}/{}/s3/aws4_request", date, REGION);
        let mut pairs: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".to_string(), format!("{}/{}", self.app_id, scope)),
            ("X-Amz-Date".to_string(), amz_date.clone()),
            ("X-Amz-Expires".to_string(), expires.to_string()),
            ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
        ];
        pairs.sort();
        let query = canonical_query(&pairs);
        let canonical = format!("{method}\n{path}\n{query}\nhost:localhost\n\nhost\nUNSIGNED-PAYLOAD");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical.as_bytes())
        );
        let signature: String = hmac(&signing_key(&self.secret, &date), &string_to_sign)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let request = Request::builder()
            .method(method)
            .uri(format!("{path}?{query}&X-Amz-Signature={signature}"))
            .header(header::HOST, "localhost")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn etag_of(body: &[u8]) -> String {
    let digest = md5::Md5::digest(body);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bucket_lifecycle_and_object_crud() {
    let env = setup().await;

    // ---- 建桶 / Head / 列桶 / 重名 ----
    let (status, _, body) = env.send("PUT", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, headers, _) = env.send("HEAD", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("x-amz-bucket-region").unwrap().to_str().unwrap(),
        "us-east-1"
    );
    let (status, _, body) = env.send("GET", "/", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<Name>crud-bucket</Name>"));
    // 同一个 App 重复建桶
    let (status, _, body) = env.send("PUT", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("<Code>BucketAlreadyOwnedByYou</Code>"));

    // ---- PutObject（含 meta + Content-MD5）----
    let payload = b"hello s3 world";
    let (status, headers, body) = env
        .send(
            "PUT",
            "/crud-bucket/greeting.txt",
            &[],
            &[
                ("content-type", "text/plain"),
                ("x-amz-meta-color", "blue"),
                ("content-md5", &md5_base64(payload)),
            ],
            payload,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        headers.get(header::ETAG).unwrap().to_str().unwrap(),
        format!("\"{}\"", etag_of(payload))
    );

    // ---- GetObject：内容、ETag、sidecar 元数据 ----
    let (status, headers, body) = env
        .send("GET", "/crud-bucket/greeting.txt", &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello s3 world");
    assert_eq!(
        headers.get("x-amz-meta-color").unwrap().to_str().unwrap(),
        "blue"
    );
    assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "14");

    // ---- HeadObject：有长度、无 body ----
    let (status, headers, body) = env
        .send("HEAD", "/crud-bucket/greeting.txt", &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "14");
    assert!(body.is_empty());

    // ---- Range + 条件请求 ----
    let (status, headers, body) = env
        .send(
            "GET",
            "/crud-bucket/greeting.txt",
            &[],
            &[("range", "bytes=0-4")],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, "hello");
    assert_eq!(headers.get(header::CONTENT_RANGE).unwrap(), "bytes 0-4/14");

    let etag = format!("\"{}\"", etag_of(payload));
    let (status, _, _) = env
        .send(
            "GET",
            "/crud-bucket/greeting.txt",
            &[],
            &[("if-none-match", &etag)],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    let (status, _, _) = env
        .send(
            "GET",
            "/crud-bucket/greeting.txt",
            &[],
            &[("if-match", "\"deadbeef\"")],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);

    // ---- CopyObject（保留源元数据）----
    let source = "/crud-bucket/greeting.txt";
    let (status, _, body) = env
        .send(
            "PUT",
            "/crud-bucket/copied.txt",
            &[],
            &[("x-amz-copy-source", source)],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(body.contains("<CopyObjectResult"));
    let (_, headers, body) = env
        .send("GET", "/crud-bucket/copied.txt", &[], &[], &[])
        .await;
    assert_eq!(body, "hello s3 world");
    assert_eq!(
        headers.get("x-amz-meta-color").unwrap().to_str().unwrap(),
        "blue"
    );

    // ---- DeleteObject：204 + 再删仍 204 + 列表不可见 ----
    let (status, _, _) = env.send("DELETE", "/crud-bucket/greeting.txt", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = env
        .send("GET", "/crud-bucket/greeting.txt", &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("<Code>NoSuchKey</Code>"));
    let (status, _, _) = env.send("DELETE", "/crud-bucket/greeting.txt", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // ---- 软删除语义：进回收站；同名重传是新对象（不继承旧元数据）----
    let trash = env.store.list_trash(None, None, 0, 100).await.unwrap();
    assert!(trash.iter().any(|o| o.key == "greeting.txt"));
    let (status, _, _) = env
        .send("PUT", "/crud-bucket/greeting.txt", &[], &[], b"second")
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, headers, body) = env
        .send("GET", "/crud-bucket/greeting.txt", &[], &[], &[])
        .await;
    assert_eq!(body, "second");
    assert!(headers.get("x-amz-meta-color").is_none());

    // ---- DeleteObjects（批量，含不存在 key）----
    env.send("PUT", "/crud-bucket/b1.txt", &[], &[], b"1").await;
    env.send("PUT", "/crud-bucket/b2.txt", &[], &[], b"2").await;
    let delete_body = "<Delete><Object><Key>b1.txt</Key></Object><Object><Key>b2.txt</Key></Object><Object><Key>missing.txt</Key></Object></Delete>";
    let (status, _, body) = env
        .send(
            "POST",
            "/crud-bucket",
            &[("delete", "")],
            &[("content-type", "application/xml")],
            delete_body.as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body.matches("<Deleted>").count(), 3);
    let (_, _, body) = env
        .send(
            "GET",
            "/crud-bucket",
            &[("list-type", "2"), ("prefix", "b")],
            &[],
            &[],
        )
        .await;
    assert!(!body.contains("<Key>b1.txt</Key>"));
    assert!(!body.contains("<Key>b2.txt</Key>"));

    // ---- 删桶：非空拒绝，清空后成功 ----
    let (status, _, body) = env.send("DELETE", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("<Code>BucketNotEmpty</Code>"));

    for key in ["copied.txt", "greeting.txt"] {
        env.send("DELETE", &format!("/crud-bucket/{key}"), &[], &[], &[])
            .await;
    }
    let (status, _, _) = env.send("DELETE", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = env.send("HEAD", "/crud-bucket", &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleted_bucket_reports_no_such_bucket_for_location_and_head() {
    let env = setup().await;
    let b = &env.bucket;

    // 桶存在时 GetBucketLocation 正常返回。
    let (status, _, body) = env
        .send("GET", &format!("/{b}"), &[("location", "")], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(body.contains("<LocationConstraint"));

    let (status, _, _) = env.send("DELETE", &format!("/{b}"), &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 桶删除后，GetBucketLocation / HeadBucket 都必须返回 NoSuchBucket。
    let (status, _, body) = env
        .send("GET", &format!("/{b}"), &[("location", "")], &[], &[])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
    assert!(body.contains("<Code>NoSuchBucket</Code>"), "body={body}");

    let (status, _, _) = env.send("HEAD", &format!("/{b}"), &[], &[], &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_objects_v2_pagination_prefix_and_delimiter() {
    let env = setup().await;
    let b = &env.bucket;
    for (key, content) in [
        ("a/1.txt", "1"),
        ("a/2.txt", "2"),
        ("b/1.txt", "3"),
        ("root.txt", "4"),
    ] {
        let (status, _, _) = env
            .send("PUT", &format!("/{b}/{key}"), &[], &[], content.as_bytes())
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    // 前缀
    let (_, _, body) = env
        .send(
            "GET",
            &format!("/{b}"),
            &[("list-type", "2"), ("prefix", "a/")],
            &[],
            &[],
        )
        .await;
    assert!(body.contains("<Key>a/1.txt</Key>"));
    assert!(body.contains("<Key>a/2.txt</Key>"));
    assert!(!body.contains("<Key>root.txt</Key>"));
    assert_eq!(tag(&body, "KeyCount").unwrap(), "2");

    // 分页：第一页 2 条，第二页用 continuation-token
    let (_, _, page1) = env
        .send(
            "GET",
            &format!("/{b}"),
            &[("list-type", "2"), ("max-keys", "2")],
            &[],
            &[],
        )
        .await;
    assert_eq!(tag(&page1, "KeyCount").unwrap(), "2");
    assert_eq!(tag(&page1, "IsTruncated").unwrap(), "true");
    let token = tag(&page1, "NextContinuationToken").unwrap();
    let (_, _, page2) = env
        .send(
            "GET",
            &format!("/{b}"),
            &[("list-type", "2"), ("max-keys", "2"), ("continuation-token", &token)],
            &[],
            &[],
        )
        .await;
    assert_eq!(tag(&page2, "KeyCount").unwrap(), "2");
    assert_eq!(tag(&page2, "IsTruncated").unwrap(), "false");

    // delimiter 分组
    let (_, _, body) = env
        .send(
            "GET",
            &format!("/{b}"),
            &[("list-type", "2"), ("delimiter", "/")],
            &[],
            &[],
        )
        .await;
    assert!(body.contains("<Prefix>a/</Prefix>"));
    assert!(body.contains("<Prefix>b/</Prefix>"));
    assert!(body.contains("<Key>root.txt</Key>"));
    assert!(!body.contains("<Key>a/1.txt</Key>"));

    // start-after
    let (_, _, body) = env
        .send(
            "GET",
            &format!("/{b}"),
            &[("list-type", "2"), ("start-after", "b/1.txt")],
            &[],
            &[],
        )
        .await;
    assert_eq!(tag(&body, "KeyCount").unwrap(), "1");
}

#[tokio::test]
async fn multipart_upload_roundtrip_and_abort() {
    let env = setup().await;
    let b = &env.bucket;
    let key = "big/file.bin";

    // 初始化
    let (status, _, body) = env
        .send("POST", &format!("/{b}/{key}"), &[("uploads", "")], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let upload_id = tag(&body, "UploadId").unwrap();

    // 上传两个分片
    let part1 = b"hello ";
    let part2 = b"multipart";
    let (status, headers, _) = env
        .send(
            "PUT",
            &format!("/{b}/{key}"),
            &[("partNumber", "1"), ("uploadId", &upload_id)],
            &[],
            part1,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let etag1 = headers.get(header::ETAG).unwrap().to_str().unwrap().to_string();
    let (status, headers, _) = env
        .send(
            "PUT",
            &format!("/{b}/{key}"),
            &[("partNumber", "2"), ("uploadId", &upload_id)],
            &[],
            part2,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let etag2 = headers.get(header::ETAG).unwrap().to_str().unwrap().to_string();
    assert_eq!(etag1, format!("\"{}\"", etag_of(part1)));

    // 列分片
    let (status, _, body) = env
        .send(
            "GET",
            &format!("/{b}/{key}"),
            &[("uploadId", &upload_id)],
            &[],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.matches("<Part>").count(), 2);

    // 完成
    let complete = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{etag1}</ETag></Part>\
         <Part><PartNumber>2</PartNumber><ETag>{etag2}</ETag></Part></CompleteMultipartUpload>"
    );
    let (status, _, body) = env
        .send(
            "POST",
            &format!("/{b}/{key}"),
            &[("uploadId", &upload_id)],
            &[("content-type", "application/xml")],
            complete.as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let final_etag = tag(&body, "ETag").unwrap();
    assert!(final_etag.contains("-2"), "etag={final_etag}");

    // 下载合并后的对象
    let (status, headers, body) = env
        .send("GET", &format!("/{b}/{key}"), &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello multipart");
    assert_eq!(headers.get(header::CONTENT_LENGTH).unwrap(), "15");

    // Abort 流程
    let (_, _, body) = env
        .send("POST", &format!("/{b}/abort.bin"), &[("uploads", "")], &[], &[])
        .await;
    let abort_id = tag(&body, "UploadId").unwrap();
    env.send(
        "PUT",
        &format!("/{b}/abort.bin"),
        &[("partNumber", "1"), ("uploadId", &abort_id)],
        &[],
        b"discard",
    )
    .await;
    let (status, _, _) = env
        .send(
            "DELETE",
            &format!("/{b}/abort.bin"),
            &[("uploadId", &abort_id)],
            &[],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = env
        .send(
            "GET",
            &format!("/{b}/abort.bin"),
            &[("uploadId", &abort_id)],
            &[],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("<Code>NoSuchUpload</Code>"));
}

#[tokio::test]
async fn presigned_get_and_put() {
    let env = setup().await;
    let path = format!("/{}/presigned.txt", env.bucket);

    let (status, _, _) = env.send_presigned("PUT", &path, 300, b"presigned body").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = env.send_presigned("GET", &path, 300, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "presigned body");

    let (status, _, _) = env.send_presigned("GET", &path, -60, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn content_md5_mismatch_is_rejected_without_creating_object() {
    let env = setup().await;
    let (status, _, body) = env
        .send(
            "PUT",
            &format!("/{}/bad.txt", env.bucket),
            &[],
            &[("content-md5", &md5_base64(b"other"))],
            b"payload",
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("<Code>BadDigest</Code>"));
    let (status, _, _) = env
        .send("GET", &format!("/{}/bad.txt", env.bucket), &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unicode_and_space_object_keys_roundtrip() {
    let env = setup().await;
    let b = &env.bucket;
    // "中文 文件.txt" 的 percent-encoding。
    let path = format!("/{b}/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.txt");

    let (status, _, body) = env
        .send(
            "PUT",
            &path,
            &[],
            &[("content-type", "text/plain; charset=utf-8")],
            "中文内容".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (status, _, body) = env.send("GET", &path, &[], &[], &[]).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body, "中文内容");
}

#[tokio::test]
async fn upload_part_content_md5_is_checked_when_provided() {
    let env = setup().await;
    let b = env.bucket.clone();
    let key = "md5/part.bin";
    let (status, _, body) = env
        .send("POST", &format!("/{b}/{key}"), &[("uploads", "")], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let upload_id = tag(&body, "UploadId").unwrap();
    let part = b"part md5 body";

    // 1) 正确的 Content-MD5：上传成功，ETag 仍是分片内容的 MD5。
    let (status, headers, body) = env
        .send(
            "PUT",
            &format!("/{b}/{key}"),
            &[("partNumber", "1"), ("uploadId", &upload_id)],
            &[("content-md5", &md5_base64(part))],
            part,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let etag1 = headers.get(header::ETAG).unwrap().to_str().unwrap().to_string();
    assert_eq!(etag1, format!("\"{}\"", etag_of(part)));

    // 2) 错误的 Content-MD5：400 BadDigest，且该分片不写入 upload。
    let (status, _, body) = env
        .send(
            "PUT",
            &format!("/{b}/{key}"),
            &[("partNumber", "2"), ("uploadId", &upload_id)],
            &[("content-md5", &md5_base64(b"other"))],
            part,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(body.contains("<Code>BadDigest</Code>"), "body={body}");

    // 3) 不带 Content-MD5：行为不变，仍然成功。
    let (status, headers, body) = env
        .send(
            "PUT",
            &format!("/{b}/{key}"),
            &[("partNumber", "3"), ("uploadId", &upload_id)],
            &[],
            part,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        headers.get(header::ETAG).unwrap().to_str().unwrap(),
        format!("\"{}\"", etag_of(part))
    );

    // ListParts 只记录分片 1 和 3，失败的 2 已被回滚。
    let (status, _, body) = env
        .send(
            "GET",
            &format!("/{b}/{key}"),
            &[("uploadId", &upload_id)],
            &[],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.matches("<Part>").count(), 2, "body={body}");
    assert!(!body.contains("<PartNumber>2</PartNumber>"), "body={body}");

    let (status, _, _) = env
        .send(
            "DELETE",
            &format!("/{b}/{key}"),
            &[("uploadId", &upload_id)],
            &[],
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn s3_bucket_name_rules_apply_only_to_new_buckets() {
    let env = setup().await;

    for name in ["192.168.1.1", "my..bucket", "UPPERCASE", "ab"] {
        let (status, _, body) = env.send("PUT", &format!("/{name}"), &[], &[], &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "name={name}");
        assert!(body.contains("<Code>InvalidBucketName</Code>"), "name={name}");
    }

    // 存量桶（旧接口允许下划线）在 S3 下仍可正常读写。
    let legacy = Bucket::create(&env.store, &env.app_id, "legacy_bucket", false)
        .await
        .unwrap();
    let (status, _, _) = env
        .send("PUT", "/legacy_bucket/keep.txt", &[], &[], b"ok")
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = env
        .send("HEAD", "/legacy_bucket", &[], &[], &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = env
        .send("GET", "/legacy_bucket/keep.txt", &[], &[], &[])
        .await;
    assert_eq!(body, "ok");
    assert_eq!(legacy.app_id, env.app_id);
}

#[tokio::test]
async fn sidecar_lifecycle_follows_object_file() {
    let env = setup().await;
    let b = &env.bucket;
    let key = "sidecar.txt";

    env.send(
        "PUT",
        &format!("/{b}/{key}"),
        &[],
        &[("x-amz-meta-owner", "tester")],
        b"data",
    )
    .await;
    let obj = oss_server::models::object::Object::get_by_bucket_and_key(
        &env.store,
        &env.bucket_id,
        key,
    )
    .await
    .unwrap()
    .unwrap();
    let meta_path = env
        .storage
        .meta_path(&obj.app_id, &obj.bucket_id, key, &obj.object_id);
    assert!(meta_path.exists());

    // 删除磁盘文件时 sidecar 一起删除（彻底删除路径）。
    env.storage
        .delete(&obj.app_id, &obj.bucket_id, key, &obj.object_id)
        .await
        .unwrap();
    assert!(!meta_path.exists());

    // 孤儿 sidecar（对象文件已不存在）应被启动自检清理。
    let orphan = env.storage.object_path(&env.app_id, &env.bucket_id, key, "orphan-id");
    let orphan_meta = env
        .storage
        .meta_path(&env.app_id, &env.bucket_id, key, "orphan-id");
    tokio::fs::create_dir_all(orphan.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&orphan_meta, b"{\"a\":\"b\"}").await.unwrap();
    assert!(!orphan.exists());

    let config = test_config(env.storage.base_dir().to_str().unwrap());
    let maintenance = MaintenanceService::new(env.store.clone(), env.storage.clone(), &config);
    maintenance
        .startup_check(StartupCheckMode::Fix, "test")
        .await
        .unwrap();
    assert!(!orphan_meta.exists(), "orphan sidecar should be removed");

    // 分片上传目录不属于对象数据，遵循 `multipart` 排除规则。
    let temp_part = env.storage.base_dir().join("multipart/upload-1/1.part");
    tokio::fs::create_dir_all(temp_part.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&temp_part, b"chunk").await.unwrap();
    maintenance
        .startup_check(StartupCheckMode::Fix, "test")
        .await
        .unwrap();
    assert!(temp_part.exists(), "multipart temp files must not be purged");
}
