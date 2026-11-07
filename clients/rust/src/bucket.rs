//! 桶级 API。

use reqwest::Method;

use crate::client::{header_string, map_transport_error, RequestSpec};
use crate::error::Error;
use crate::types::{BucketHead, BucketInfo};
use crate::xml;
use crate::OssClient;

impl OssClient {
    /// 创建桶。桶已存在时返回 `409`（`BucketAlreadyOwnedByYou` / `BucketAlreadyExists`）。
    pub async fn create_bucket(&self, bucket: Option<&str>) -> Result<(), Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let spec = RequestSpec::new(Method::PUT, Some(bucket), None);
        self.send_checked(&spec).await?;
        Ok(())
    }

    /// 删除桶。桶内仍有 active 对象时返回 `409 BucketNotEmpty`。
    pub async fn delete_bucket(&self, bucket: Option<&str>) -> Result<(), Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let spec = RequestSpec::new(Method::DELETE, Some(bucket), None);
        self.send_checked(&spec).await?;
        Ok(())
    }

    /// 列出当前 App 的全部桶，每项包含 name 与 created_at。
    pub async fn list_buckets(&self) -> Result<Vec<BucketInfo>, Error> {
        let spec = RequestSpec::new(Method::GET, None, None);
        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;

        let mut buckets = Vec::new();
        if let Some(container) = root.child("Buckets") {
            for bucket in container.children_named("Bucket") {
                let Some(name) = bucket.child_text("Name") else {
                    continue;
                };
                buckets.push(BucketInfo {
                    name,
                    created_at: bucket.child_text("CreationDate").unwrap_or_default(),
                });
            }
        }
        Ok(buckets)
    }

    /// HEAD 桶：返回是否存在以及 region。
    ///
    /// region 优先取响应头 `x-amz-bucket-region`，缺失时回退到配置的 region；
    /// 桶不存在（404）时返回 `exists = false`，不抛错。
    pub async fn head_bucket(&self, bucket: Option<&str>) -> Result<BucketHead, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let spec = RequestSpec::new(Method::HEAD, Some(bucket), None);
        match self.send_checked(&spec).await {
            Ok(response) => Ok(BucketHead {
                exists: true,
                region: header_string(response.headers(), "x-amz-bucket-region")
                    .filter(|region| !region.is_empty())
                    .unwrap_or_else(|| self.config().region.clone()),
            }),
            Err(err) if err.is_not_found() => Ok(BucketHead {
                exists: false,
                region: self.config().region.clone(),
            }),
            Err(err) => Err(err),
        }
    }

    /// 桶是否存在。等价于 `head_bucket(bucket)?.exists`。
    pub async fn bucket_exists(&self, bucket: Option<&str>) -> Result<bool, Error> {
        Ok(self.head_bucket(bucket).await?.exists)
    }

    /// 获取桶 location；服务端返回空值时统一归一化为 `us-east-1`。
    pub async fn get_bucket_location(&self, bucket: Option<&str>) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::GET, Some(bucket), None);
        spec.query.push(("location".to_string(), String::new()));

        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;
        let location = root
            .child("LocationConstraint")
            .map(|node| node.text_value())
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(if location.is_empty() {
            "us-east-1".to_string()
        } else {
            location
        })
    }
}
