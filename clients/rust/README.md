# oss-s3-client（Rust）

OSS Server 官方 Rust 客户端 SDK，对接服务端的 S3 兼容 API。API 契约与 Java / JavaScript / Python 客户端统一：方法名在 Rust 使用 snake_case，参数顺序、语义与返回值结构与其他语言一致。

## 特性

- AWS Signature V4：header 签名 + 预签名 URL（GET / PUT）
- 桶：创建、删除、列表、HEAD（存在性 + region）、location
- 对象：字节 / 文件上传、下载到内存 / 文件、Range、HEAD、复制、删除、批量删除
- ListObjectsV2：prefix / delimiter / max_keys / continuation_token / start_after
- 分片上传：低层 API + 高层自动分片（超过 `part_size` 自动切换，默认 8 MB，失败自动 abort）
- 大文件上传 / 下载全程流式，不把整个文件读入内存
- 重试：可重放请求体重试，429 / 5xx / 网络错误指数退避，404 / 403 不重试
- 错误：解析 S3 XML 错误为 `Error::Service(S3Error { code, message, status, request_id })`

## 安装

```toml
[dependencies]
oss-s3-client = "0.1"
tokio = { version = "1", features = ["full"] }
reqwest = "0.12"   # 仅“预签名 URL 第三方上传”示例需要，业务工程按需添加
```

本地开发可直接引用路径：

```toml
oss-s3-client = { path = "E:/ai_code_proj/oss-server/clients/rust" }
```

## 配置

```rust
use std::time::Duration;
use oss_client::{ClientConfig, OssClient};

let client = OssClient::with_config(
    ClientConfig::new("http://127.0.0.1:18100", "APP_ID", "APP_SECRET")
        .with_region("us-east-1")
        .with_timeout(Duration::from_secs(30))
        .with_max_retries(3),
)?;
```

| 字段 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `endpoint` | 是 | - | 服务端地址，**允许路径前缀**，如 `http://host:8080/api` |
| `access_key` | 是 | - | App ID |
| `secret_key` | 是 | - | App Secret |
| `region` | 否 | `us-east-1` | 任意非空值，仅参与签名 |
| `default_bucket` | 否 | `None` | 默认桶；方法里的 `bucket` 传 `None` 时使用它，未配置则报 `Error::Config` |
| `timeout` | 否 | 30s | 单次请求超时 |
| `max_retries` | 否 | 3 | 失败重试次数（0 表示不重试） |

## 五行快速开始

```rust
let client = OssClient::new("http://127.0.0.1:18100", "APP_ID", "APP_SECRET")?;
client.create_bucket(Some("demo")).await?;                                                    // 配置 + 建桶
let put = client.upload_file(Some("demo"), "docs/a.txt", "./a.txt", &Default::default()).await?; // 上传
client.get_object_to_file(Some("demo"), "docs/a.txt", "./downloaded.txt", None).await?;        // 下载
let url = client.presign_get(Some("demo"), "docs/a.txt", None)?;                              // 预签名，None → 900 秒
```

## 统一 API 契约

以下方法参数顺序固定，语义与其他语言客户端一致。

**bucket 可选**：Rust 没有默认参数，统一用 `Option<&str>` 表达——传 `Some("name")` 显式指定，传 `None` 使用配置里的 `default_bucket`；两者都没有时返回 `Error::Config` 并说明原因。

```rust
// 配置了默认桶后可以省略 bucket
let client = OssClient::with_config(
    ClientConfig::new(endpoint, "APP_ID", "APP_SECRET").with_default_bucket("demo"),
)?;
client.put_object(None, "docs/hello.txt", b"hello".to_vec(), &Default::default()).await?;
client.presign_get(None, "docs/hello.txt", None)?;
```

### 两种写法：绑定桶视图 vs 显式传 bucket

两种写法完全等价，`OssClient` 的 23 个方法签名和语义都没有变化。新增的 `BucketView<'a>` 只是借用客户端的**轻量视图**，不复制 HTTP 连接、连接池或配置，因此不会有额外内存与状态不一致问题。

```rust
// 原来：每次调用都要重复写 Some("demo")
client.put_object(Some("demo"), "docs/a.txt", body, &opts).await?;
client.get_object(Some("demo"), "docs/a.txt", None).await?;
client.list_objects_v2(Some("demo"), Some("docs/"), None, None, None, None).await?;

// 现在：绑定一次桶，后续对象 / 分片调用直接省略 bucket
let demo = client.bucket("demo");
demo.put_object("docs/a.txt", body, &opts).await?;
demo.get_object("docs/a.txt", None).await?;
demo.list_objects_v2(Some("docs/"), None, None, None, None).await?;

// 配置了 default_bucket 时，用同一入口的默认桶视图
client.default_bucket().put_object("docs/hello.txt", b"hello".to_vec(), &opts).await?;
```

`BucketView` 覆盖：`put_object` / `upload_file` / `get_object` / `get_object_to_file` / `head_object` / `object_exists` / `delete_object` / `copy_object`（同桶） / `delete_objects` / `list_objects_v2` / `presign_get` / `presign_put`，以及分片全套 `create_multipart_upload` / `upload_part` / `complete_multipart_upload` / `abort_multipart_upload` / `list_parts`。

**什么时候用哪个**：

- 同一段逻辑反复操作同一个桶 → 用 `client.bucket("name")` 或 `client.default_bucket()`，少传参、可读性好；
- 一次调用涉及多个桶（尤其跨桶复制）→ 用 `OssClient` 显式传 bucket；视图的 `copy_object` 只做**同桶**复制；
- 建桶 / 删桶 / 列桶 / HEAD 桶 / 查 location → 用 `OssClient`。

**桶级方法为什么不在视图里**：`BucketView` 表达的是“在某个桶内部操作对象”，不是桶资源本身；建桶 / 删桶属于账号级生命周期操作，且创建视图时并不保证桶已存在；`list_buckets` 根本不涉及单个桶。因此视图只提供对象级、分片级与预签名方法。

### 桶

| 方法 | 返回 |
| --- | --- |
| `create_bucket(bucket)` | 无 |
| `delete_bucket(bucket)` | 无 |
| `list_buckets()` | 桶列表，每项 `{ name, created_at }` |
| `head_bucket(bucket)` | `{ exists, region }`；region 取自响应头 `x-amz-bucket-region`，缺失时回退配置值；404 时 `exists = false` |
| `bucket_exists(bucket)` | `bool` |
| `get_bucket_location(bucket)` | 字符串；服务端返回空值时统一为 `us-east-1` |

```rust
client.create_bucket(Some("demo")).await?;
assert!(client.bucket_exists(Some("demo")).await?);
let head = client.head_bucket(Some("demo")).await?;
println!("exists={} region={}", head.exists, head.region);
assert_eq!(client.get_bucket_location(Some("demo")).await?, "us-east-1");
```

### 对象

| 方法 | 返回 |
| --- | --- |
| `put_object(bucket, key, body, options)` | `{ etag, size }` |
| `upload_file(bucket, key, path, options)` | `{ etag, size }` |
| `get_object(bucket, key, range)` | `Vec<u8>`（Range 命中时只返回区间） |
| `get_object_to_file(bucket, key, path, range)` | 无 |
| `head_object(bucket, key)` | `{ size, etag, content_type, metadata, last_modified }` |
| `object_exists(bucket, key)` | `bool`（仅 404 视为不存在） |
| `delete_object(bucket, key)` | 无 |
| `copy_object(source_bucket, source_key, dest_bucket, dest_key, metadata, metadata_directive)` | `{ etag }` |
| `delete_objects(bucket, keys, quiet)` | `{ deleted, errors }` |
| `list_objects_v2(bucket, prefix, delimiter, max_keys, continuation_token, start_after)` | `{ items, common_prefixes, is_truncated, next_token }` |

```rust
use oss_client::PutObjectOptions;

// 字节上传（自动分片默认开启）
let options = PutObjectOptions::new()
    .with_content_type("text/plain")
    .with_metadata("owner", "alice")
    .with_content_md5("XUFAKrxLKna5cZ2REBfFkg==") // 可选；此处是 "hello" 的 MD5(base64)
    .with_part_size(8 * 1024 * 1024)
    .with_auto_multipart(true);
let put = client.put_object(Some("demo"), "docs/hello.txt", b"hello".to_vec(), &options).await?;

// 文件上传（按 part_size 分块读取；超过 part_size 自动走分片上传）
client.upload_file(Some("demo"), "docs/big.bin", "./big.bin", &Default::default()).await?;

// 下载
let bytes = client.get_object(Some("demo"), "docs/hello.txt", None).await?;
let range = client.get_object(Some("demo"), "docs/hello.txt", Some(oss_client::Range::FromTo(0, 4))).await?;
client.get_object_to_file(Some("demo"), "docs/big.bin", "./downloaded.bin", None).await?;

// HEAD / 存在性 / 删除
let head = client.head_object(Some("demo"), "docs/hello.txt").await?;
println!("{} {} {}", head.size, head.etag, head.content_type);
client.delete_object(Some("demo"), "docs/hello.txt").await?;
```

复制对象的指令推导：

- 未传 `metadata_directive`：`metadata` 非空 → `REPLACE`；`metadata` 为空（包括“传了空 map”）→ `COPY`，不会因为“传了但为空”判成 `REPLACE`
- 显式传 `Some(MetadataDirective::Copy)`：忽略 `metadata`
- 显式传 `Some(MetadataDirective::Replace)`：使用本次 `metadata` 覆盖

```rust
use std::collections::BTreeMap;
use oss_client::MetadataDirective;

let mut metadata = BTreeMap::new();
metadata.insert("owner".to_string(), "bob".to_string());
// 传了 metadata，默认按 REPLACE
client.copy_object(Some("demo"), "a.txt", Some("demo"), "b.txt", &metadata, None).await?;
// 显式 COPY，metadata 被忽略
client.copy_object(Some("demo"), "a.txt", Some("demo"), "b.txt", &metadata, Some(MetadataDirective::Copy)).await?;
```

批量删除超过 1000 个 key 时自动按 1000 分批，`quiet = true` 时服务端不返回已删除列表；`keys` 为空时直接返回空结果（`deleted` 与 `errors` 都为空），不抛异常：

```rust
let keys = ["a.txt", "b.txt", "c.txt"];
let result = client.delete_objects(Some("demo"), &keys, false).await?;
assert_eq!(result.deleted.len(), 3);
assert!(result.errors.is_empty());
```

分页列表：

```rust
let page = client
    .list_objects_v2(Some("demo"), Some("docs/"), Some("/"), Some(100), None, None)
    .await?;
for item in &page.items {
    println!("{} {}", item.key, item.size);
}
let next = page.next_token; // is_truncated 为 true 时用于下一页
```

### 预签名

```rust
let get_url = client.presign_get(Some("demo"), "docs/a.txt", Some(300))?;  // 秒，1..=604800；None → 900
let put_url = client.presign_put(Some("demo"), "docs/upload.txt", None)?;  // 默认 900 秒

// 第三方拿到 URL 后可直接上传（任意 HTTP 客户端均可，这里以 reqwest 为例，需自行添加该依赖）
reqwest::Client::new().put(put_url).body(b"content".to_vec()).send().await?;
```

### 分片上传

低层 API：

```rust
use std::collections::BTreeMap;
use oss_client::CompletedPart;

let upload_id = client
    .create_multipart_upload(Some("demo"), "big.bin", Some("application/octet-stream"), &BTreeMap::new())
    .await?;

let etag1 = client.upload_part(Some("demo"), "big.bin", &upload_id, 1, part1_bytes).await?;
let etag2 = client.upload_part(Some("demo"), "big.bin", &upload_id, 2, part2_bytes).await?;

let listed = client.list_parts(Some("demo"), "big.bin", &upload_id).await?;
assert!(!listed.is_truncated);

let parts = vec![
    CompletedPart { part_number: 2, etag: etag2 },
    CompletedPart { part_number: 1, etag: etag1 },
];
let etag = client.complete_multipart_upload(Some("demo"), "big.bin", &upload_id, &parts).await?;

// 取消：client.abort_multipart_upload(Some("demo"), "big.bin", &upload_id).await?;
```

- `create_multipart_upload` → `upload_id` 字符串
- `upload_part` / `complete_multipart_upload` → ETag 字符串
- `list_parts` → `{ parts, is_truncated }`
- `abort_multipart_upload` → 无返回值

高层 `upload_file` / `put_object` 在 `auto_multipart = true`（默认）且内容超过 `part_size`（默认 8 MB）时自动走上述流程；任一步失败会自动 abort，避免残留分片。

## 错误处理

```rust
use oss_client::Error;

match client.get_object(Some("demo"), "missing.txt", None).await {
    Ok(data) => println!("{} bytes", data.len()),
    Err(Error::Service(err)) => {
        eprintln!("code={} message={} status={} request_id={:?}",
            err.code, err.message, err.status, err.request_id);
        assert!(err.is_not_found() || err.is_access_denied() || err.is_retryable());
    }
    Err(Error::Timeout) => eprintln!("请求超时"),
    Err(err) => eprintln!("其他错误: {err}"),
}
```

| 情况 | 行为 |
| --- | --- |
| 404（对象 / 桶不存在） | `Error::Service`，`code = NoSuchKey` / `NoSuchBucket`，**不重试** |
| 403（签名错误 / 权限不足） | `Error::Service`，`code = SignatureDoesNotMatch` / `AccessDenied`，**不重试** |
| 429 / 5xx | 指数退避重试（100ms × 2^n），最多 `max_retries` 次 |
| 网络错误 / 超时 | 可重放请求体重试；超时映射为 `Error::Timeout` |
| 一次性流 body | 不可重放，不重试（当前公开 API 的 body 均为字节或文件路径） |

`S3Error` 字段统一为 `code`、`message`、`status`、`request_id`。

## 统一语义说明

- **endpoint 路径前缀**：`http://host:8080/api` 会正确拼接为 `/api/{bucket}/{key}`，签名与预签名 URL 都包含前缀。
- **自定义元数据校验**：key 必须非空且符合 RFC 7230 token（ASCII 字母数字与 ``!#$%&'*+-.^_`|~``）；value 只允许可打印 ASCII `0x20..=0x7E`，Tab、CR、LF、NUL 等控制字符一律拒绝；非法时在本地返回 `Error::Config`，不发出请求。
- **Content-MD5**（`put_object` / `upload_file` 的可选 `content_md5`），三种情况：

  1. **单次 PUT**（`auto_multipart = false`，或数据不超过 `part_size`）：先做**本地整对象 MD5 校验**（不匹配直接返回 `Error::Service`，`code = BadDigest`、`status = 400`，不发请求），再发送整对象 `Content-MD5` 请求头，由服务端复核。
  2. **自动分片**：同样先做**本地整对象 MD5 校验**；随后**不发送整对象 MD5**，改为逐片计算该分片内容的 MD5，作为该分片 `UploadPart` 请求的 `Content-MD5` 头发送。服务端逐片校验，任一分片不匹配返回 `400 BadDigest`（该分片会被服务端回滚，SDK 随后自动 abort 整次上传）。
  3. **不可重放的数据流**：跳过本地整对象校验（当前公开 API 的 body 均为字节或文件路径，均可重放）。

  校验通过与否取决于 `content_md5` 与实际内容的 MD5（base64）是否一致；哈希算法固定为 MD5。
- **分片提交顺序**：`complete_multipart_upload` 提交前一律按 `part_number` 升序排序，调用方传入乱序也安全。
- **预签名有效期**：`presign_get` / `presign_put` 的 `expires_in` 为 `Option<u64>`，传 `None` 时默认 **900 秒**，显式值仍需在 `1..=604800`。
- **流式输入规则**：未知长度的流一律走单次 PUT，不尝试分片；只有**已知长度且可重放**（字节、文件路径、可 seek 流）的数据才允许自动分片。本 crate 当前公开 API 只接受字节与文件路径，不提供不可重放流入口，因此不会出现“看似支持实则失败”的流式分片。其他语言客户端同样遵守该规则。
- **返回值字段**：`put_object` / `upload_file` 含 `etag`、`size`；`copy_object` 含 `etag`；`delete_objects` 含 `deleted`、`errors`；`list_parts` 含 `parts`、`is_truncated`。

## 测试

单元测试（SigV4 使用 AWS 官方测试向量）：

```powershell
$TC="$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-gnu"
$env:PATH="$TC\bin;$TC\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;$env:PATH"
cargo test --lib --test sigv4_vectors --test client_behavior
```

集成测试需要真实服务端（默认端口 18100）：

```powershell
$env:OSS_HOST="127.0.0.1:18100"
$env:OSS_DATA_DIR="$env:TEMP\oss-server-rust-data"
$env:OSS_ADMIN_TOKEN="rust-sdk-it-admin"
$env:OSS_STORAGE_BACKEND="redb"
$env:OSS_CLEANUP_INTERVAL_MINUTES="0"
$env:OSS_STARTUP_CHECK_MODE="report"
& "C:\Users\zy\AppData\Local\Temp\oss-server-build-rust\release\oss-server.exe"

# 另一个终端
$env:OSS_TEST_ENDPOINT="http://127.0.0.1:18100"
$env:OSS_TEST_ADMIN_TOKEN="rust-sdk-it-admin"
cargo test --test integration
```

测试用管理 API `POST /api/admin/apps` 自动创建 App 获取 accessKey / secretKey；也支持直接传 `OSS_TEST_ACCESS_KEY` / `OSS_TEST_SECRET_KEY`。

## 已知限制

- 仅支持 path-style 寻址（本服务端未实现 virtual-hosted style）
- 未实现 ACL、Versioning、标签、生命周期、对象锁
- `list_parts` 依赖服务端一次性返回全部分片，未实现 `part-number-marker` 分页
- 自定义元数据 key 仅支持 RFC 7230 token；value 仅支持可打印 ASCII `0x20..=0x7E`（不含 Tab/CR/LF/NUL）
- 自动分片路径不发送整对象 Content-MD5，只逐片发送分片 MD5（见上文说明）
- 不提供不可重放流式输入；未知长度的流需调用方自行缓冲后走单次 PUT，避免“未知长度却尝试分片”
- 预签名 PUT 不绑定 Content-Type / Content-Length，第三方可自由设置

## 发布

```powershell
cargo package
cargo publish
```
