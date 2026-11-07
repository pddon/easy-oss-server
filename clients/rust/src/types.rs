//! 请求 / 响应数据类型与统一客户端契约中的 options。

use std::collections::BTreeMap;

/// 默认分片大小：8 MB。
pub const DEFAULT_PART_SIZE: u64 = 8 * 1024 * 1024;

/// HTTP Range 请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// `bytes=start-end`（闭区间）。
    FromTo(u64, u64),
    /// `bytes=start-`。
    From(u64),
    /// `bytes=-suffix`（最后 N 字节）。
    Suffix(u64),
}

impl Range {
    pub(crate) fn to_header_value(self) -> String {
        match self {
            Range::FromTo(start, end) => format!("bytes={start}-{end}"),
            Range::From(start) => format!("bytes={start}-"),
            Range::Suffix(len) => format!("bytes=-{len}"),
        }
    }
}

/// `put_object` / `upload_file` 的统一选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectOptions {
    /// 对象的 Content-Type；不传时由服务端按 `application/octet-stream` 处理。
    pub content_type: Option<String>,
    /// 自定义元数据，发送为 `x-amz-meta-*`。
    pub metadata: BTreeMap<String, String>,
    /// 可选的 `Content-MD5`（base64 字符串），传了就随单次 PUT 发送。
    pub content_md5: Option<String>,
    /// 自动分片阈值 / 分片大小，默认 [`DEFAULT_PART_SIZE`]。
    pub part_size: Option<u64>,
    /// 是否开启自动分片，默认 `true`。
    pub auto_multipart: bool,
}

impl Default for PutObjectOptions {
    fn default() -> Self {
        Self {
            content_type: None,
            metadata: BTreeMap::new(),
            content_md5: None,
            part_size: None,
            auto_multipart: true,
        }
    }
}

impl PutObjectOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn with_content_md5(mut self, content_md5: impl Into<String>) -> Self {
        self.content_md5 = Some(content_md5.into());
        self
    }

    pub fn with_part_size(mut self, part_size: u64) -> Self {
        self.part_size = Some(part_size);
        self
    }

    pub fn with_auto_multipart(mut self, auto_multipart: bool) -> Self {
        self.auto_multipart = auto_multipart;
        self
    }

    pub(crate) fn effective_part_size(&self) -> u64 {
        self.part_size.unwrap_or(DEFAULT_PART_SIZE)
    }
}

/// CopyObject 的元数据指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataDirective {
    /// 复用源对象的元数据与 Content-Type。
    Copy,
    /// 使用本次传入的元数据覆盖目标对象。
    Replace,
}

impl MetadataDirective {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MetadataDirective::Copy => "COPY",
            MetadataDirective::Replace => "REPLACE",
        }
    }
}

/// 桶信息（ListBuckets 返回项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketInfo {
    pub name: String,
    /// ISO8601 创建时间。
    pub created_at: String,
}

/// 桶 HEAD 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketHead {
    /// 桶是否存在；404 时返回 `false` 而不是错误。
    pub exists: bool,
    /// 优先取响应头 `x-amz-bucket-region`，缺失时回退到配置的 region。
    pub region: String,
}

/// 列表返回的单个对象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: Option<String>,
    pub storage_class: String,
}

/// ListObjectsV2 响应（统一契约字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListObjectsV2Response {
    pub items: Vec<ObjectInfo>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    /// 是否截断为 true 时才有下一页 token。
    pub next_token: Option<String>,
}

/// HEAD 对象结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectHead {
    /// 本次响应的 Content-Length（Range 请求时为区间长度）。
    pub size: u64,
    pub etag: String,
    pub content_type: String,
    pub last_modified: Option<String>,
    /// `x-amz-meta-*` 自定义元数据。
    pub metadata: BTreeMap<String, String>,
}

/// PutObject / 高层 upload_file 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectResult {
    pub etag: String,
    pub size: u64,
}

/// CopyObject 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyObjectResult {
    pub etag: String,
}

/// 批量删除中单个失败 key 的信息（成功的不算错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteObjectError {
    pub key: String,
    pub code: String,
    pub message: String,
}

/// 批量删除结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteObjectsResult {
    pub deleted: Vec<String>,
    pub errors: Vec<DeleteObjectError>,
}

/// 分片上传中的单个分片（ListParts 返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartInfo {
    pub part_number: u32,
    pub etag: String,
    pub size: u64,
    pub last_modified: Option<String>,
}

/// ListParts 结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListPartsResult {
    pub parts: Vec<PartInfo>,
    pub is_truncated: bool,
}

/// CompleteMultipartUpload 时提交的分片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPart {
    pub part_number: u32,
    pub etag: String,
}
