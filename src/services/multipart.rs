//! S3 分片上传（Multipart Upload）的磁盘实现。
//!
//! 不新增数据库表：每次上传在 `{data_dir}/multipart/{uploadId}/` 下保存
//! `manifest.json` 与各分片文件（`{partNumber}.part` 与 `{partNumber}.json`）。
//! 合并成功后由 handler 创建正式对象记录并删除该目录；Abort 直接删除目录。
//!
//! 该目录已在维护服务的孤儿扫描中排除，不会被误删。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::storage::StorageService;

/// 分片上传的元信息（manifest.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultipartUpload {
    pub upload_id: String,
    pub app_id: String,
    pub bucket_id: String,
    pub key: String,
    pub content_type: String,
    pub meta: HashMap<String, String>,
    pub created_at: i64,
}

/// 单个分片的信息（`{partNumber}.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartInfo {
    pub part_number: i32,
    pub etag: String,
    pub size: u64,
}

/// 分片合并结果。
#[derive(Debug, Clone)]
pub struct CompletedUpload {
    pub etag: String,
    pub size: u64,
}

#[derive(Clone)]
pub struct MultipartService {
    base_dir: PathBuf,
}

impl MultipartService {
    pub fn new(storage: &StorageService) -> Self {
        Self {
            base_dir: storage.base_dir().join("multipart"),
        }
    }

    pub fn upload_dir(&self, upload_id: &str) -> PathBuf {
        self.base_dir.join(upload_id)
    }

    fn manifest_path(&self, upload_id: &str) -> PathBuf {
        self.upload_dir(upload_id).join("manifest.json")
    }

    /// 创建一次分片上传。
    pub async fn create(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        content_type: &str,
        meta: HashMap<String, String>,
    ) -> Result<MultipartUpload> {
        let upload = MultipartUpload {
            upload_id: uuid::Uuid::new_v4().to_string(),
            app_id: app_id.to_string(),
            bucket_id: bucket_id.to_string(),
            key: key.to_string(),
            content_type: content_type.to_string(),
            meta,
            created_at: Utc::now().timestamp(),
        };
        let dir = self.upload_dir(&upload.upload_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("Failed to create upload dir: {}", dir.display()))?;
        let json = serde_json::to_vec_pretty(&upload)?;
        tokio::fs::write(self.manifest_path(&upload.upload_id), json)
            .await
            .context("Failed to write multipart manifest")?;
        Ok(upload)
    }

    /// 读取分片上传元信息（不存在返回 `None`）。
    pub async fn load(&self, upload_id: &str) -> Result<Option<MultipartUpload>> {
        if !is_safe_id(upload_id) {
            return Ok(None);
        }
        let path = self.manifest_path(upload_id);
        let data = match tokio::fs::read(&path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to read {}", path.display()))
            }
        };
        let upload = serde_json::from_slice(&data)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Some(upload))
    }

    /// 写入一个分片，返回 MD5 与大小。
    pub async fn put_part<S, E>(
        &self,
        upload_id: &str,
        part_number: i32,
        mut stream: S,
        max_size: u64,
    ) -> Result<PartInfo>
    where
        S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
        E: std::error::Error + Send + Sync + 'static,
    {
        if !is_safe_id(upload_id) {
            return Err(anyhow!("Invalid upload id"));
        }
        let dir = self.upload_dir(upload_id);
        if !dir.is_dir() {
            return Err(anyhow!("NoSuchUpload"));
        }

        let part_path = dir.join(format!("{part_number}.part"));
        let mut file = tokio::fs::File::create(&part_path)
            .await
            .with_context(|| format!("Failed to create {}", part_path.display()))?;
        let mut hasher = Md5::new();
        let mut size = 0u64;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("Failed to read part body: {}", e))?;
            size += chunk.len() as u64;
            if size > max_size {
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(anyhow!("exceeds maximum allowed size"));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .with_context(|| format!("Failed to write {}", part_path.display()))?;
        }
        file.flush().await.context("Failed to flush part")?;

        let info = PartInfo {
            part_number,
            etag: hex_encode(&hasher.finalize()),
            size,
        };
        let json = serde_json::to_vec(&info)?;
        tokio::fs::write(dir.join(format!("{part_number}.json")), json)
            .await
            .context("Failed to write part metadata")?;
        Ok(info)
    }

    /// 删除单个已写入的分片（如 UploadPart 的 Content-MD5 校验失败时回滚）。
    pub async fn remove_part(&self, upload_id: &str, part_number: i32) -> Result<()> {
        if !is_safe_id(upload_id) {
            return Err(anyhow!("Invalid upload id"));
        }
        let dir = self.upload_dir(upload_id);
        if !dir.is_dir() {
            return Err(anyhow!("NoSuchUpload"));
        }
        for ext in ["part", "json"] {
            let path = dir.join(format!("{part_number}.{ext}"));
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("Failed to remove {}", path.display()))
                }
            }
        }
        Ok(())
    }

    /// 列出已上传的分片。
    pub async fn list_parts(&self, upload_id: &str) -> Result<Vec<PartInfo>> {
        if !is_safe_id(upload_id) {
            return Err(anyhow!("Invalid upload id"));
        }
        let dir = self.upload_dir(upload_id);
        if !dir.is_dir() {
            return Err(anyhow!("NoSuchUpload"));
        }
        let mut parts = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .with_context(|| format!("Failed to read {}", dir.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("manifest.json") {
                continue;
            }
            let data = tokio::fs::read(&path).await?;
            if let Ok(info) = serde_json::from_slice::<PartInfo>(&data) {
                parts.push(info);
            }
        }
        parts.sort_by_key(|p| p.part_number);
        Ok(parts)
    }

    /// 合并分片到 `dest`，返回 multipart ETag（`md5-of-md5s-N`）与总大小。
    pub async fn complete(
        &self,
        upload_id: &str,
        requested: &[(i32, String)],
        dest: &Path,
    ) -> Result<CompletedUpload> {
        if requested.is_empty() {
            return Err(anyhow!("InvalidPart: no parts provided"));
        }
        if !is_safe_id(upload_id) {
            return Err(anyhow!("Invalid upload id"));
        }
        let dir = self.upload_dir(upload_id);
        if !dir.is_dir() {
            return Err(anyhow!("NoSuchUpload"));
        }

        let existing = self.list_parts(upload_id).await?;
        let mut used = Vec::with_capacity(requested.len());
        let mut last_number = 0;
        for (number, etag) in requested {
            if *number <= last_number {
                return Err(anyhow!("InvalidPartOrder: parts must be in ascending order"));
            }
            last_number = *number;
            let part = existing
                .iter()
                .find(|p| p.part_number == *number)
                .ok_or_else(|| anyhow!("InvalidPart: part {number} was not uploaded"))?;
            let expected = etag.trim().trim_matches('"');
            if !part.etag.eq_ignore_ascii_case(expected) {
                return Err(anyhow!("InvalidPart: etag mismatch for part {number}"));
            }
            used.push(part.clone());
        }

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let mut out = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("Failed to create {}", dest.display()))?;
        let mut md5_of_md5s = Md5::new();
        let mut total = 0u64;
        for part in &used {
            let path = dir.join(format!("{}.part", part.part_number));
            let data = tokio::fs::read(&path)
                .await
                .with_context(|| format!("Failed to read {}", path.display()))?;
            out.write_all(&data)
                .await
                .with_context(|| format!("Failed to write {}", dest.display()))?;
            total += data.len() as u64;
            let digest = hex_decode(&part.etag)
                .ok_or_else(|| anyhow!("Corrupted part etag {}", part.etag))?;
            md5_of_md5s.update(digest);
        }
        out.flush().await.context("Failed to flush merged object")?;

        let etag = format!(
            "{}-{}",
            hex_encode(&md5_of_md5s.finalize()),
            used.len()
        );
        Ok(CompletedUpload {
            etag,
            size: total,
        })
    }

    /// 删除整个上传目录（Abort / 完成后清理）。
    pub async fn abort(&self, upload_id: &str) -> Result<bool> {
        if !is_safe_id(upload_id) {
            return Ok(false);
        }
        let dir = self.upload_dir(upload_id);
        if !dir.exists() {
            return Ok(false);
        }
        tokio::fs::remove_dir_all(&dir)
            .await
            .with_context(|| format!("Failed to remove {}", dir.display()))?;
        Ok(true)
    }
}

/// uploadId 必须是普通 UUID/安全字符串，避免路径穿越。
fn is_safe_id(upload_id: &str) -> bool {
    !upload_id.is_empty()
        && upload_id.len() <= 128
        && upload_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    for chunk in bytes.chunks(2) {
        let value = u8::from_str_radix(&format!("{}{}", chunk[0] as char, chunk[1] as char), 16).ok()?;
        out.push(value);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;
    use tempfile::tempdir;

    fn storage() -> (tempfile::TempDir, StorageService) {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path().join("data"));
        (dir, storage)
    }

    #[tokio::test]
    async fn multipart_roundtrip_builds_md5_of_md5s_etag() {
        let (_dir, storage) = storage();
        let svc = MultipartService::new(&storage);
        let upload = svc
            .create("app", "bucket", "big.bin", "application/octet-stream", HashMap::new())
            .await
            .unwrap();

        let p1 = svc
            .put_part(
                &upload.upload_id,
                1,
                Box::pin(stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"hello "))])),
                1024,
            )
            .await
            .unwrap();
        let p2 = svc
            .put_part(
                &upload.upload_id,
                2,
                Box::pin(stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"world"))])),
                1024,
            )
            .await
            .unwrap();

        let parts = svc.list_parts(&upload.upload_id).await.unwrap();
        assert_eq!(parts.len(), 2);

        let dest = storage.object_path("app", "bucket", "big.bin", "obj-1");
        let done = svc
            .complete(
                &upload.upload_id,
                &[(1, format!("\"{}\"", p1.etag)), (2, p2.etag.clone())],
                &dest,
            )
            .await
            .unwrap();
        let data = tokio::fs::read(&dest).await.unwrap();
        assert_eq!(data, b"hello world");
        assert_eq!(done.size, 11);
        assert!(done.etag.ends_with("-2"));
        assert_eq!(done.etag.len(), 32 + 1 + 1);

        svc.abort(&upload.upload_id).await.unwrap();
        assert!(!svc.upload_dir(&upload.upload_id).exists());
    }

    #[tokio::test]
    async fn complete_rejects_unknown_part() {
        let (_dir, storage) = storage();
        let svc = MultipartService::new(&storage);
        let upload = svc
            .create("app", "bucket", "k", "text/plain", HashMap::new())
            .await
            .unwrap();
        let dest = storage.object_path("app", "bucket", "k", "obj-2");
        let err = svc
            .complete(&upload.upload_id, &[(1, "deadbeef".into())], &dest)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("InvalidPart"));
    }
}
