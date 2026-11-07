//! 客户端配置。

use std::time::Duration;

/// 客户端配置。
///
/// 所有字段都可以直接用结构体字面量构造，也可用链式 `with_*` 方法。
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// 服务端地址，例如 `http://127.0.0.1:8080`。可携带路径前缀。
    pub endpoint: String,
    /// AccessKey，对应服务端的 **App ID**。
    pub access_key: String,
    /// SecretKey，对应服务端的 **App Secret**。
    pub secret_key: String,
    /// 签名 region，服务端不校验，任意非空值即可（默认 `us-east-1`）。
    pub region: String,
    /// 默认桶；`OssClient::default_bucket()` 视图会使用它，调用方也可显式传 `None`。
    pub default_bucket: Option<String>,
    /// 单次请求超时（连接 + 读取响应）。
    pub timeout: Duration,
    /// 失败重试次数（0 表示不重试）。仅对可重放请求体（字节 / 文件路径）生效。
    pub max_retries: u32,
}

impl ClientConfig {
    pub fn new(
        endpoint: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: "us-east-1".to_string(),
            default_bucket: None,
            timeout: Duration::from_secs(30),
            max_retries: 3,
        }
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn with_default_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.default_bucket = Some(bucket.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), crate::Error> {
        if self.endpoint.trim().is_empty() {
            return Err(crate::Error::Config("endpoint 不能为空".into()));
        }
        if self.access_key.trim().is_empty() {
            return Err(crate::Error::Config("access_key 不能为空".into()));
        }
        if self.secret_key.trim().is_empty() {
            return Err(crate::Error::Config("secret_key 不能为空".into()));
        }
        if self.region.trim().is_empty() {
            return Err(crate::Error::Config("region 不能为空".into()));
        }
        Ok(())
    }
}
