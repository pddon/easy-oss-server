//! 不依赖真实服务端的契约行为测试：endpoint 路径前缀、元数据校验、
//! Content-MD5 请求头、重试策略。使用本地极简 HTTP 服务器捕获请求。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use pddon_oss_client::{ClientConfig, CompletedPart, Error, OssClient, PutObjectOptions};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct ResponseSpec {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}

impl ResponseSpec {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

struct TestServer {
    port: u16,
    captured: Arc<Mutex<Vec<Captured>>>,
}

impl TestServer {
    fn start(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 失败");
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();

        std::thread::spawn(move || {
            for spec in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                if let Some(request) = read_request(&mut stream) {
                    sink.lock().unwrap().push(request);
                }
                write_response(&mut stream, &spec);
            }
        });

        Self { port, captured }
    }

    fn requests(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Captured> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];

    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&buffer[..read]);
        if let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = split + 4;
            let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if raw.len() >= header_end + content_length {
                break;
            }
        }
    }

    let split = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
    let header_text = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = header_text.lines();
    let request_line = lines.next()?.to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    Some(Captured {
        method,
        path,
        headers,
        body: raw[split + 4..].to_vec(),
    })
}

fn write_response(stream: &mut TcpStream, spec: &ResponseSpec) {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        spec.status,
        if spec.status == 200 { "OK" } else { "Error" },
        spec.body.len()
    );
    for (name, value) in &spec.headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&spec.body);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn client_for(port: u16, max_retries: u32) -> OssClient {
    let config = ClientConfig::new(format!("http://127.0.0.1:{port}"), "ak", "sk")
        .with_max_retries(max_retries);
    OssClient::with_config(config).expect("创建客户端失败")
}

fn sample_error(code: &str, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Error><Code>{code}</Code><Message>{message}</Message>\
         <RequestId>unknown</RequestId><Resource>/b/k</Resource></Error>"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoint_path_prefix_applies_to_requests_and_presigned_urls() {
    let list_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <ListAllMyBucketsResult><Buckets><Bucket><Name>alpha</Name>\
        <CreationDate>2026-01-02T03:04:05.000Z</CreationDate></Bucket></Buckets>\
        </ListAllMyBucketsResult>";
    let server = TestServer::start(vec![
        ResponseSpec::ok(list_xml),
        ResponseSpec::ok(""),
    ]);
    let endpoint = format!("http://127.0.0.1:{}/api/v1", server.port);
    let client = OssClient::new(endpoint, "ak", "sk").expect("创建客户端失败");

    let buckets = client.list_buckets().await.expect("ListBuckets 失败");
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].name, "alpha");
    assert_eq!(buckets[0].created_at, "2026-01-02T03:04:05.000Z");

    client
        .create_bucket(Some("demo"))
        .await
        .expect("创建桶失败");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/v1");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].path, "/api/v1/demo");

    let url = client
        .presign_get(Some("demo"), "dir/a b.txt", Some(300))
        .expect("预签名失败");
    let prefix = format!("http://127.0.0.1:{}/api/v1/demo/dir/a%20b.txt?", server.port);
    assert!(url.starts_with(&prefix), "预签名 URL 必须包含 endpoint 前缀: {url}");
    assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"), "{url}");
    assert!(url.contains("X-Amz-Expires=300"), "{url}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_validation_and_content_md5_header() {
    let server = TestServer::start(vec![
        ResponseSpec::ok("").with_header("ETag", "\"etag-1\""),
    ]);
    let client = client_for(server.port, 0);

    let options = PutObjectOptions::new().with_metadata("bad key", "v");
    let error = client
        .put_object(Some("b"), "k", b"data".to_vec(), &options)
        .await
        .expect_err("含空格的元数据 key 应本地报错");
    assert!(matches!(error, Error::Config(_)), "错误类型: {error}");

    let options = PutObjectOptions::new().with_metadata("bad-value", "元数据");
    let error = client
        .put_object(Some("b"), "k", b"data".to_vec(), &options)
        .await
        .expect_err("非 ASCII 元数据 value 应本地报错");
    assert!(matches!(error, Error::Config(_)), "错误类型: {error}");

    // 统一收紧：Tab 属于控制字符，不再放行。
    let options = PutObjectOptions::new().with_metadata("tab-value", "a\tb");
    let error = client
        .put_object(Some("b"), "k", b"data".to_vec(), &options)
        .await
        .expect_err("含 Tab 的元数据 value 应本地报错");
    assert!(matches!(error, Error::Config(_)), "错误类型: {error}");

    // 非法输入不应发出任何请求；服务器仍在等待第一个真实请求。
    assert!(server.requests().is_empty());

    let md5 = md5_base64(b"data");
    let options = PutObjectOptions::new()
        .with_content_type("text/plain")
        .with_metadata("token_ok-1.~", "value")
        .with_content_md5(md5.clone())
        .with_auto_multipart(false);
    let output = client
        .put_object(Some("b"), "k", b"data".to_vec(), &options)
        .await
        .expect("PUT 失败");
    assert_eq!(output.etag, "etag-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].path, "/b/k");
    assert_eq!(
        requests[0].headers.get("content-md5").map(String::as_str),
        Some(md5.as_str())
    );
    assert_eq!(
        requests[0].headers.get("x-amz-meta-token_ok-1.~").map(String::as_str),
        Some("value")
    );
    assert_eq!(requests[0].body, b"data");
}

fn md5_base64(data: &[u8]) -> String {
    STANDARD.encode(Md5::digest(data))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_multipart_sends_content_md5_per_part_and_not_whole_object() {
    let init_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <InitiateMultipartUploadResult><UploadId>uid-1</UploadId></InitiateMultipartUploadResult>";
    let complete_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <CompleteMultipartUploadResult><ETag>\"final-3\"</ETag></CompleteMultipartUploadResult>";
    let server = TestServer::start(vec![
        ResponseSpec::ok(init_xml),
        ResponseSpec::ok("").with_header("ETag", "\"etag-1\""),
        ResponseSpec::ok("").with_header("ETag", "\"etag-2\""),
        ResponseSpec::ok("").with_header("ETag", "\"etag-3\""),
        ResponseSpec::ok(complete_xml),
    ]);
    let client = client_for(server.port, 0);

    let part_size = 1024usize;
    let data: Vec<u8> = (0..part_size * 2 + 7).map(|index| (index % 251) as u8).collect();
    let whole_md5 = md5_base64(&data);
    let output = client
        .put_object(
            Some("b"),
            "k",
            data.clone(),
            &PutObjectOptions::new()
                .with_part_size(part_size as u64)
                .with_content_md5(whole_md5.clone()),
        )
        .await
        .expect("自动分片失败");
    assert!(output.etag.ends_with("-3"), "应分 3 片: {}", output.etag);

    let requests = server.requests();
    assert_eq!(requests.len(), 5, "Create + 3 × UploadPart + Complete");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/b/k?uploads=");
    assert!(
        !requests[0].headers.contains_key("content-md5"),
        "CreateMultipartUpload 不应发送整对象 Content-MD5"
    );

    let chunks: Vec<&[u8]> = data.chunks(part_size).collect();
    for (index, request) in requests[1..4].iter().enumerate() {
        assert_eq!(request.method, "PUT");
        let part_number = index + 1;
        assert_eq!(
            request.path,
            format!("/b/k?partNumber={part_number}&uploadId=uid-1")
        );
        let expected = md5_base64(chunks[index]);
        assert_eq!(
            request.headers.get("content-md5").map(String::as_str),
            Some(expected.as_str()),
            "第 {part_number} 片必须发送该片自身 MD5（不是整对象 MD5）"
        );
        assert_eq!(request.body, chunks[index], "分片内容必须与本地 MD5 对应");
        assert_ne!(
            request.headers.get("content-md5").map(String::as_str),
            Some(whole_md5.as_str()),
            "分片请求不应发送整对象 Content-MD5"
        );
    }

    assert_eq!(requests[4].method, "POST");
    assert_eq!(requests[4].path, "/b/k?uploadId=uid-1");
    assert!(!requests[4].headers.contains_key("content-md5"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_multipart_rejects_wrong_whole_object_md5_locally() {
    // 不启动服务端：本地校验失败应发生在任何请求之前。
    let client = client_for(1, 0);
    let data = vec![b'x'; 2048];
    let error = client
        .put_object(
            Some("b"),
            "k",
            data,
            &PutObjectOptions::new()
                .with_part_size(1024)
                .with_content_md5(md5_base64(b"different content")),
        )
        .await
        .expect_err("整对象 MD5 不匹配应本地失败");
    let s3 = error.service().expect("应为 S3 结构化错误");
    assert_eq!(s3.status, 400);
    assert_eq!(s3.code, "BadDigest");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_replays_bytes_and_files_but_not_404_403() {
    // 字节 body：500 -> 200，应重试一次。
    let server = TestServer::start(vec![
        ResponseSpec::status(500, ""),
        ResponseSpec::ok("").with_header("ETag", "\"retried\""),
    ]);
    let client = client_for(server.port, 2);
    let output = client
        .put_object(
            Some("b"),
            "k",
            b"retry me".to_vec(),
            &PutObjectOptions::new().with_auto_multipart(false),
        )
        .await
        .expect("500 后应重试成功");
    assert_eq!(output.etag, "retried");
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "可重放的字节 body 应重试");
    assert!(requests.iter().all(|request| request.body == b"retry me"));

    // 文件 body：500 -> 200，文件路径属于可重放请求体。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retry.bin");
    std::fs::write(&path, b"file body").unwrap();
    let server = TestServer::start(vec![
        ResponseSpec::status(500, ""),
        ResponseSpec::ok("").with_header("ETag", "\"file-retried\""),
    ]);
    let client = client_for(server.port, 2);
    client
        .upload_file(
            Some("b"),
            "k",
            &path,
            &PutObjectOptions::new().with_auto_multipart(false),
        )
        .await
        .expect("文件 body 应可重试");
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "文件路径 body 应重试");
    assert!(requests.iter().all(|request| request.body == b"file body"));

    // 404 不重试。
    let server = TestServer::start(vec![ResponseSpec::status(
        404,
        sample_error("NoSuchKey", "missing"),
    )]);
    let client = client_for(server.port, 2);
    let error = client
        .get_object(Some("b"), "k", None)
        .await
        .expect_err("404 应失败");
    assert!(error.is_not_found());
    assert_eq!(error.service().unwrap().request_id.as_deref(), Some("unknown"));
    assert_eq!(server.requests().len(), 1, "404 不应重试");

    // 403 不重试。
    let server = TestServer::start(vec![ResponseSpec::status(
        403,
        sample_error("AccessDenied", "denied"),
    )]);
    let client = client_for(server.port, 2);
    let error = client
        .delete_object(Some("b"), "k")
        .await
        .expect_err("403 应失败");
    assert!(error.is_access_denied());
    assert_eq!(error.service().unwrap().status, 403);
    assert_eq!(server.requests().len(), 1, "403 不应重试");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presign_expires_defaults_to_900_and_accepts_explicit_value() {
    let client = client_for(1, 0);
    let url = client
        .presign_get(Some("demo"), "k", None)
        .expect("默认有效期预签名失败");
    assert!(url.contains("X-Amz-Expires=900"), "默认应为 900 秒: {url}");

    let url = client
        .presign_put(Some("demo"), "k", Some(60))
        .expect("显式有效期预签名失败");
    assert!(url.contains("X-Amz-Expires=60"), "{url}");

    assert!(
        client.presign_get(Some("demo"), "k", Some(0)).is_err(),
        "0 秒应报错"
    );
    assert!(
        client.presign_get(Some("demo"), "k", Some(604_801)).is_err(),
        "超过 7 天应报错"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_bucket_is_used_when_bucket_omitted() {
    let server = TestServer::start(vec![ResponseSpec::ok("")]);
    let config = ClientConfig::new(format!("http://127.0.0.1:{}", server.port), "ak", "sk")
        .with_max_retries(0)
        .with_default_bucket("defbucket");
    let client = OssClient::with_config(config).expect("创建客户端失败");

    client.create_bucket(None).await.expect("使用默认桶建桶失败");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/defbucket");

    let url = client
        .presign_get(None, "k", None)
        .expect("使用默认桶预签名失败");
    assert!(
        url.contains("/defbucket/k?"),
        "预签名 URL 应使用默认桶: {url}"
    );

    let client_without_default = client_for(1, 0);
    let error = client_without_default
        .get_object(None, "k", None)
        .await
        .expect_err("没有默认桶时应报错");
    match error {
        Error::Config(message) => assert!(
            message.contains("default_bucket"),
            "错误信息应说明缺少默认桶: {message}"
        ),
        other => panic!("应为 Config 错误: {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_objects_with_empty_keys_returns_empty_result() {
    // 不启动服务端：空 keys 必须在发出请求前直接返回空结果。
    let client = client_for(1, 0);
    let result = client
        .delete_objects(Some("b"), &[], false)
        .await
        .expect("空 keys 不应报错");
    assert!(result.deleted.is_empty());
    assert!(result.errors.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_object_with_empty_metadata_defaults_to_copy() {
    let copy_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <CopyObjectResult><ETag>\"copied\"</ETag></CopyObjectResult>";
    let server = TestServer::start(vec![ResponseSpec::ok(copy_xml)]);
    let client = client_for(server.port, 0);

    let metadata = BTreeMap::new();
    let output = client
        .copy_object(Some("b"), "src", Some("b"), "dst", &metadata, None)
        .await
        .expect("复制失败");
    assert_eq!(output.etag, "copied");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("x-amz-metadata-directive")
            .map(String::as_str),
        Some("COPY"),
        "空 metadata 必须按 COPY 处理"
    );
    assert!(
        !requests[0].headers.contains_key("x-amz-meta-anything"),
        "COPY 不应携带自定义元数据"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_view_binds_put_get_file_and_copy() {
    let copy_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <CopyObjectResult><ETag>\"copied\"</ETag></CopyObjectResult>";
    let server = TestServer::start(vec![
        ResponseSpec::ok("").with_header("ETag", "\"view-1\""),
        ResponseSpec::ok("hello"),
        ResponseSpec::ok("").with_header("ETag", "\"view-2\""),
        ResponseSpec::ok("file-bytes"),
        ResponseSpec::ok(copy_xml),
    ]);
    let client = client_for(server.port, 0);
    let view = client.bucket("bound");
    assert_eq!(view.bucket_name(), Some("bound"));

    let put = view
        .put_object("docs/a.txt", b"hello".to_vec(), &Default::default())
        .await
        .expect("视图 PUT 失败");
    assert_eq!(put.etag, "view-1");
    let data = view
        .get_object("docs/a.txt", None)
        .await
        .expect("视图 GET 失败");
    assert_eq!(data, b"hello".to_vec());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("up.bin");
    std::fs::write(&path, b"file-bytes").unwrap();
    let up = view
        .upload_file("docs/up.bin", &path, &Default::default())
        .await
        .expect("视图 upload_file 失败");
    assert_eq!(up.size, 10);
    let down = dir.path().join("down.bin");
    view.get_object_to_file("docs/up.bin", &down, None)
        .await
        .expect("视图下载失败");
    assert_eq!(std::fs::read(&down).unwrap(), b"file-bytes".to_vec());

    let copied = view
        .copy_object("docs/a.txt", "docs/copy.txt", &BTreeMap::new(), None)
        .await
        .expect("视图复制失败");
    assert_eq!(copied.etag, "copied");

    let requests = server.requests();
    assert_eq!(requests.len(), 5);
    for (index, expected) in [
        "/bound/docs/a.txt",
        "/bound/docs/a.txt",
        "/bound/docs/up.bin",
        "/bound/docs/up.bin",
        "/bound/docs/copy.txt",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(&requests[index].path, expected);
    }
    assert_eq!(
        requests[4]
            .headers
            .get("x-amz-copy-source")
            .map(String::as_str),
        Some("/bound/docs/a.txt"),
        "同桶复制应使用绑定桶作为源"
    );

    let url = view
        .presign_get("docs/a.txt", None)
        .expect("视图预签名失败");
    assert!(url.contains("/bound/docs/a.txt?"), "{url}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_view_matches_explicit_bucket_argument() {
    let server_view = TestServer::start(vec![
        ResponseSpec::ok("").with_header("ETag", "\"e1\"")
    ]);
    let server_explicit = TestServer::start(vec![
        ResponseSpec::ok("").with_header("ETag", "\"e1\"")
    ]);
    let client_view = client_for(server_view.port, 0);
    let client_explicit = client_for(server_explicit.port, 0);

    client_view
        .bucket("same")
        .put_object("k", b"body".to_vec(), &Default::default())
        .await
        .expect("视图上传失败");
    client_explicit
        .put_object(Some("same"), "k", b"body".to_vec(), &Default::default())
        .await
        .expect("显式上传失败");

    let view_requests = server_view.requests();
    let explicit_requests = server_explicit.requests();
    assert_eq!(view_requests.len(), 1);
    assert_eq!(explicit_requests.len(), 1);
    assert_eq!(view_requests[0].method, explicit_requests[0].method);
    assert_eq!(view_requests[0].path, explicit_requests[0].path);
    assert_eq!(view_requests[0].body, explicit_requests[0].body);
    for name in ["content-length", "x-amz-content-sha256"] {
        assert_eq!(
            view_requests[0].headers.get(name),
            explicit_requests[0].headers.get(name),
            "请求头 {name} 应一致"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_view_uses_configured_default_bucket() {
    let server = TestServer::start(vec![
        ResponseSpec::ok("").with_header("ETag", "\"def\"")
    ]);
    let config = ClientConfig::new(format!("http://127.0.0.1:{}", server.port), "ak", "sk")
        .with_max_retries(0)
        .with_default_bucket("defbucket");
    let client = OssClient::with_config(config).expect("创建客户端失败");

    let view = client.default_bucket();
    assert_eq!(view.bucket_name(), None);
    view.put_object("k", b"x".to_vec(), &Default::default())
        .await
        .expect("默认桶视图上传失败");
    assert_eq!(server.requests()[0].path, "/defbucket/k");

    let url = view.presign_get("k", None).expect("默认桶视图预签名失败");
    assert!(url.contains("/defbucket/k?"), "{url}");

    // 未配置默认桶：调用视图方法时才报错，并说明缺少 default_bucket。
    let error = client_for(1, 0)
        .default_bucket()
        .put_object("k", b"x".to_vec(), &Default::default())
        .await
        .expect_err("缺少默认桶应报错");
    match error {
        Error::Config(message) => assert!(
            message.contains("default_bucket"),
            "错误信息应说明缺少默认桶: {message}"
        ),
        other => panic!("应为 Config 错误: {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bucket_view_multipart_methods_forward_bound_bucket() {
    let init_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <InitiateMultipartUploadResult><UploadId>uid-view</UploadId></InitiateMultipartUploadResult>";
    let complete_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <CompleteMultipartUploadResult><ETag>\"final-view\"</ETag></CompleteMultipartUploadResult>";
    let server = TestServer::start(vec![
        ResponseSpec::ok(init_xml),
        ResponseSpec::ok("").with_header("ETag", "\"p1\""),
        ResponseSpec::ok(complete_xml),
    ]);
    let client = client_for(server.port, 0);
    let view = client.bucket("bound");

    let upload_id = view
        .create_multipart_upload("big.bin", None, &BTreeMap::new())
        .await
        .expect("视图 CreateMultipartUpload 失败");
    assert_eq!(upload_id, "uid-view");
    let etag = view
        .upload_part("big.bin", &upload_id, 1, b"part".to_vec())
        .await
        .expect("视图 UploadPart 失败");
    assert_eq!(etag, "p1");
    let parts = [CompletedPart {
        part_number: 1,
        etag: etag.clone(),
    }];
    let final_etag = view
        .complete_multipart_upload("big.bin", &upload_id, &parts)
        .await
        .expect("视图 CompleteMultipartUpload 失败");
    assert_eq!(final_etag, "final-view");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].path, "/bound/big.bin?uploads=");
    assert_eq!(
        requests[1].path,
        "/bound/big.bin?partNumber=1&uploadId=uid-view"
    );
    assert_eq!(requests[2].path, "/bound/big.bin?uploadId=uid-view");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_put_also_rejects_wrong_whole_object_md5_locally() {
    // 字节：单次 PUT 也先本地校验，不匹配不发请求。
    let client = client_for(1, 0);
    let error = client
        .put_object(
            Some("b"),
            "k",
            b"payload".to_vec(),
            &PutObjectOptions::new()
                .with_auto_multipart(false)
                .with_content_md5(md5_base64(b"different")),
        )
        .await
        .expect_err("单次 PUT 整对象 MD5 不匹配应本地失败");
    assert_eq!(error.service().expect("应为 S3 错误").code, "BadDigest");

    // 文件：单次 PUT 同样先流式本地校验。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("single.bin");
    std::fs::write(&path, b"file payload").unwrap();
    let error = client
        .upload_file(
            Some("b"),
            "k",
            &path,
            &PutObjectOptions::new()
                .with_auto_multipart(false)
                .with_content_md5(md5_base64(b"different file")),
        )
        .await
        .expect_err("单次 PUT 文件 MD5 不匹配应本地失败");
    let s3 = error.service().expect("应为 S3 错误");
    assert_eq!(s3.status, 400);
    assert_eq!(s3.code, "BadDigest");
}
