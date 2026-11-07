//! Local disk storage service with streaming support.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::Stream;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::models::object::key_prefix;

/// Storage service for reading and writing files to disk.
#[derive(Debug, Clone)]
pub struct StorageService {
    base_dir: PathBuf,
}

impl StorageService {
    /// Create a new storage service with the given base directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Get the base directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Get the file path for an object.
    ///
    /// Structure: `{base_dir}/{app_id}/{bucket_id}/{key_prefix}/{object_id}`
    pub fn object_path(&self, app_id: &str, bucket_id: &str, key: &str, object_id: &str) -> PathBuf {
        let prefix = key_prefix(key);
        self.base_dir
            .join(app_id)
            .join(bucket_id)
            .join(prefix)
            .join(object_id)
    }

    /// 对象旁挂的 S3 自定义元数据文件（`x-amz-meta-*`）。
    ///
    /// 与对象文件同目录、文件名加 `.meta.json` 后缀，生命周期与对象文件一致。
    pub fn meta_path(&self, app_id: &str, bucket_id: &str, key: &str, object_id: &str) -> PathBuf {
        let object = self.object_path(app_id, bucket_id, key, object_id);
        object.with_file_name(format!("{object_id}.meta.json"))
    }

    /// 写入 S3 自定义元数据 sidecar。
    pub async fn write_meta(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
        meta: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let path = self.meta_path(app_id, bucket_id, key, object_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let json = serde_json::to_vec(meta).context("Failed to serialize object metadata")?;
        fs::write(&path, json)
            .await
            .with_context(|| format!("Failed to write metadata file: {}", path.display()))?;
        Ok(())
    }

    /// 读取 S3 自定义元数据 sidecar（不存在返回 `None`）。
    pub async fn read_meta(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
    ) -> Result<Option<std::collections::HashMap<String, String>>> {
        let path = self.meta_path(app_id, bucket_id, key, object_id);
        let data = match fs::read(&path).await {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Failed to read metadata file: {}", path.display()))
            }
        };
        let meta = serde_json::from_slice(&data)
            .with_context(|| format!("Failed to parse metadata file: {}", path.display()))?;
        Ok(Some(meta))
    }

    /// 复制对象文件（用于 S3 CopyObject），返回目标路径。
    #[allow(clippy::too_many_arguments)]
    pub async fn copy_object(
        &self,
        src_app_id: &str,
        src_bucket_id: &str,
        src_key: &str,
        src_object_id: &str,
        dst_app_id: &str,
        dst_bucket_id: &str,
        dst_key: &str,
        dst_object_id: &str,
    ) -> Result<()> {
        let from = self.object_path(src_app_id, src_bucket_id, src_key, src_object_id);
        let to = self.object_path(dst_app_id, dst_bucket_id, dst_key, dst_object_id);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        fs::copy(&from, &to)
            .await
            .with_context(|| format!("Failed to copy {} to {}", from.display(), to.display()))?;
        Ok(())
    }

    /// Get the directory path for a bucket.
    pub fn bucket_dir(&self, app_id: &str, bucket_id: &str) -> PathBuf {
        self.base_dir.join(app_id).join(bucket_id)
    }

    /// Initialize the storage base directory.
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.base_dir)
            .await
            .context("Failed to create data directory")?;
        Ok(())
    }

    /// Write a file from a byte stream, returning (etag_md5_hex, size).
    ///
    /// Streams data directly to disk without loading the entire file into memory.
    pub async fn write_stream<S, E>(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
        mut stream: S,
        max_size: u64,
    ) -> Result<(String, u64)>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::error::Error + Send + Sync + 'static,
    {
        use futures_util::StreamExt;
        use md5::{Digest, Md5};

        let path = self.object_path(app_id, bucket_id, key, object_id);

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create directory: {}", parent.display())
            })?;
        }

        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .with_context(|| format!("Failed to create file: {}", path.display()))?;

        let mut hasher = Md5::new();
        let mut total_size: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("Stream error: {}", e))?;

            if total_size + chunk.len() as u64 > max_size {
                // Clean up partial file
                let _ = fs::remove_file(&path).await;
                return Err(anyhow!(
                    "File exceeds maximum allowed size of {} bytes",
                    max_size
                ));
            }

            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .with_context(|| format!("Failed to write to file: {}", path.display()))?;
            total_size += chunk.len() as u64;
        }

        file.flush()
            .await
            .with_context(|| format!("Failed to flush file: {}", path.display()))?;

        let md5_result = hasher.finalize();
        let etag = hex_encode(&md5_result);

        Ok((etag, total_size))
    }

    /// Write a file from bytes (for smaller files / tests).
    pub async fn write_bytes(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
        data: &[u8],
    ) -> Result<(String, u64)> {
        use md5::{Digest, Md5};

        let path = self.object_path(app_id, bucket_id, key, object_id);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create directory: {}", parent.display())
            })?;
        }

        fs::write(&path, data)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        let mut hasher = Md5::new();
        hasher.update(data);
        let result = hasher.finalize();
        let etag = hex_encode(&result);

        Ok((etag, data.len() as u64))
    }

    /// Open a file for reading, returning a stream and file size.
    pub async fn read_stream(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
    ) -> Result<(ReaderStream<File>, u64)> {
        let path = self.object_path(app_id, bucket_id, key, object_id);

        let file = File::open(&path)
            .await
            .with_context(|| format!("Failed to open file: {}", path.display()))?;

        let metadata = file
            .metadata()
            .await
            .with_context(|| format!("Failed to get metadata: {}", path.display()))?;

        let size = metadata.len();
        let stream = ReaderStream::new(file);

        Ok((stream, size))
    }

    /// Read a range of a file, returning a stream and the range length.
    pub async fn read_range(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
        start: u64,
        end: u64,
    ) -> Result<(ReaderStream<tokio::io::Take<File>>, u64)> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let path = self.object_path(app_id, bucket_id, key, object_id);

        let mut file = File::open(&path)
            .await
            .with_context(|| format!("Failed to open file: {}", path.display()))?;

        let metadata = file
            .metadata()
            .await
            .with_context(|| format!("Failed to get metadata: {}", path.display()))?;

        let file_size = metadata.len();
        if start >= file_size || end < start {
            return Err(anyhow!(
                "Invalid range {}-{} for file size {}",
                start,
                end,
                file_size
            ));
        }

        let actual_end = end.min(file_size.saturating_sub(1));
        let range_len = actual_end - start + 1;

        file.seek(std::io::SeekFrom::Start(start))
            .await
            .with_context(|| format!("Failed to seek in file: {}", path.display()))?;

        // Use a limited reader
        let limited = file.take(range_len);
        let stream = ReaderStream::new(limited);

        Ok((stream, range_len))
    }

    /// Read the entire file into bytes (for tests / small files).
    pub async fn read_bytes(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
    ) -> Result<Vec<u8>> {
        let path = self.object_path(app_id, bucket_id, key, object_id);
        let data = fs::read(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        Ok(data)
    }

    /// Delete a file from disk（连同其 S3 元数据 sidecar）。
    pub async fn delete(
        &self,
        app_id: &str,
        bucket_id: &str,
        key: &str,
        object_id: &str,
    ) -> Result<()> {
        let path = self.object_path(app_id, bucket_id, key, object_id);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .with_context(|| format!("Failed to delete file: {}", path.display()))?;
        }
        let meta = self.meta_path(app_id, bucket_id, key, object_id);
        if meta.exists() {
            fs::remove_file(&meta)
                .await
                .with_context(|| format!("Failed to delete metadata file: {}", meta.display()))?;
        }
        Ok(())
    }

    /// Delete all files for a bucket (recursively remove bucket directory).
    pub async fn delete_bucket(&self, app_id: &str, bucket_id: &str) -> Result<()> {
        let path = self.bucket_dir(app_id, bucket_id);
        if path.exists() {
            fs::remove_dir_all(&path)
                .await
                .with_context(|| format!("Failed to delete bucket dir: {}", path.display()))?;
        }
        Ok(())
    }

    /// Delete all files for an app.
    pub async fn delete_app(&self, app_id: &str) -> Result<()> {
        let path = self.base_dir.join(app_id);
        if path.exists() {
            fs::remove_dir_all(&path)
                .await
                .with_context(|| format!("Failed to delete app dir: {}", path.display()))?;
        }
        Ok(())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;
    use tempfile::tempdir;

    fn make_stream(data: &[u8]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        // Split into small chunks to simulate streaming
        let chunks: Vec<Result<Bytes, std::io::Error>> = data
            .chunks(1024)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        Box::pin(stream::iter(chunks))
    }

    #[tokio::test]
    async fn test_write_and_read_bytes() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        let data = b"Hello, OSS World!";
        let (etag, size) = storage
            .write_bytes("app1", "bucket1", "test.txt", "obj1", data)
            .await
            .unwrap();

        assert_eq!(size, data.len() as u64);
        assert_eq!(etag.len(), 32); // MD5 hex

        let read_back = storage
            .read_bytes("app1", "bucket1", "test.txt", "obj1")
            .await
            .unwrap();
        assert_eq!(read_back, data);
    }

    #[tokio::test]
    async fn test_write_stream() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        let data = vec![0x41u8; 10_000]; // 10KB of 'A'
        let stream = make_stream(&data);

        let (_etag, size) = storage
            .write_stream("app1", "bucket1", "stream.txt", "obj2", stream, 1_000_000)
            .await
            .unwrap();

        assert_eq!(size, 10_000);

        let read_back = storage
            .read_bytes("app1", "bucket1", "stream.txt", "obj2")
            .await
            .unwrap();
        assert_eq!(read_back, data);
    }

    #[tokio::test]
    async fn test_write_stream_exceeds_max_size() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        let data = vec![0x41u8; 100];
        let stream = make_stream(&data);

        let result = storage
            .write_stream("app1", "bucket1", "big.txt", "obj3", stream, 50)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_range() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        let data: Vec<u8> = (0..100u8).collect();
        storage
            .write_bytes("app1", "bucket1", "range.txt", "obj4", &data)
            .await
            .unwrap();

        let (stream, len) = storage
            .read_range("app1", "bucket1", "range.txt", "obj4", 10, 19)
            .await
            .unwrap();

        assert_eq!(len, 10);

        // Collect stream
        use futures_util::StreamExt;
        let chunks: Vec<Bytes> = stream.map(|c| c.unwrap()).collect().await;
        let total: Vec<u8> = chunks.into_iter().flatten().collect();
        assert_eq!(total, (10..20u8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn test_delete() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        storage
            .write_bytes("app1", "bucket1", "del.txt", "obj5", b"delete me")
            .await
            .unwrap();

        storage
            .delete("app1", "bucket1", "del.txt", "obj5")
            .await
            .unwrap();

        let result = storage
            .read_bytes("app1", "bucket1", "del.txt", "obj5")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_bucket() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        storage
            .write_bytes("app1", "bucket1", "a.txt", "obj1", b"a")
            .await
            .unwrap();
        storage
            .write_bytes("app1", "bucket1", "b.txt", "obj2", b"b")
            .await
            .unwrap();

        storage.delete_bucket("app1", "bucket1").await.unwrap();

        let bucket_dir = storage.bucket_dir("app1", "bucket1");
        assert!(!bucket_dir.exists());
    }

    #[tokio::test]
    async fn test_object_path_structure() {
        let storage = StorageService::new("/data");

        // Simple key goes to "root" prefix
        let p = storage.object_path("app1", "buck1", "file.txt", "obj-123");
        assert!(p.ends_with("app1/buck1/root/obj-123"));

        // Single level
        let p = storage.object_path("app1", "buck1", "photos/1.jpg", "obj-456");
        assert!(p.ends_with("app1/buck1/photos/obj-456"));

        // Multi-level uses first 2 levels
        let p = storage.object_path("app1", "buck1", "a/b/c/d/file.txt", "obj-789");
        assert!(p.ends_with("app1/buck1/a/b/obj-789"));
    }

    #[tokio::test]
    async fn test_streaming_md5_matches_bytes_md5() {
        let dir = tempdir().unwrap();
        let storage = StorageService::new(dir.path());
        storage.init().await.unwrap();

        let data = vec![0x42u8; 5000];
        let stream = make_stream(&data);

        let (stream_etag, _) = storage
            .write_stream("app1", "b1", "s.txt", "o1", stream, 1_000_000)
            .await
            .unwrap();

        let (bytes_etag, _) = storage
            .write_bytes("app1", "b1", "b.txt", "o2", &data)
            .await
            .unwrap();

        assert_eq!(stream_etag, bytes_etag);
    }
}
