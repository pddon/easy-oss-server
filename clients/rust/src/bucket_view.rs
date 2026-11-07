//! 绑定桶名的轻量视图。
//!
//! [`BucketView`] 只借用 [`OssClient`]，不复制 HTTP 连接、连接池或配置，
//! 也不会与客户端产生状态分叉。所有方法都直接转发到 `OssClient` 的同名方法。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::client::OssClient;
use crate::types::{
    CompletedPart, CopyObjectResult, DeleteObjectsResult, ListObjectsV2Response, ListPartsResult,
    MetadataDirective, ObjectHead, PutObjectOptions, PutObjectResult, Range,
};
use crate::Error;

/// 绑定了一个桶（或配置里的默认桶）的客户端视图。
///
/// 由 [`OssClient::bucket`] 或 [`OssClient::default_bucket`] 创建，生命周期与客户端一致，
/// 可直接链式调用对象 / 分片方法而无需再传 `bucket`：
///
/// ```no_run
/// # async fn demo(client: oss_s3_client::OssClient) -> Result<(), oss_s3_client::Error> {
/// client
///     .bucket("my-bucket")
///     .put_object("docs/a.txt", b"hello".to_vec(), &Default::default())
///     .await?;
/// # Ok(())
/// # }
/// ```
///
/// 桶级方法（建桶、删桶、列桶、HEAD 桶、查 location）不在此视图上提供：
/// 这个视图表达的是“在某个桶内部操作对象”，而不是桶资源本身；
/// 建桶 / 删桶属于账号级生命周期操作，且视图创建时不保证桶已存在。
pub struct BucketView<'a> {
    client: &'a OssClient,
    bucket: Option<&'a str>,
}

impl<'a> BucketView<'a> {
    pub(crate) fn new(client: &'a OssClient, bucket: Option<&'a str>) -> Self {
        Self { client, bucket }
    }

    /// 视图绑定的桶名；使用配置默认桶时为 `None`。
    pub fn bucket_name(&self) -> Option<&'a str> {
        self.bucket
    }

    /// 上传字节内容。等价于 `OssClient::put_object(bucket, key, body, options)`。
    pub async fn put_object(
        &self,
        key: &str,
        body: impl Into<Vec<u8>>,
        options: &PutObjectOptions,
    ) -> Result<PutObjectResult, Error> {
        self.client.put_object(self.bucket, key, body, options).await
    }

    /// 上传文件。等价于 `OssClient::upload_file(bucket, key, path, options)`。
    pub async fn upload_file(
        &self,
        key: &str,
        path: impl Into<PathBuf>,
        options: &PutObjectOptions,
    ) -> Result<PutObjectResult, Error> {
        self.client.upload_file(self.bucket, key, path, options).await
    }

    /// 下载对象内容。等价于 `OssClient::get_object(bucket, key, range)`。
    pub async fn get_object(&self, key: &str, range: Option<Range>) -> Result<Vec<u8>, Error> {
        self.client.get_object(self.bucket, key, range).await
    }

    /// 下载对象到文件。等价于 `OssClient::get_object_to_file(bucket, key, path, range)`。
    pub async fn get_object_to_file(
        &self,
        key: &str,
        path: impl Into<PathBuf>,
        range: Option<Range>,
    ) -> Result<(), Error> {
        self.client
            .get_object_to_file(self.bucket, key, path, range)
            .await
    }

    /// HEAD 对象。等价于 `OssClient::head_object(bucket, key)`。
    pub async fn head_object(&self, key: &str) -> Result<ObjectHead, Error> {
        self.client.head_object(self.bucket, key).await
    }

    /// 对象是否存在。等价于 `OssClient::object_exists(bucket, key)`。
    pub async fn object_exists(&self, key: &str) -> Result<bool, Error> {
        self.client.object_exists(self.bucket, key).await
    }

    /// 删除对象。等价于 `OssClient::delete_object(bucket, key)`。
    pub async fn delete_object(&self, key: &str) -> Result<(), Error> {
        self.client.delete_object(self.bucket, key).await
    }

    /// 同桶内复制对象。跨桶复制请直接调用 `OssClient::copy_object`。
    pub async fn copy_object(
        &self,
        source_key: &str,
        dest_key: &str,
        metadata: &BTreeMap<String, String>,
        metadata_directive: Option<MetadataDirective>,
    ) -> Result<CopyObjectResult, Error> {
        self.client
            .copy_object(
                self.bucket,
                source_key,
                self.bucket,
                dest_key,
                metadata,
                metadata_directive,
            )
            .await
    }

    /// 批量删除。等价于 `OssClient::delete_objects(bucket, keys, quiet)`。
    pub async fn delete_objects(
        &self,
        keys: &[&str],
        quiet: bool,
    ) -> Result<DeleteObjectsResult, Error> {
        self.client.delete_objects(self.bucket, keys, quiet).await
    }

    /// 分页列举对象。等价于 `OssClient::list_objects_v2(bucket, ...)`。
    #[allow(clippy::too_many_arguments)]
    pub async fn list_objects_v2(
        &self,
        prefix: Option<&str>,
        delimiter: Option<&str>,
        max_keys: Option<u32>,
        continuation_token: Option<&str>,
        start_after: Option<&str>,
    ) -> Result<ListObjectsV2Response, Error> {
        self.client
            .list_objects_v2(
                self.bucket,
                prefix,
                delimiter,
                max_keys,
                continuation_token,
                start_after,
            )
            .await
    }

    /// 预签名 GET URL。等价于 `OssClient::presign_get(bucket, key, expires_in)`。
    pub fn presign_get(&self, key: &str, expires_in: Option<u64>) -> Result<String, Error> {
        self.client.presign_get(self.bucket, key, expires_in)
    }

    /// 预签名 PUT URL。等价于 `OssClient::presign_put(bucket, key, expires_in)`。
    pub fn presign_put(&self, key: &str, expires_in: Option<u64>) -> Result<String, Error> {
        self.client.presign_put(self.bucket, key, expires_in)
    }

    /// 创建分片上传。等价于 `OssClient::create_multipart_upload(bucket, key, ...)`。
    pub async fn create_multipart_upload(
        &self,
        key: &str,
        content_type: Option<&str>,
        metadata: &BTreeMap<String, String>,
    ) -> Result<String, Error> {
        self.client
            .create_multipart_upload(self.bucket, key, content_type, metadata)
            .await
    }

    /// 上传分片。等价于 `OssClient::upload_part(bucket, key, upload_id, part_number, body)`。
    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: u32,
        body: impl Into<Vec<u8>>,
    ) -> Result<String, Error> {
        self.client
            .upload_part(self.bucket, key, upload_id, part_number, body)
            .await
    }

    /// 完成分片上传。等价于 `OssClient::complete_multipart_upload(bucket, key, ...)`。
    pub async fn complete_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[CompletedPart],
    ) -> Result<String, Error> {
        self.client
            .complete_multipart_upload(self.bucket, key, upload_id, parts)
            .await
    }

    /// 取消分片上传。等价于 `OssClient::abort_multipart_upload(bucket, key, upload_id)`。
    pub async fn abort_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<(), Error> {
        self.client
            .abort_multipart_upload(self.bucket, key, upload_id)
            .await
    }

    /// 列举分片。等价于 `OssClient::list_parts(bucket, key, upload_id)`。
    pub async fn list_parts(&self, key: &str, upload_id: &str) -> Result<ListPartsResult, Error> {
        self.client.list_parts(self.bucket, key, upload_id).await
    }
}
