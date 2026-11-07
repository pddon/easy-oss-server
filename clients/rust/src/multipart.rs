//! 分片上传（Multipart Upload）低层 API。

use std::collections::BTreeMap;

use reqwest::Method;

use crate::client::{header_string, map_transport_error, RequestBody, RequestSpec};
use crate::error::Error;
use crate::object::{validate_header_value, validate_metadata};
use crate::types::{CompletedPart, ListPartsResult, PartInfo};
use crate::xml::{self, escape};
use crate::OssClient;

impl OssClient {
    /// 初始化分片上传，返回 `upload_id`。
    pub async fn create_multipart_upload(
        &self,
        bucket: Option<&str>,
        key: &str,
        content_type: Option<&str>,
        metadata: &BTreeMap<String, String>,
    ) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::POST, Some(bucket), Some(key.to_string()));
        spec.query.push(("uploads".to_string(), String::new()));
        push_upload_headers(&mut spec.headers, content_type, metadata)?;

        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;
        root.child_text("UploadId")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::InvalidResponse("响应缺少 UploadId".to_string()))
    }

    /// 上传一个分片（字节），返回去掉引号的 ETag。
    pub async fn upload_part(
        &self,
        bucket: Option<&str>,
        key: &str,
        upload_id: &str,
        part_number: u32,
        body: impl Into<Vec<u8>>,
    ) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        self.send_part(&bucket, key, upload_id, part_number, body.into(), None)
            .await
    }

    /// 内部方法：自动分片上传时逐片计算 MD5 并发送 `Content-MD5`。
    pub(crate) async fn upload_part_with_md5(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: u32,
        body: Vec<u8>,
        content_md5: String,
    ) -> Result<String, Error> {
        self.send_part(
            bucket,
            key,
            upload_id,
            part_number,
            body,
            Some(content_md5),
        )
        .await
    }

    async fn send_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: u32,
        body: Vec<u8>,
        content_md5: Option<String>,
    ) -> Result<String, Error> {
        let mut spec = self.part_request(bucket, key, upload_id, part_number)?;
        if let Some(content_md5) = content_md5 {
            validate_header_value(&content_md5, "Content-MD5")?;
            spec.headers
                .push(("content-md5".to_string(), content_md5));
        }
        spec.body = RequestBody::Bytes(body);

        let response = self.send_checked(&spec).await?;
        Ok(header_string(response.headers(), "etag")
            .map(|etag| etag.trim().trim_matches('"').to_string())
            .unwrap_or_default())
    }

    fn part_request(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: u32,
    ) -> Result<RequestSpec, Error> {
        if !(1..=10_000).contains(&part_number) {
            return Err(Error::Config(
                "part_number 必须在 1..=10000 之间".to_string(),
            ));
        }
        let mut spec = RequestSpec::new(
            Method::PUT,
            Some(bucket.to_string()),
            Some(key.to_string()),
        );
        spec.query
            .push(("partNumber".to_string(), part_number.to_string()));
        spec.query
            .push(("uploadId".to_string(), upload_id.to_string()));
        Ok(spec)
    }

    /// 完成分片上传。传入分片会按 part_number 升序排序后提交，返回最终 ETag。
    pub async fn complete_multipart_upload(
        &self,
        bucket: Option<&str>,
        key: &str,
        upload_id: &str,
        parts: &[CompletedPart],
    ) -> Result<String, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        if parts.is_empty() {
            return Err(Error::Config("至少需要一个分片".to_string()));
        }

        let mut ordered = parts.to_vec();
        ordered.sort_by_key(|part| part.part_number);

        let mut body = String::from("<CompleteMultipartUpload>");
        for part in &ordered {
            body.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part.part_number,
                escape(&part.etag)
            ));
        }
        body.push_str("</CompleteMultipartUpload>");

        let mut spec = RequestSpec::new(Method::POST, Some(bucket), Some(key.to_string()));
        spec.query
            .push(("uploadId".to_string(), upload_id.to_string()));
        spec.headers
            .push(("content-type".to_string(), "application/xml".to_string()));
        spec.body = RequestBody::Bytes(body.into_bytes());

        let response = self.send_checked(&spec).await?;
        let raw = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&raw).map_err(Error::InvalidResponse)?;
        root.child_text("ETag")
            .map(|etag| etag.trim().trim_matches('"').to_string())
            .filter(|etag| !etag.is_empty())
            .ok_or_else(|| Error::InvalidResponse("响应缺少 ETag".to_string()))
    }

    /// 取消分片上传并清理服务端临时分片。
    pub async fn abort_multipart_upload(
        &self,
        bucket: Option<&str>,
        key: &str,
        upload_id: &str,
    ) -> Result<(), Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::DELETE, Some(bucket), Some(key.to_string()));
        spec.query
            .push(("uploadId".to_string(), upload_id.to_string()));
        self.send_checked(&spec).await?;
        Ok(())
    }

    /// 列出某次分片上传已上传的分片（服务端一次性返回全部，按 part_number 升序）。
    pub async fn list_parts(
        &self,
        bucket: Option<&str>,
        key: &str,
        upload_id: &str,
    ) -> Result<ListPartsResult, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::GET, Some(bucket), Some(key.to_string()));
        spec.query
            .push(("uploadId".to_string(), upload_id.to_string()));

        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;

        let mut parts: Vec<PartInfo> = Vec::new();
        let is_truncated = root
            .child_text("IsTruncated")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        for part in root.children_named("Part") {
            let part_number = part
                .child_text("PartNumber")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            parts.push(PartInfo {
                part_number,
                etag: part
                    .child_text("ETag")
                    .map(|etag| etag.trim().trim_matches('"').to_string())
                    .unwrap_or_default(),
                size: part
                    .child_text("Size")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                last_modified: part.child_text("LastModified"),
            });
        }
        parts.sort_by_key(|part| part.part_number);
        Ok(ListPartsResult { parts, is_truncated })
    }
}

pub(crate) fn push_upload_headers(
    headers: &mut Vec<(String, String)>,
    content_type: Option<&str>,
    metadata: &BTreeMap<String, String>,
) -> Result<(), Error> {
    if let Some(content_type) = content_type {
        validate_header_value(content_type, "Content-Type")?;
        headers.push(("content-type".to_string(), content_type.to_string()));
    }
    validate_metadata(metadata)?;
    for (key, value) in metadata {
        headers.push((format!("x-amz-meta-{key}"), value.to_string()));
    }
    Ok(())
}
