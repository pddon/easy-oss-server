//! 客户端核心：URL 构造、SigV4 签名、请求发送与重试。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use reqwest::{Method, Response};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use url::Url;

use crate::config::ClientConfig;
use crate::error::Error;
use crate::signing::{self, ALGORITHM, EMPTY_PAYLOAD_SHA256, SERVICE, UNSIGNED_PAYLOAD};
use crate::types::ObjectHead;

const MAX_PRESIGN_EXPIRES_SECS: u64 = 7 * 24 * 3600;
/// 未显式指定预签名有效期时的默认值（秒）。
const DEFAULT_PRESIGN_EXPIRES_SECS: u64 = 900;
const SIGNED_HEADERS: [&str; 3] = ["host", "x-amz-content-sha256", "x-amz-date"];

/// OSS 客户端。内部使用 `Arc`，可跨任务克隆共享。
#[derive(Clone)]
pub struct OssClient {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub http: reqwest::Client,
    pub config: ClientConfig,
    origin: String,
    host: String,
    base_path: String,
}

/// 请求 body。文件 body 每次尝试都会重新打开，因此可安全重试。
pub(crate) enum RequestBody {
    Empty,
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// 一次 S3 请求的描述。
pub(crate) struct RequestSpec {
    pub method: Method,
    pub bucket: Option<String>,
    pub key: Option<String>,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: RequestBody,
}

impl RequestSpec {
    pub(crate) fn new(method: Method, bucket: Option<String>, key: Option<String>) -> Self {
        Self {
            method,
            bucket,
            key,
            query: Vec::new(),
            headers: Vec::new(),
            body: RequestBody::Empty,
        }
    }
}

impl OssClient {
    /// 用 endpoint / accessKey / secretKey 创建客户端（其余参数取默认值）。
    pub fn new(
        endpoint: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self, Error> {
        Self::with_config(ClientConfig::new(endpoint, access_key, secret_key))
    }

    /// 用完整配置创建客户端。
    pub fn with_config(config: ClientConfig) -> Result<Self, Error> {
        config.validate()?;

        let raw = config.endpoint.trim().trim_end_matches('/');
        let base = Url::parse(raw)
            .map_err(|err| Error::Config(format!("endpoint 不是合法 URL: {err}")))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(Error::Config(format!(
                "endpoint 只支持 http/https，当前为 {}",
                base.scheme()
            )));
        }
        if base.host_str().is_none() {
            return Err(Error::Config("endpoint 缺少 host".into()));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(Error::Config(
                "endpoint 不能包含 query 或 fragment".into(),
            ));
        }

        let origin = origin_of(&base);
        let host = host_header_of(&base);
        // endpoint 允许带路径前缀（如 http://host:8080/api），签名与请求都使用该前缀。
        let base_path = base.path().trim_end_matches('/').to_string();

        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(concat!("oss-client/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                origin,
                host,
                base_path,
            }),
        })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// 派生一个绑定桶名的轻量视图，后续对象 / 分片调用无需再传 bucket。
    ///
    /// 视图只借用当前客户端，不复制 HTTP 连接、连接池或配置。
    pub fn bucket<'a>(&'a self, bucket: &'a str) -> crate::bucket_view::BucketView<'a> {
        crate::bucket_view::BucketView::new(self, Some(bucket))
    }

    /// 派生一个使用配置 `default_bucket` 的视图。
    ///
    /// 未配置默认桶时不会立即报错，而是在调用视图方法时返回 `Error::Config`。
    pub fn default_bucket(&self) -> crate::bucket_view::BucketView<'_> {
        crate::bucket_view::BucketView::new(self, None)
    }

    /// 解析 bucket：传入具体值则使用它；传 `None` 时使用配置里的 `default_bucket`。
    ///
    /// 两者都没有时返回 `Error::Config` 并说明原因。
    pub(crate) fn resolve_bucket(&self, bucket: Option<&str>) -> Result<String, Error> {
        match bucket {
            Some(name) => Ok(name.to_string()),
            None => self.inner.config.default_bucket.clone().ok_or_else(|| {
                Error::Config(
                    "未提供 bucket，且客户端配置中没有 default_bucket；请显式传入 bucket，\
                     或用 ClientConfig::with_default_bucket 设置默认桶"
                        .to_string(),
                )
            }),
        }
    }

    /// 生成预签名 GET URL。`expires_in` 为秒，取值 1..=604800；传 `None` 时默认 900 秒。
    pub fn presign_get(
        &self,
        bucket: Option<&str>,
        key: &str,
        expires_in: Option<u64>,
    ) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        self.presign_url(Method::GET, &bucket, key, expires_in)
    }

    /// 生成预签名 PUT URL，可用于第三方直接上传临时文件。
    /// `expires_in` 为秒，取值 1..=604800；传 `None` 时默认 900 秒。
    pub fn presign_put(
        &self,
        bucket: Option<&str>,
        key: &str,
        expires_in: Option<u64>,
    ) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        self.presign_url(Method::PUT, &bucket, key, expires_in)
    }

    fn presign_url(
        &self,
        method: Method,
        bucket: &str,
        key: &str,
        expires_in: Option<u64>,
    ) -> Result<String, Error> {
        let expires_in = expires_in.unwrap_or(DEFAULT_PRESIGN_EXPIRES_SECS);
        if expires_in == 0 || expires_in > MAX_PRESIGN_EXPIRES_SECS {
            return Err(Error::Config(format!(
                "预签名有效期必须在 1..={MAX_PRESIGN_EXPIRES_SECS} 秒之间"
            )));
        }

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let config = &self.inner.config;
        let scope = format!(
            "{}/{}/{}/{}",
            date, config.region, SERVICE, signing::TERMINATOR
        );
        let credential = format!("{}/{scope}", config.access_key);

        let params = vec![
            ("X-Amz-Algorithm".to_string(), ALGORITHM.to_string()),
            ("X-Amz-Credential".to_string(), credential),
            ("X-Amz-Date".to_string(), amz_date.clone()),
            ("X-Amz-Expires".to_string(), expires_in.to_string()),
            ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
        ];
        let canonical_query = signing::canonical_query_string(&params);

        let path = self.inner.path_for(Some(bucket), Some(key));
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), self.inner.host.clone());

        let signed_headers = ["host".to_string()];
        let canonical_request = signing::canonical_request(
            method.as_str(),
            &path,
            &canonical_query,
            &headers,
            &signed_headers,
            UNSIGNED_PAYLOAD,
        );
        let string_to_sign = signing::string_to_sign(
            &amz_date,
            &date,
            &config.region,
            SERVICE,
            &canonical_request,
        );
        let signature = signing::compute_signature(
            &config.secret_key,
            &date,
            &config.region,
            SERVICE,
            &string_to_sign,
        );

        Ok(format!(
            "{}{}?{}&X-Amz-Signature={}",
            self.inner.origin, path, canonical_query, signature
        ))
    }

    /// 发送请求并在非 2xx 时映射为 [`Error::Service`]。
    pub(crate) async fn send_checked(&self, spec: &RequestSpec) -> Result<Response, Error> {
        let response = self.execute(spec).await?;
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap_or_default();
        Err(Error::Service(crate::xml::parse_s3_error(status, &body)))
    }

    /// 带重试的请求发送。
    ///
    /// 只对可重放请求体重试：字节、文件路径在每次尝试时都会重新构造；
    /// 429 / 5xx / 网络错误按指数退避重试，404 / 403 不重试。
    pub(crate) async fn execute(&self, spec: &RequestSpec) -> Result<Response, Error> {
        let max_attempts = self.inner.config.max_retries.saturating_add(1);
        let mut attempt = 0u32;

        loop {
            match self.send_once(spec).await {
                Ok(response) => {
                    if attempt + 1 < max_attempts && retryable_status(response.status().as_u16()) {
                        attempt += 1;
                        // 丢弃响应体，连接可复用。
                        let _ = response.bytes().await;
                        self.backoff(attempt).await;
                        continue;
                    }
                    return Ok(response);
                }
                Err(err) => {
                    if err.is_retryable() && attempt + 1 < max_attempts {
                        attempt += 1;
                        self.backoff(attempt).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn backoff(&self, attempt: u32) {
        let millis = 100u64.saturating_mul(1u64 << attempt.min(4));
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }

    async fn send_once(&self, spec: &RequestSpec) -> Result<Response, Error> {
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();

        let (payload_hash, file) = match &spec.body {
            RequestBody::Empty => (EMPTY_PAYLOAD_SHA256.to_string(), None),
            RequestBody::Bytes(data) => (signing::sha256_hex(data), None),
            RequestBody::File(path) => {
                let file = tokio::fs::File::open(path).await?;
                (UNSIGNED_PAYLOAD.to_string(), Some(file))
            }
        };

        let path = self.inner.path_for(spec.bucket.as_deref(), spec.key.as_deref());
        let canonical_query = signing::canonical_query_string(&spec.query);

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), self.inner.host.clone());
        headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
        headers.insert("x-amz-date".to_string(), amz_date.clone());

        let config = &self.inner.config;
        let signed_headers: Vec<String> = SIGNED_HEADERS.iter().map(|h| h.to_string()).collect();
        let canonical_request = signing::canonical_request(
            spec.method.as_str(),
            &path,
            &canonical_query,
            &headers,
            &signed_headers,
            &payload_hash,
        );
        let string_to_sign = signing::string_to_sign(
            &amz_date,
            &date,
            &config.region,
            SERVICE,
            &canonical_request,
        );
        let signature = signing::compute_signature(
            &config.secret_key,
            &date,
            &config.region,
            SERVICE,
            &string_to_sign,
        );
        let signed_headers_joined = SIGNED_HEADERS.join(";");
        let authorization = format!(
            "{ALGORITHM} Credential={}/{}/{}/{}/{}, SignedHeaders={}, Signature={}",
            config.access_key,
            date,
            config.region,
            SERVICE,
            signing::TERMINATOR,
            signed_headers_joined,
            signature
        );

        let url = if canonical_query.is_empty() {
            format!("{}{}", self.inner.origin, path)
        } else {
            format!("{}{}?{}", self.inner.origin, path, canonical_query)
        };

        let mut builder = self.inner.http.request(spec.method.clone(), url);
        for (name, value) in &spec.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder = builder
            .header("host", &self.inner.host)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("authorization", authorization);

        builder = match (&spec.body, file) {
            (RequestBody::Empty, _) => builder,
            (RequestBody::Bytes(data), _) => builder.body(data.clone()),
            (RequestBody::File(_), Some(file)) => {
                let size = file.metadata().await?.len();
                builder
                    .header("content-length", size.to_string())
                    .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            }
            (RequestBody::File(_), None) => unreachable!("文件 body 已在上面打开"),
        };

        builder.send().await.map_err(map_transport_error)
    }
}

impl Inner {
    pub(crate) fn path_for(&self, bucket: Option<&str>, key: Option<&str>) -> String {
        let mut path = self.base_path.clone();
        if let Some(bucket) = bucket {
            path.push('/');
            path.push_str(&signing::uri_encode(bucket, true));
        }
        if let Some(key) = key {
            path.push('/');
            path.push_str(&signing::uri_encode(key, false));
        }
        if path.is_empty() {
            path.push('/');
        }
        path
    }
}

pub(crate) fn map_transport_error(err: reqwest::Error) -> Error {
    if err.is_timeout() {
        Error::Timeout
    } else {
        Error::Http(err)
    }
}

fn retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

pub(crate) fn origin_of(url: &Url) -> String {
    let host = match url.host() {
        Some(url::Host::Ipv6(addr)) => format!("[{addr}]"),
        Some(host) => host.to_string(),
        None => String::new(),
    };
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

fn host_header_of(url: &Url) -> String {
    let host = match url.host() {
        Some(url::Host::Ipv6(addr)) => format!("[{addr}]"),
        Some(host) => host.to_string(),
        None => String::new(),
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// 从响应头解析对象元信息。
pub(crate) fn object_head_from_response(response: &Response) -> ObjectHead {
    let headers = response.headers();
    let mut metadata = BTreeMap::new();
    for (name, value) in headers.iter() {
        if let Some(key) = name.as_str().strip_prefix("x-amz-meta-") {
            if let Ok(value) = value.to_str() {
                metadata.insert(key.to_string(), value.to_string());
            }
        }
    }

    ObjectHead {
        size: headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        etag: header_string(headers, "etag")
            .map(|etag| etag.trim().trim_matches('"').to_string())
            .unwrap_or_default(),
        content_type: header_string(headers, "content-type").unwrap_or_default(),
        last_modified: header_string(headers, "last-modified"),
        metadata,
    }
}

pub(crate) fn header_string(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

/// 把响应体写入本地文件（流式，不整体读入内存）。
pub(crate) async fn response_to_file(
    response: Response,
    path: impl AsRef<std::path::Path>,
) -> Result<(), Error> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let result = write_stream(response, &path).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    result
}

async fn write_stream(response: Response, path: &std::path::Path) -> Result<(), Error> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_transport_error)?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}
