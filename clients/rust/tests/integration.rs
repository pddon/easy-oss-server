//! 集成测试：针对真实 OSS Server 实例跑通主要链路。
//!
//! 运行前需要：
//! 1. 启动服务端（默认约定端口 18100）；
//! 2. 通过管理 API 创建 App，或直接提供凭证；
//! 3. 设置环境变量：
//!    - `OSS_TEST_ENDPOINT`（必填，例如 `http://127.0.0.1:18100`）
//!    - `OSS_TEST_ACCESS_KEY` / `OSS_TEST_SECRET_KEY`（已创建 App 的凭证）
//!    - 或 `OSS_TEST_ADMIN_TOKEN`（测试会用管理 API 自动创建 App）
//!
//! 缺少环境变量时测试会打印 SKIP 并直接返回，不会伪造通过。

use std::collections::BTreeMap;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::Utc;
use md5::{Digest, Md5};
use oss_s3_client::signing::{self, ALGORITHM, SERVICE, TERMINATOR};
use oss_s3_client::{
    ClientConfig, CompletedPart, Error, MetadataDirective, OssClient, PutObjectOptions, Range,
};

macro_rules! client_or_skip {
    () => {
        match make_client().await {
            Some(client) => client,
            None => {
                eprintln!(
                    "SKIP: 未提供 OSS_TEST_ENDPOINT / OSS_TEST_ACCESS_KEY / OSS_TEST_SECRET_KEY（或 OSS_TEST_ADMIN_TOKEN）"
                );
                return;
            }
        }
    };
}

async fn resolve_credentials() -> Option<(String, String, String)> {
    let endpoint = std::env::var("OSS_TEST_ENDPOINT").ok()?;

    let (access_key, secret_key) = match (
        std::env::var("OSS_TEST_ACCESS_KEY"),
        std::env::var("OSS_TEST_SECRET_KEY"),
    ) {
        (Ok(access_key), Ok(secret_key)) => (access_key, secret_key),
        _ => {
            let admin_token = std::env::var("OSS_TEST_ADMIN_TOKEN").ok()?;
            let url = format!("{}/api/admin/apps", endpoint.trim_end_matches('/'));
            let response = reqwest::Client::new()
                .post(url)
                .header("X-Admin-Token", admin_token)
                .json(&serde_json::json!({ "name": "rust-sdk-integration" }))
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                eprintln!(
                    "无法通过管理 API 创建 App: HTTP {} {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                );
                return None;
            }
            let body: serde_json::Value = response.json().await.ok()?;
            (
                body.get("app_id")?.as_str()?.to_string(),
                body.get("app_secret")?.as_str()?.to_string(),
            )
        }
    };

    Some((endpoint, access_key, secret_key))
}

async fn make_client() -> Option<OssClient> {
    let (endpoint, access_key, secret_key) = resolve_credentials().await?;
    OssClient::new(endpoint, access_key, secret_key).ok()
}

fn unique_bucket(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let suffix = rand::random::<u16>();
    format!("it-{tag}-{}-{suffix}", nanos % 1_000_000_000)
}

async fn cleanup_bucket(client: &OssClient, bucket: &str) {
    if let Ok(page) = client
        .list_objects_v2(Some(bucket), None, None, None, None, None)
        .await
    {
        let keys: Vec<String> = page.items.iter().map(|object| object.key.clone()).collect();
        if !keys.is_empty() {
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            let _ = client.delete_objects(Some(bucket), &refs, false).await;
        }
    }
    let _ = client.delete_bucket(Some(bucket)).await;
}

fn service_error(error: &Error) -> &oss_s3_client::S3Error {
    error
        .service()
        .unwrap_or_else(|| panic!("期望 S3 服务端错误，实际为 {error}"))
}

fn md5_base64(data: &[u8]) -> String {
    STANDARD.encode(Md5::digest(data))
}

/// 把 endpoint 拆成 origin / Host 头 / 路径前缀。
fn endpoint_parts(endpoint: &str) -> (String, String, String) {
    let trimmed = endpoint.trim().trim_end_matches('/');
    let scheme_end = trimmed.find("://").expect("endpoint 必须包含 scheme");
    let after_scheme = scheme_end + 3;
    let (origin, base_path) = match trimmed[after_scheme..].find('/') {
        Some(index) => (
            trimmed[..after_scheme + index].to_string(),
            trimmed[after_scheme + index..].to_string(),
        ),
        None => (trimmed.to_string(), String::new()),
    };
    let host = origin
        .split_once("://")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default();
    (origin, host, base_path)
}

/// 用公开的 SigV4 辅助函数手工发送一次 UploadPart，并附带指定的 `Content-MD5`。
///
/// 公共 `upload_part` 契约不接受 MD5 参数，因此“错误分片 MD5”只能在测试里绕过 SDK 直接发请求。
#[allow(clippy::too_many_arguments)]
async fn raw_upload_part(
    endpoint: &str,
    access_key: &str,
    secret_key: &str,
    bucket: &str,
    key: &str,
    upload_id: &str,
    part_number: u32,
    body: &[u8],
    content_md5: &str,
) -> reqwest::Response {
    let (origin, host, base_path) = endpoint_parts(endpoint);
    let path = format!(
        "{}/{}/{}",
        base_path,
        signing::uri_encode(bucket, true),
        signing::uri_encode(key, false)
    );
    let query = vec![
        ("partNumber".to_string(), part_number.to_string()),
        ("uploadId".to_string(), upload_id.to_string()),
    ];
    let canonical_query = signing::canonical_query_string(&query);

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = signing::sha256_hex(body);

    let mut headers = BTreeMap::new();
    headers.insert("host".to_string(), host.clone());
    headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
    headers.insert("x-amz-date".to_string(), amz_date.clone());
    let signed_headers = [
        "host".to_string(),
        "x-amz-content-sha256".to_string(),
        "x-amz-date".to_string(),
    ];

    let canonical_request = signing::canonical_request(
        "PUT",
        &path,
        &canonical_query,
        &headers,
        &signed_headers,
        &payload_hash,
    );
    let string_to_sign =
        signing::string_to_sign(&amz_date, &date, "us-east-1", SERVICE, &canonical_request);
    let signature =
        signing::compute_signature(secret_key, &date, "us-east-1", SERVICE, &string_to_sign);
    let authorization = format!(
        "{ALGORITHM} Credential={access_key}/{date}/us-east-1/{SERVICE}/{TERMINATOR}, \
         SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}"
    );

    let url = format!("{origin}{path}?{canonical_query}");
    reqwest::Client::new()
        .put(url)
        .header("host", &host)
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", &payload_hash)
        .header("authorization", authorization)
        .header("content-md5", content_md5)
        .body(body.to_vec())
        .send()
        .await
        .expect("发送原始 UploadPart 请求失败")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_lifecycle() {
    let client = client_or_skip!();
    let bucket = unique_bucket("bucket");

    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let head = client.head_bucket(Some(bucket.as_str())).await.expect("HEAD 桶失败");
    assert!(head.exists, "新建桶 HEAD 应存在");
    assert_eq!(head.region, "us-east-1");

    assert!(client.bucket_exists(Some(bucket.as_str())).await.expect("bucket_exists 失败"));

    let location = client
        .get_bucket_location(Some(bucket.as_str()))
        .await
        .expect("获取 location 失败");
    assert_eq!(location, "us-east-1", "空 location 应归一化为 us-east-1");

    let buckets = client.list_buckets().await.expect("ListBuckets 失败");
    let item = buckets
        .iter()
        .find(|item| item.name == bucket)
        .expect("ListBuckets 未包含新建桶");
    assert!(!item.created_at.is_empty(), "桶应包含 created_at");

    client.delete_bucket(Some(bucket.as_str())).await.expect("删除桶失败");
    assert!(!client.bucket_exists(Some(bucket.as_str())).await.expect("bucket_exists 失败"));

    let head = client.head_bucket(Some(bucket.as_str())).await.expect("HEAD 桶不应报错");
    assert!(!head.exists, "删除后桶应不存在");
    assert_eq!(head.region, "us-east-1", "缺失响应头时回退配置 region");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_crud_metadata_range_copy_md5_and_errors() {
    let client = client_or_skip!();
    let bucket = unique_bucket("object");
    let key = "docs/hello.txt";
    let content = b"hello world";

    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let mut metadata = BTreeMap::new();
    metadata.insert("owner".to_string(), "rust-sdk".to_string());
    metadata.insert("env".to_string(), "integration".to_string());

    let options = PutObjectOptions::new()
        .with_content_type("text/plain; charset=utf-8")
        .with_metadata("owner", "rust-sdk")
        .with_metadata("env", "integration")
        .with_content_md5(md5_base64(content));
    let put = client
        .put_object(Some(bucket.as_str()), key, content.to_vec(), &options)
        .await
        .expect("put_object 失败");
    assert!(!put.etag.is_empty(), "PUT 应返回 ETag");

    let got = client
        .get_object(Some(bucket.as_str()), key, None)
        .await
        .expect("get_object 失败");
    assert_eq!(got, content.to_vec());
    assert_eq!(put.size, content.len() as u64);

    let head = client.head_object(Some(bucket.as_str()), key).await.expect("HEAD 失败");
    assert_eq!(head.size, content.len() as u64);
    assert_eq!(head.etag, put.etag);
    assert_eq!(head.content_type, "text/plain; charset=utf-8");
    assert_eq!(head.metadata, metadata);
    assert!(head.last_modified.is_some(), "HEAD 应包含 last_modified");
    assert!(client.object_exists(Some(bucket.as_str()), key).await.expect("object_exists 失败"));

    let range = client
        .get_object(Some(bucket.as_str()), key, Some(Range::FromTo(6, 10)))
        .await
        .expect("Range GET 失败");
    assert_eq!(range, b"world".to_vec());

    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let download = dir.path().join("download").join("hello.txt");
    client
        .get_object_to_file(Some(bucket.as_str()), key, &download, None)
        .await
        .expect("下载到文件失败");
    assert_eq!(std::fs::read(&download).unwrap(), content);

    let copy_key = "docs/copy.txt";
    let copied = client
        .copy_object(Some(bucket.as_str()), key, Some(bucket.as_str()), copy_key, &BTreeMap::new(), None)
        .await
        .expect("默认 COPY 失败");
    assert!(!copied.etag.is_empty());
    let head = client.head_object(Some(bucket.as_str()), copy_key).await.expect("HEAD copy 失败");
    assert_eq!(head.metadata, metadata, "未传 metadata 时应按 COPY 保留元数据");

    let mut replaced_metadata = BTreeMap::new();
    replaced_metadata.insert("new-owner".to_string(), "copy".to_string());
    client
        .copy_object(Some(bucket.as_str()), key, Some(bucket.as_str()), copy_key, &replaced_metadata, None)
        .await
        .expect("推导 REPLACE 失败");
    let head = client.head_object(Some(bucket.as_str()), copy_key).await.expect("HEAD copy 失败");
    assert_eq!(head.metadata, replaced_metadata, "传了 metadata 应推导为 REPLACE");

    client
        .copy_object(
            Some(bucket.as_str()),
            key,
            Some(bucket.as_str()),
            copy_key,
            &replaced_metadata,
            Some(MetadataDirective::Copy),
        )
        .await
        .expect("显式 COPY 失败");
    let head = client.head_object(Some(bucket.as_str()), copy_key).await.expect("HEAD copy 失败");
    assert_eq!(head.metadata, metadata, "显式 COPY 应复制源对象元数据");

    let bad_md5_options = PutObjectOptions::new()
        .with_content_type("text/plain")
        .with_content_md5(md5_base64(b"other content"));
    let error = client
        .put_object(Some(bucket.as_str()), "docs/bad-md5.txt", content.to_vec(), &bad_md5_options)
        .await
        .expect_err("错误 Content-MD5 应失败");
    assert_eq!(service_error(&error).status, 400);
    assert_eq!(service_error(&error).code, "BadDigest");

    let missing = client
        .get_object(Some(bucket.as_str()), "docs/missing.txt", None)
        .await
        .expect_err("不存在的对象应返回 404");
    assert!(missing.is_not_found());
    let s3 = service_error(&missing);
    assert_eq!(s3.status, 404);
    assert_eq!(s3.code, "NoSuchKey");
    assert!(!s3.message.is_empty(), "错误应包含 message");
    assert_eq!(s3.request_id.as_deref(), Some("unknown"));

    client.delete_object(Some(bucket.as_str()), key).await.expect("删除对象失败");
    assert!(!client.object_exists(Some(bucket.as_str()), key).await.expect("object_exists 失败"));

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_objects_v2_pagination_prefix_delimiter_start_after() {
    let client = client_or_skip!();
    let bucket = unique_bucket("list");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    for index in 0..12 {
        client
            .put_object(Some(bucket.as_str()), &format!("list/{index:02}.txt"), b"x".to_vec(), &PutObjectOptions::default())
            .await
            .expect("上传列表对象失败");
    }
    for index in 0..2 {
        client
            .put_object(Some(bucket.as_str()), &format!("other/{index}.txt"), b"y".to_vec(), &PutObjectOptions::default())
            .await
            .expect("上传列表对象失败");
    }
    client
        .put_object(Some(bucket.as_str()), "root.txt", b"z".to_vec(), &PutObjectOptions::default())
        .await
        .expect("上传根对象失败");

    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let page = client
            .list_objects_v2(Some(bucket.as_str()), Some("list/"), None, Some(5), token.as_deref(), None)
            .await
            .expect("分页列表失败");
        pages += 1;
        assert!(page.items.len() <= 5);
        collected.extend(page.items.iter().map(|item| item.key.clone()));
        if page.is_truncated {
            assert!(page.next_token.is_some(), "截断时必须返回 next_token");
            token = page.next_token;
        } else {
            assert!(page.next_token.is_none(), "未截断时 next_token 应为空");
            break;
        }
    }
    assert_eq!(pages, 3, "12 个对象按每页 5 个应分 3 页");
    let expected: Vec<String> = (0..12).map(|index| format!("list/{index:02}.txt")).collect();
    assert_eq!(collected, expected);

    let page = client
        .list_objects_v2(Some(bucket.as_str()), Some("list/"), None, Some(3), None, Some("list/04.txt"))
        .await
        .expect("start_after 失败");
    assert!(page.is_truncated);
    assert_eq!(
        page.items.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        vec!["list/05.txt", "list/06.txt", "list/07.txt"]
    );

    let page = client
        .list_objects_v2(Some(bucket.as_str()), None, Some("/"), Some(1000), None, None)
        .await
        .expect("delimiter 列表失败");
    assert_eq!(
        page.items.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        vec!["root.txt"]
    );
    assert_eq!(page.common_prefixes, vec!["list/", "other/"]);
    assert!(!page.is_truncated);

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presigned_get_put_and_expiry() {
    let client = client_or_skip!();
    let bucket = unique_bucket("presign");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let key = "signed/demo.txt";
    let content = b"presigned payload";
    client
        .put_object(Some(bucket.as_str()), key, content.to_vec(), &PutObjectOptions::default())
        .await
        .expect("准备对象失败");

    let get_url = client.presign_get(Some(bucket.as_str()), key, Some(300)).expect("生成预签名 GET URL 失败");
    let response = reqwest::Client::new()
        .get(&get_url)
        .send()
        .await
        .expect("预签名 GET 请求失败");
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), content);

    let put_key = "signed/uploaded.txt";
    let put_content = b"uploaded via presigned url";
    let put_url = client.presign_put(Some(bucket.as_str()), put_key, Some(300)).expect("生成预签名 PUT URL 失败");
    let response = reqwest::Client::new()
        .put(&put_url)
        .body(put_content.to_vec())
        .send()
        .await
        .expect("预签名 PUT 请求失败");
    assert_eq!(response.status(), 200);

    let head = client.head_object(Some(bucket.as_str()), put_key).await.expect("HEAD 失败");
    assert_eq!(head.size, put_content.len() as u64);
    let got = client.get_object(Some(bucket.as_str()), put_key, None).await.expect("GET 失败");
    assert_eq!(got, put_content.to_vec());

    let expired_url = client.presign_get(Some(bucket.as_str()), key, Some(1)).expect("生成短时效 URL 失败");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let response = reqwest::Client::new().get(&expired_url).send().await.expect("请求失败");
    assert_eq!(response.status(), 403);
    let body = response.text().await.unwrap();
    assert!(body.contains("<Code>AccessDenied</Code>"), "过期预签名应返回 AccessDenied: {body}");

    assert!(matches!(
        client.presign_get(Some(bucket.as_str()), key, Some(0)),
        Err(Error::Config(_))
    ));
    assert!(matches!(
        client.presign_put(Some(bucket.as_str()), key, Some(604_801)),
        Err(Error::Config(_))
    ));

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_low_level_complete_and_abort() {
    let client = client_or_skip!();
    let bucket = unique_bucket("mpu");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let key = "multipart/hello.txt";
    let mut metadata = BTreeMap::new();
    metadata.insert("source".to_string(), "multipart".to_string());

    let upload_id = client
        .create_multipart_upload(Some(bucket.as_str()), key, Some("text/plain"), &metadata)
        .await
        .expect("CreateMultipartUpload 失败");
    assert!(!upload_id.is_empty());

    let etag1 = client
        .upload_part(Some(bucket.as_str()), key, &upload_id, 1, b"hello ".to_vec())
        .await
        .expect("上传分片 1 失败");
    let etag2 = client
        .upload_part(Some(bucket.as_str()), key, &upload_id, 2, b"multipart!".to_vec())
        .await
        .expect("上传分片 2 失败");
    assert!(!etag1.is_empty() && !etag2.is_empty());

    let listed = client
        .list_parts(Some(bucket.as_str()), key, &upload_id)
        .await
        .expect("ListParts 失败");
    assert!(!listed.is_truncated);
    let parts = listed.parts;
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].part_number, 1);
    assert_eq!(parts[0].size, 6);
    assert_eq!(parts[1].part_number, 2);
    assert_eq!(parts[1].size, 10);

    // 故意乱序提交，SDK 应自动按 part_number 排序。
    let completed = vec![
        CompletedPart { part_number: 2, etag: etag2 },
        CompletedPart { part_number: 1, etag: etag1 },
    ];
    let final_etag = client
        .complete_multipart_upload(Some(bucket.as_str()), key, &upload_id, &completed)
        .await
        .expect("CompleteMultipartUpload 失败");
    assert!(final_etag.ends_with("-2"), "分片 ETag 应以 -2 结尾: {final_etag}");

    let head = client.head_object(Some(bucket.as_str()), key).await.expect("HEAD 失败");
    assert_eq!(head.size, 16);
    assert_eq!(head.content_type, "text/plain");
    assert_eq!(head.metadata.get("source").map(String::as_str), Some("multipart"));

    let got = client.get_object(Some(bucket.as_str()), key, None).await.expect("GET 失败");
    assert_eq!(got, b"hello multipart!".to_vec());

    let abort_key = "multipart/abort.txt";
    let upload_id = client
        .create_multipart_upload(Some(bucket.as_str()), abort_key, None, &BTreeMap::new())
        .await
        .expect("CreateMultipartUpload 失败");
    client
        .upload_part(Some(bucket.as_str()), abort_key, &upload_id, 1, b"temp".to_vec())
        .await
        .expect("上传分片失败");
    client
        .abort_multipart_upload(Some(bucket.as_str()), abort_key, &upload_id)
        .await
        .expect("AbortMultipartUpload 失败");

    let error = client
        .list_parts(Some(bucket.as_str()), abort_key, &upload_id)
        .await
        .expect_err("abort 后 ListParts 应返回 404");
    assert_eq!(service_error(&error).code, "NoSuchUpload");
    assert_eq!(service_error(&error).status, 404);

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_part_rejects_bad_content_md5() {
    let client = client_or_skip!();
    // 直接复用客户端的凭证/endpoint：桶归属于该 App，重新 resolve 会拿到另一个 App。
    let endpoint = client.config().endpoint.clone();
    let access_key = client.config().access_key.clone();
    let secret_key = client.config().secret_key.clone();
    let bucket = unique_bucket("partmd5");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let key = "md5/part.bin";
    let upload_id = client
        .create_multipart_upload(Some(bucket.as_str()), key, None, &BTreeMap::new())
        .await
        .expect("CreateMultipartUpload 失败");

    let part_body = b"first part payload";
    let response = raw_upload_part(
        &endpoint,
        &access_key,
        &secret_key,
        &bucket,
        key,
        &upload_id,
        1,
        part_body,
        &md5_base64(part_body),
    )
    .await;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 200,
        "正确分片 Content-MD5 应被接受（否则服务端 UploadPart 校验状态异常）: {text}"
    );

    // 故意给第 2 片错误的 MD5：服务端逐片校验必须真实生效，返回 400 BadDigest。
    let bad_body = b"second part payload";
    let response = raw_upload_part(
        &endpoint,
        &access_key,
        &secret_key,
        &bucket,
        key,
        &upload_id,
        2,
        bad_body,
        &md5_base64(b"intentionally wrong digest input"),
    )
    .await;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 400,
        "错误分片 Content-MD5 必须返回 400（若为 2xx 说明服务端 UploadPart 校验未生效）: {text}"
    );
    assert!(
        text.contains("<Code>BadDigest</Code>"),
        "错误码应为 BadDigest: {text}"
    );

    client
        .abort_multipart_upload(Some(bucket.as_str()), key, &upload_id)
        .await
        .expect("AbortMultipartUpload 失败");
    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_file_and_put_object_auto_multipart() {
    let client = client_or_skip!();
    let bucket = unique_bucket("auto");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");
    let dir = tempfile::tempdir().expect("创建临时目录失败");

    // upload_file：9 MB + 123 字节，默认 8 MB 分片 -> 2 片。
    let big = vec![b'a'; 9 * 1024 * 1024 + 123];
    let big_path = dir.path().join("big.bin");
    std::fs::write(&big_path, &big).expect("写入大文件失败");
    let output = client
        .upload_file(
            Some(bucket.as_str()),
            "auto/big.bin",
            &big_path,
            &PutObjectOptions::new()
                .with_content_type("application/octet-stream")
                .with_content_md5(md5_base64(&big)),
        )
        .await
        .expect("upload_file 失败（整对象 MD5 本地校验应通过）");
    assert!(output.etag.ends_with("-2"), "9MB 文件应自动分 2 片: {}", output.etag);
    assert_eq!(output.size, big.len() as u64, "upload_file 应返回 size");

    let head = client.head_object(Some(bucket.as_str()), "auto/big.bin").await.expect("HEAD 失败");
    assert_eq!(head.size, big.len() as u64);
    let download = dir.path().join("downloaded.bin");
    client
        .get_object_to_file(Some(bucket.as_str()), "auto/big.bin", &download, None)
        .await
        .expect("下载大文件失败");
    assert_eq!(std::fs::read(&download).unwrap(), big, "大文件内容应逐字节一致");

    // put_object：2.5 MB 字节，part_size = 1 MB -> 3 片。
    let data = vec![b'b'; 2 * 1024 * 1024 + 512 * 1024];
    let output = client
        .put_object(
            Some(bucket.as_str()),
            "auto/bytes.bin",
            data.clone(),
            &PutObjectOptions::new()
                .with_part_size(1024 * 1024)
                .with_content_md5(md5_base64(&data)),
        )
        .await
        .expect("put_object 自动分片失败（整对象 MD5 本地校验应通过）");
    assert!(output.etag.ends_with("-3"), "2.5MB / 1MB 应分 3 片: {}", output.etag);
    assert_eq!(output.size, data.len() as u64);
    let got = client.get_object(Some(bucket.as_str()), "auto/bytes.bin", None).await.expect("GET 失败");
    assert_eq!(got, data);

    // 自动分片 + 错误的整对象 MD5：本地校验失败返回 BadDigest，且不上传任何分片。
    let error = client
        .put_object(
            Some(bucket.as_str()),
            "auto/bad-md5.bin",
            data.clone(),
            &PutObjectOptions::new()
                .with_part_size(1024 * 1024)
                .with_content_md5(md5_base64(b"not the object content")),
        )
        .await
        .expect_err("错误整对象 MD5 应本地失败");
    assert_eq!(service_error(&error).status, 400);
    assert_eq!(service_error(&error).code, "BadDigest");
    assert!(
        !client
            .object_exists(Some(bucket.as_str()), "auto/bad-md5.bin")
            .await
            .expect("object_exists 失败"),
        "本地校验失败不应创建对象"
    );

    // upload_file 自动分片 + 错误的整对象 MD5：同样本地失败。
    let error = client
        .upload_file(
            Some(bucket.as_str()),
            "auto/bad-file-md5.bin",
            &big_path,
            &PutObjectOptions::new().with_content_md5(md5_base64(b"not the file content")),
        )
        .await
        .expect_err("文件整对象 MD5 不匹配应本地失败");
    assert_eq!(service_error(&error).code, "BadDigest");

    // 关闭自动分片 -> 单次 PUT，ETag 不含 `-`。
    let output = client
        .put_object(
            Some(bucket.as_str()),
            "auto/single.bin",
            data.clone(),
            &PutObjectOptions::new()
                .with_part_size(1024 * 1024)
                .with_auto_multipart(false),
        )
        .await
        .expect("put_object 单次上传失败");
    assert!(!output.etag.contains('-'), "关闭自动分片后应为单次 PUT: {}", output.etag);

    // upload_file 小文件 + Content-MD5 走单次 PUT。
    let small = b"small file with md5";
    let small_path = dir.path().join("small.txt");
    std::fs::write(&small_path, small).expect("写入小文件失败");
    client
        .upload_file(
            Some(bucket.as_str()),
            "auto/small.txt",
            &small_path,
            &PutObjectOptions::new().with_content_md5(md5_base64(small)),
        )
        .await
        .expect("upload_file 小文件失败");
    let got = client.get_object(Some(bucket.as_str()), "auto/small.txt", None).await.expect("GET 失败");
    assert_eq!(got, small.to_vec());

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_objects_batches_over_1000_and_quiet() {
    let client = client_or_skip!();
    let bucket = unique_bucket("batch");
    client.create_bucket(Some(bucket.as_str())).await.expect("创建桶失败");

    let total = 1005;
    let keys: Vec<String> = (0..total).map(|index| format!("batch/{index:04}.txt")).collect();

    // 在第一页与第二页各放一个真实对象，验证分批后仍然删除成功。
    for index in [0usize, 1004] {
        client
            .put_object(Some(bucket.as_str()), &keys[index], b"payload".to_vec(), &PutObjectOptions::default())
            .await
            .expect("上传对象失败");
    }

    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let output = client
        .delete_objects(Some(bucket.as_str()), &refs, false)
        .await
        .expect("批量删除失败");
    assert_eq!(output.deleted.len(), total, "超过 1000 个 key 应自动分批并全部删除");
    assert!(output.errors.is_empty(), "批量删除不应有错误: {:?}", output.errors);

    for index in [0usize, 1004] {
        assert!(!client.object_exists(Some(bucket.as_str()), &keys[index]).await.expect("object_exists 失败"));
    }

    // quiet = true：服务端不返回 Deleted 列表。
    let quiet_keys = ["quiet/a.txt", "quiet/b.txt"];
    for key in quiet_keys {
        client
            .put_object(Some(bucket.as_str()), key, b"x".to_vec(), &PutObjectOptions::default())
            .await
            .expect("上传对象失败");
    }
    let output = client
        .delete_objects(Some(bucket.as_str()), &quiet_keys, true)
        .await
        .expect("quiet 批量删除失败");
    assert!(output.deleted.is_empty(), "quiet 模式不应返回已删除 key");
    assert!(output.errors.is_empty());
    for key in quiet_keys {
        assert!(!client.object_exists(Some(bucket.as_str()), key).await.expect("object_exists 失败"));
    }

    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_view_roundtrip_and_default_bucket() {
    let client = client_or_skip!();
    let bucket = unique_bucket("view");
    client
        .create_bucket(Some(bucket.as_str()))
        .await
        .expect("创建桶失败");

    let view = client.bucket(bucket.as_str());
    let put = view
        .put_object(
            "view/hello.txt",
            b"hello".to_vec(),
            &PutObjectOptions::default(),
        )
        .await
        .expect("视图上传失败");
    assert_eq!(put.size, 5);
    assert_eq!(
        view.get_object("view/hello.txt", None)
            .await
            .expect("视图下载失败"),
        b"hello".to_vec()
    );
    let head = view
        .head_object("view/hello.txt")
        .await
        .expect("视图 HEAD 失败");
    assert_eq!(head.size, 5);
    assert!(view
        .object_exists("view/hello.txt")
        .await
        .expect("视图 object_exists 失败"));

    let copied = view
        .copy_object("view/hello.txt", "view/copy.txt", &BTreeMap::new(), None)
        .await
        .expect("视图复制失败");
    assert!(!copied.etag.is_empty());
    let page = view
        .list_objects_v2(Some("view/"), None, None, None, None)
        .await
        .expect("视图列表失败");
    assert_eq!(page.items.len(), 2, "应看到原对象与复制对象");

    // 配置默认桶后，用同一入口省略桶名。
    let config = ClientConfig::new(
        client.config().endpoint.clone(),
        client.config().access_key.clone(),
        client.config().secret_key.clone(),
    )
    .with_max_retries(0)
    .with_default_bucket(bucket.as_str());
    let default_client = OssClient::with_config(config).expect("创建默认桶客户端失败");
    let default_view = default_client.default_bucket();
    default_view
        .put_object(
            "view/default.txt",
            b"d".to_vec(),
            &PutObjectOptions::default(),
        )
        .await
        .expect("默认桶视图上传失败");
    assert_eq!(
        default_view
            .get_object("view/default.txt", None)
            .await
            .expect("默认桶视图下载失败"),
        b"d".to_vec()
    );

    view.delete_objects(&["view/hello.txt", "view/default.txt"], false)
        .await
        .expect("视图批量删除失败");
    view.delete_object("view/copy.txt")
        .await
        .expect("视图删除失败");
    cleanup_bucket(&client, &bucket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn access_denied_for_bad_signature() {
    let Some((endpoint, access_key, _)) = resolve_credentials().await else {
        eprintln!("SKIP: 未提供测试凭证");
        return;
    };

    let bad_client = OssClient::new(endpoint, access_key, "definitely-wrong-secret")
        .expect("创建客户端失败");
    let error = bad_client
        .list_buckets()
        .await
        .expect_err("错误 secret 应返回 403");
    assert!(error.is_access_denied());
    let s3 = service_error(&error);
    assert_eq!(s3.status, 403);
    assert_eq!(s3.code, "SignatureDoesNotMatch");
    assert!(!s3.message.is_empty());
    assert_eq!(s3.request_id.as_deref(), Some("unknown"));
}
