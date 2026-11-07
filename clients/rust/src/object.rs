//! 对象级 API：上传、下载、HEAD、复制、删除、批量删除、ListObjectsV2。

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use reqwest::Method;
use tokio::io::AsyncReadExt;

use crate::client::{
    header_string, map_transport_error, object_head_from_response, response_to_file, RequestBody,
    RequestSpec,
};
use crate::error::Error;
use crate::signing;
use crate::types::{
    CopyObjectResult, DeleteObjectError, DeleteObjectsResult, ListObjectsV2Response,
    MetadataDirective, ObjectHead, ObjectInfo, PutObjectOptions, PutObjectResult, Range,
};
use crate::xml::{self, escape};
use crate::OssClient;

const MAX_DELETE_BATCH: usize = 1000;
const MAX_PARTS: u32 = 10_000;

impl OssClient {
    /// 上传字节内容。开启 `auto_multipart`（默认）且内容超过 `part_size` 时自动走分片上传。
    pub async fn put_object(
        &self,
        bucket: Option<&str>,
        key: &str,
        body: impl Into<Vec<u8>>,
        options: &PutObjectOptions,
    ) -> Result<PutObjectResult, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let data = body.into();
        let size = data.len() as u64;
        validate_options(options)?;
        let part_size = options.effective_part_size();
        if part_size == 0 {
            return Err(Error::Config("part_size 必须大于 0".to_string()));
        }

        // 数据可重放：只要传入了具体 MD5，单次 PUT 与自动分片都先做本地整对象校验。
        if let Some(expected) = options.content_md5.as_deref() {
            verify_bytes_md5(expected, &data)?;
        }

        if options.auto_multipart && data.len() as u64 > part_size {
            // 分片路径不再发送整对象 Content-MD5，改为逐片发送分片 MD5。
            let upload_id = self
                .create_multipart_upload(
                    Some(bucket.as_str()),
                    key,
                    options.content_type.as_deref(),
                    &options.metadata,
                )
                .await?;
            let result = async {
                let mut parts = Vec::new();
                for (index, chunk) in data.chunks(part_size as usize).enumerate() {
                    let part_number = index as u32 + 1;
                    if part_number > MAX_PARTS {
                        return Err(Error::Config(
                            "分片数超过服务端上限 10000，请增大 part_size".to_string(),
                        ));
                    }
                    let etag = self
                        .upload_part_with_md5(
                            &bucket,
                            key,
                            &upload_id,
                            part_number,
                            chunk.to_vec(),
                            md5_base64(chunk),
                        )
                        .await?;
                    parts.push(crate::types::CompletedPart { part_number, etag });
                }
                self.complete_multipart_upload(Some(bucket.as_str()), key, &upload_id, &parts)
                    .await
            }
            .await;

            return match result {
                Ok(etag) => Ok(PutObjectResult { etag, size }),
                Err(err) => {
                    let _ = self
                        .abort_multipart_upload(Some(bucket.as_str()), key, &upload_id)
                        .await;
                    Err(err)
                }
            };
        }

        let mut spec = RequestSpec::new(Method::PUT, Some(bucket), Some(key.to_string()));
        push_object_headers(&mut spec.headers, options)?;
        spec.body = RequestBody::Bytes(data);

        let response = self.send_checked(&spec).await?;
        Ok(PutObjectResult {
            etag: etag_of(&response),
            size,
        })
    }

    /// 按文件路径上传。文件内容按 `part_size` 分块读取，不会整体载入内存。
    /// 开启 `auto_multipart`（默认）且文件超过 `part_size` 时自动走分片上传。
    pub async fn upload_file(
        &self,
        bucket: Option<&str>,
        key: &str,
        path: impl Into<PathBuf>,
        options: &PutObjectOptions,
    ) -> Result<PutObjectResult, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        validate_options(options)?;
        let part_size = options.effective_part_size();
        if part_size == 0 {
            return Err(Error::Config("part_size 必须大于 0".to_string()));
        }

        let path = path.into();
        let size = tokio::fs::metadata(&path).await?.len();
        // 文件可重放：只要传入了具体 MD5，单次 PUT 与自动分片都先流式校验整对象。
        if let Some(expected) = options.content_md5.as_deref() {
            verify_file_md5(expected, &path).await?;
        }
        if options.auto_multipart && size > part_size {
            return self
                .upload_file_multipart(&bucket, key, &path, options, part_size, size)
                .await;
        }

        let mut spec = RequestSpec::new(Method::PUT, Some(bucket), Some(key.to_string()));
        push_object_headers(&mut spec.headers, options)?;
        spec.body = RequestBody::File(path);

        let response = self.send_checked(&spec).await?;
        Ok(PutObjectResult {
            etag: etag_of(&response),
            size,
        })
    }

    async fn upload_file_multipart(
        &self,
        bucket: &str,
        key: &str,
        path: &std::path::Path,
        options: &PutObjectOptions,
        part_size: u64,
        size: u64,
    ) -> Result<PutObjectResult, Error> {
        let upload_id = self
            .create_multipart_upload(
                Some(bucket),
                key,
                options.content_type.as_deref(),
                &options.metadata,
            )
            .await?;

        let result = async {
            let mut file = tokio::fs::File::open(path).await?;
            let mut parts = Vec::new();
            let mut part_number = 1u32;

            loop {
                let mut buffer = vec![0u8; part_size as usize];
                let mut filled = 0usize;
                while filled < buffer.len() {
                    let read = file.read(&mut buffer[filled..]).await?;
                    if read == 0 {
                        break;
                    }
                    filled += read;
                }
                if filled == 0 {
                    break;
                }
                buffer.truncate(filled);
                if part_number > MAX_PARTS {
                    return Err(Error::Config(
                        "分片数超过服务端上限 10000，请增大 part_size".to_string(),
                    ));
                }

                let content_md5 = md5_base64(&buffer);
                let etag = self
                    .upload_part_with_md5(
                        bucket,
                        key,
                        &upload_id,
                        part_number,
                        buffer,
                        content_md5,
                    )
                    .await?;
                parts.push(crate::types::CompletedPart { part_number, etag });

                part_number += 1;
            }

            self.complete_multipart_upload(Some(bucket), key, &upload_id, &parts)
                .await
        }
        .await;

        match result {
            Ok(etag) => Ok(PutObjectResult { etag, size }),
            Err(err) => {
                let _ = self
                    .abort_multipart_upload(Some(bucket), key, &upload_id)
                    .await;
                Err(err)
            }
        }
    }

    /// 读取对象到内存（字节）；传入 Range 时只返回指定区间。
    pub async fn get_object(
        &self,
        bucket: Option<&str>,
        key: &str,
        range: Option<Range>,
    ) -> Result<Vec<u8>, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::GET, Some(bucket), Some(key.to_string()));
        if let Some(range) = range {
            spec.headers
                .push(("range".to_string(), range.to_header_value()));
        }

        let response = self.send_checked(&spec).await?;
        Ok(response
            .bytes()
            .await
            .map_err(map_transport_error)?
            .to_vec())
    }

    /// 下载对象到本地文件（流式写入）；传入 Range 时只下载指定区间。
    pub async fn get_object_to_file(
        &self,
        bucket: Option<&str>,
        key: &str,
        path: impl Into<PathBuf>,
        range: Option<Range>,
    ) -> Result<(), Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::GET, Some(bucket), Some(key.to_string()));
        if let Some(range) = range {
            spec.headers
                .push(("range".to_string(), range.to_header_value()));
        }

        let response = self.send_checked(&spec).await?;
        response_to_file(response, path.into()).await
    }

    /// HEAD 对象，返回 size / etag / content-type / last-modified / 自定义元数据。
    pub async fn head_object(&self, bucket: Option<&str>, key: &str) -> Result<ObjectHead, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let spec = RequestSpec::new(Method::HEAD, Some(bucket), Some(key.to_string()));
        let response = self.send_checked(&spec).await?;
        Ok(object_head_from_response(&response))
    }

    /// 对象是否存在。仅把 404 视为“不存在”，其他错误照常返回。
    pub async fn object_exists(&self, bucket: Option<&str>, key: &str) -> Result<bool, Error> {
        match self.head_object(bucket, key).await {
            Ok(_) => Ok(true),
            Err(err) if err.is_not_found() => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// 删除对象（本服务为软删除进入回收站；对象不存在也返回成功）。
    pub async fn delete_object(&self, bucket: Option<&str>, key: &str) -> Result<(), Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let spec = RequestSpec::new(Method::DELETE, Some(bucket), Some(key.to_string()));
        self.send_checked(&spec).await?;
        Ok(())
    }

    /// 复制对象。
    ///
    /// 未显式指定 `metadata_directive` 时：metadata 为空（包括“传了空 map”）按 `COPY`，
    /// 非空按 `REPLACE`。显式指定时以指定值为准（`COPY` 会忽略 metadata）。
    pub async fn copy_object(
        &self,
        source_bucket: Option<&str>,
        source_key: &str,
        dest_bucket: Option<&str>,
        dest_key: &str,
        metadata: &BTreeMap<String, String>,
        metadata_directive: Option<MetadataDirective>,
    ) -> Result<CopyObjectResult, Error> {
        let source_bucket = self.resolve_bucket(source_bucket)?;
        let dest_bucket = self.resolve_bucket(dest_bucket)?;
        let directive = metadata_directive.unwrap_or(if metadata.is_empty() {
            MetadataDirective::Copy
        } else {
            MetadataDirective::Replace
        });

        let source = format!(
            "/{}/{}",
            signing::uri_encode(&source_bucket, true),
            signing::uri_encode(source_key, false)
        );

        let mut spec = RequestSpec::new(
            Method::PUT,
            Some(dest_bucket),
            Some(dest_key.to_string()),
        );
        spec.headers
            .push(("x-amz-copy-source".to_string(), source));
        spec.headers.push((
            "x-amz-metadata-directive".to_string(),
            directive.as_str().to_string(),
        ));
        if directive == MetadataDirective::Replace {
            validate_metadata(metadata)?;
            for (key, value) in metadata {
                spec.headers
                    .push((format!("x-amz-meta-{key}"), value.to_string()));
            }
        }

        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;

        Ok(CopyObjectResult {
            etag: strip_etag(root.child_text("ETag").unwrap_or_default()),
        })
    }

    /// 批量删除。超过 1000 个 key 时自动按 1000 分批；`quiet = true` 时服务端不返回已删除列表。
    pub async fn delete_objects(
        &self,
        bucket: Option<&str>,
        keys: &[&str],
        quiet: bool,
    ) -> Result<DeleteObjectsResult, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut output = DeleteObjectsResult::default();
        if keys.is_empty() {
            return Ok(output);
        }

        for batch in keys.chunks(MAX_DELETE_BATCH) {
            let mut body = String::from("<Delete>");
            for key in batch {
                body.push_str("<Object><Key>");
                body.push_str(&escape(key));
                body.push_str("</Key></Object>");
            }
            body.push_str(if quiet {
                "<Quiet>true</Quiet></Delete>"
            } else {
                "<Quiet>false</Quiet></Delete>"
            });

            let mut spec = RequestSpec::new(Method::POST, Some(bucket.clone()), None);
            spec.query.push(("delete".to_string(), String::new()));
            spec.headers
                .push(("content-type".to_string(), "application/xml".to_string()));
            spec.body = RequestBody::Bytes(body.into_bytes());

            let response = self.send_checked(&spec).await?;
            let raw = response.bytes().await.map_err(map_transport_error)?;
            let root = xml::parse(&raw).map_err(Error::InvalidResponse)?;

            for deleted in root.children_named("Deleted") {
                if let Some(key) = deleted.child_text("Key") {
                    output.deleted.push(key);
                }
            }
            for error in root.children_named("Error") {
                output.errors.push(DeleteObjectError {
                    key: error.child_text("Key").unwrap_or_default(),
                    code: error.child_text("Code").unwrap_or_default(),
                    message: error.child_text("Message").unwrap_or_default(),
                });
            }
        }
        Ok(output)
    }

    /// ListObjectsV2（支持 prefix / delimiter / max_keys / continuation_token / start_after）。
    pub async fn list_objects_v2(
        &self,
        bucket: Option<&str>,
        prefix: Option<&str>,
        delimiter: Option<&str>,
        max_keys: Option<u32>,
        continuation_token: Option<&str>,
        start_after: Option<&str>,
    ) -> Result<ListObjectsV2Response, Error> {
        let bucket = self.resolve_bucket(bucket)?;
        let mut spec = RequestSpec::new(Method::GET, Some(bucket), None);
        spec.query.push(("list-type".to_string(), "2".to_string()));
        if let Some(prefix) = prefix.filter(|value| !value.is_empty()) {
            spec.query.push(("prefix".to_string(), prefix.to_string()));
        }
        if let Some(delimiter) = delimiter.filter(|value| !value.is_empty()) {
            spec.query
                .push(("delimiter".to_string(), delimiter.to_string()));
        }
        if let Some(max_keys) = max_keys {
            spec.query
                .push(("max-keys".to_string(), max_keys.to_string()));
        }
        if let Some(token) = continuation_token.filter(|value| !value.is_empty()) {
            spec.query
                .push(("continuation-token".to_string(), token.to_string()));
        }
        if let Some(start_after) = start_after.filter(|value| !value.is_empty()) {
            spec.query
                .push(("start-after".to_string(), start_after.to_string()));
        }

        let response = self.send_checked(&spec).await?;
        let body = response.bytes().await.map_err(map_transport_error)?;
        let root = xml::parse(&body).map_err(Error::InvalidResponse)?;

        let mut output = ListObjectsV2Response {
            is_truncated: root
                .child_text("IsTruncated")
                .map(|value| value.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            next_token: root.child_text("NextContinuationToken"),
            ..Default::default()
        };

        for content in root.children_named("Contents") {
            output.items.push(ObjectInfo {
                key: content.child_text("Key").unwrap_or_default(),
                size: content
                    .child_text("Size")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                etag: strip_etag(content.child_text("ETag").unwrap_or_default()),
                last_modified: content.child_text("LastModified"),
                storage_class: content
                    .child_text("StorageClass")
                    .unwrap_or_else(|| "STANDARD".to_string()),
            });
        }
        for common in root.children_named("CommonPrefixes") {
            if let Some(prefix) = common.child_text("Prefix") {
                output.common_prefixes.push(prefix);
            }
        }

        Ok(output)
    }
}

fn validate_options(options: &PutObjectOptions) -> Result<(), Error> {
    if let Some(content_type) = options.content_type.as_deref() {
        validate_header_value(content_type, "Content-Type")?;
    }
    validate_metadata(&options.metadata)?;
    if let Some(content_md5) = options.content_md5.as_deref() {
        validate_header_value(content_md5, "Content-MD5")?;
    }
    Ok(())
}

fn push_object_headers(
    headers: &mut Vec<(String, String)>,
    options: &PutObjectOptions,
) -> Result<(), Error> {
    if let Some(content_type) = options.content_type.as_deref() {
        headers.push(("content-type".to_string(), content_type.to_string()));
    }
    for (key, value) in &options.metadata {
        headers.push((format!("x-amz-meta-{key}"), value.to_string()));
    }
    if let Some(content_md5) = options.content_md5.as_deref() {
        headers.push(("content-md5".to_string(), content_md5.to_string()));
    }
    Ok(())
}

/// 元数据统一校验：key 必须是非空 RFC 7230 token；value 只允许可打印 ASCII `0x20..=0x7E`。
pub(crate) fn validate_metadata(metadata: &BTreeMap<String, String>) -> Result<(), Error> {
    for (key, value) in metadata {
        if key.is_empty() || !key.chars().all(is_token_char) {
            return Err(Error::Config(format!(
                "元数据 key 必须是 RFC 7230 token（ASCII 字母数字与 !#$%&'*+-.^_`|~）：{key}"
            )));
        }
        validate_header_value(value, &format!("x-amz-meta-{key}"))?;
    }
    Ok(())
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

pub(crate) fn validate_header_value(value: &str, name: &str) -> Result<(), Error> {
    // 仅允许可打印 ASCII 0x20..=0x7E：Tab、CR、LF、NUL 等控制字符一律拒绝。
    if !value.bytes().all(|byte| (0x20..=0x7E).contains(&byte)) {
        return Err(Error::Config(format!(
            "请求头 {name} 只能包含可打印 ASCII 字符（0x20..0x7E），不允许 Tab/CR/LF/NUL 等控制字符"
        )));
    }
    Ok(())
}

/// 计算一段字节的 MD5（base64，Content-MD5 头要求格式）。
fn md5_base64(data: &[u8]) -> String {
    STANDARD.encode(Md5::digest(data))
}

/// 本地整对象 MD5 校验（字节）。不匹配时返回与服务端一致的 `BadDigest` 400。
fn verify_bytes_md5(expected: &str, data: &[u8]) -> Result<(), Error> {
    let actual = md5_base64(data);
    if expected != actual {
        return Err(bad_digest(expected, &actual));
    }
    Ok(())
}

/// 本地整对象 MD5 校验（文件，流式分块读取，不整体载入内存）。
async fn verify_file_md5(expected: &str, path: &std::path::Path) -> Result<(), Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = STANDARD.encode(hasher.finalize());
    if expected != actual {
        return Err(bad_digest(expected, &actual));
    }
    Ok(())
}

fn bad_digest(expected: &str, actual: &str) -> Error {
    Error::Service(crate::error::S3Error {
        code: "BadDigest".to_string(),
        message: format!("Content-MD5 不匹配：期望 {expected}，实际 {actual}"),
        status: 400,
        request_id: None,
    })
}

fn etag_of(response: &reqwest::Response) -> String {
    header_string(response.headers(), "etag")
        .map(|etag| etag.trim().trim_matches('"').to_string())
        .unwrap_or_default()
}

fn strip_etag(raw: String) -> String {
    raw.trim().trim_matches('"').to_string()
}
