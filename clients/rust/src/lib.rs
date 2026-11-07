//! OSS Server 官方 Rust 客户端 SDK（S3 兼容 API）。
//!
//! 设计目标：
//! - 与 AWS S3 SDK 保持一致的调用心智，但只覆盖 OSS Server 已兼容的能力；
//! - path-style 寻址 + SigV4（header 签名 / 预签名 URL）；
//! - 大文件上传/下载全程流式，不把整个对象读入内存。
//!
//! # 快速开始
//!
//! ```no_run
//! use pddon_oss_client::OssClient;
//!
//! # async fn demo() -> Result<(), pddon_oss_client::Error> {
//! let client = OssClient::new("http://127.0.0.1:8080", "APP_ID", "APP_SECRET")?;
//! client.create_bucket(Some("my-bucket")).await?;
//! client.put_object(Some("my-bucket"), "hello.txt", b"hello".to_vec(), &Default::default()).await?;
//! let url = client.presign_get(Some("my-bucket"), "hello.txt", None)?; // None → 默认 900 秒
//! # let _ = url;
//! # Ok(())
//! # }
//! ```
//!
//! 更完整的示例见 README.md。

mod bucket;
mod bucket_view;
mod client;
mod config;
mod error;
mod multipart;
mod object;
pub mod signing;
mod types;
mod xml;

pub use bucket_view::BucketView;
pub use client::OssClient;
pub use config::ClientConfig;
pub use error::{Error, S3Error};
pub use types::{
    BucketHead, BucketInfo, CompletedPart, CopyObjectResult, DeleteObjectError,
    DeleteObjectsResult, ListObjectsV2Response, ListPartsResult, MetadataDirective, ObjectHead,
    ObjectInfo, PartInfo, PutObjectOptions, PutObjectResult, Range, DEFAULT_PART_SIZE,
};
