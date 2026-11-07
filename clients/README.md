# OSS Server 官方客户端 SDK 总览

这是 Rust 版 OSS 服务器（S3 兼容协议 + AWS Signature V4）的官方客户端 SDK 集合。五个子项目已经按同一份契约改造：23 个核心方法在 Rust、Java、JavaScript、TypeScript、Python 中一一对应，参数顺序相同，只有命名惯例（camelCase / snake_case）和语言本地的 body、options 载体类型不同。同一套 `endpoint + App ID/App Secret` 配置、path-style 寻址、自动分片、批量删除、预签名与 S3 错误映射，可以在五种语言之间直接迁移。

## 语言对照表

下表的版本号、坐标、最低运行时均取自各子项目的 `Cargo.toml`、`pom.xml`、`package.json`、`pyproject.toml`。测试数量未在本机重新统计，统一写“见该子项目 README”。

| 语言 | 目录 | 安装方式 | 包名 / 坐标 | 最低运行时要求 | 测试命令 | 测试数量 |
| --- | --- | --- | --- | --- | --- | --- |
| Rust | [`clients/rust`](rust/) | 在 `Cargo.toml` 中加 path 依赖；发布到 registry 后可用版本依赖 | crate `oss-s3-client` `0.1.0` | Rust `1.75`（edition 2021），异步运行时需 `tokio` | 单元：`cargo test --lib --test sigv4_vectors --test client_behavior`；集成：按 README 启动服务端并设置 `OSS_TEST_ENDPOINT`（或 `OSS_TEST_ADMIN_TOKEN`）后 `cargo test --test integration` | 见该子项目 README |
| Java | [`clients/java`](java/) | 在 `clients/java` 执行 `mvn install`；也可直接使用 `target/oss-s3-client-1.0.0.jar` | `com.oss:oss-s3-client:1.0.0` | JDK 8+；构建需 Maven 3.3+；运行期零第三方依赖 | 单元：`mvn test`（未设置集成环境变量时集成测试自动跳过）；集成：`powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-integration.ps1` | 见该子项目 README |
| JavaScript | [`clients/js`](js/) | `npm install oss-s3-client`；本仓库本地安装用 `npm install E:/ai_code_proj/oss-server/clients/js` | npm 包 `oss-s3-client` `1.0.0` | Node.js >= 14.17；零运行时依赖，仅用内置模块 | `npm run build`；`npm run test:unit`；`npm run test:integration`；`npm test`（单元 + 集成） | 见该子项目 README |
| TypeScript | [`clients/typescript`](typescript/) | `npm install @pddon/oss-s3-client`；本仓库本地开发用 `npm install` 后 `npm run build`，再以本地路径安装 | npm 包 `@pddon/oss-s3-client` `0.1.0` | Node.js >= 14.17；零运行时依赖，使用内置 `crypto` / `http` / `https` | `npm run typecheck`；`npm run build`；`npm run test:unit`；`npm run test:integration`；`npm test`（build + 单元 + 集成） | 见该子项目 README |
| Python | [`clients/python`](python/) | 在 `clients/python` 执行 `python -m pip install -e .`；或把该目录加入 `PYTHONPATH` | 发行包 `oss-s3-client` `0.1.0`（import 名为 `oss_client`） | Python >= 3.8；构建需 setuptools >= 61；运行期零第三方依赖 | 单元：`python -m unittest discover -s tests -p "test_unit.py" -v`；`python -m unittest discover -s tests -p "test_signer.py" -v`；集成：`python -m unittest discover -s tests -p "test_integration.py" -v`；全部：`python -m unittest discover -s tests -v` | 见该子项目 README |

JavaScript 与 TypeScript 是两个独立发布的包：`clients/js` 是 `oss-s3-client@1.0.0`；`clients/typescript` 是 `@pddon/oss-s3-client@0.1.0`，构建产物为 `dist/cjs`（CommonJS）、`dist/esm`（ESM）与 `dist/types`（`.d.ts`），因此 JS 项目也可以直接 `require('@pddon/oss-s3-client')`。

Rust 的本地依赖写法：

```toml
[dependencies]
oss-s3-client = { path = "../oss-server/clients/rust" }  # 发布后可改为 oss-s3-client = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Java 的 Maven 坐标写法：

```xml
<dependency>
    <groupId>com.oss</groupId>
    <artifactId>oss-s3-client</artifactId>
    <version>1.0.0</version>
</dependency>
```

## 三步接入

1. **拿到 endpoint 与凭据**：endpoint 是服务端地址（默认 `http://127.0.0.1:8080`）。在管理控制台 `/admin/` 或管理 API（`POST /api/admin/apps`，请求头 `X-Admin-Token`）创建 App，返回的 `app_id` 即 AccessKey，`app_secret` 即 Secret。region 可填任意非空值（默认 `us-east-1`），服务端只用它参与签名。
2. **配置客户端**：填入 endpoint、AccessKey、Secret；五种 SDK 都支持可选默认桶（调用时 bucket 传空即回退到配置的默认桶）、超时、重试次数。endpoint 允许携带路径前缀（如 `http://host:8080/api`），五种 SDK 都会正确拼接并签名。
3. **上传第一个文件**：建桶（已存在则跳过）→ PutObject（默认超过 8MB 自动分片）→ 生成预签名 GET 链接。

下面每段代码都可直接运行，endpoint 统一用本服务默认的 `http://127.0.0.1:8080`。

### Rust

```rust
use oss_client::{OssClient, PutObjectOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 配置
    let client = OssClient::new("http://127.0.0.1:8080", "APP_ID", "APP_SECRET")?;

    // 2. 建桶（已存在则跳过）
    if !client.bucket_exists(Some("demo")).await? {
        client.create_bucket(Some("demo")).await?;
    }

    // 3. 上传（超过 8MB 自动分片）
    let options = PutObjectOptions::default().with_content_type("text/plain");
    client
        .put_object(Some("demo"), "hello.txt", b"hello".to_vec(), &options)
        .await?;

    // 4. 预签名下载链接（600 秒有效）
    let url = client.presign_get(Some("demo"), "hello.txt", Some(600))?;
    println!("{url}");
    Ok(())
}
```

### Java

```java
import com.oss.client.ClientConfig;
import com.oss.client.OssClient;
import com.oss.client.PutObjectOptions;
import java.nio.charset.StandardCharsets;

public class QuickStart {
    public static void main(String[] args) {
        // 1. 配置
        ClientConfig cfg = ClientConfig.builder(
                "http://127.0.0.1:8080", "APP_ID", "APP_SECRET")
            .build();
        OssClient oss = new OssClient(cfg);

        // 2. 建桶（已存在则跳过）
        if (!oss.bucketExists("demo")) {
            oss.createBucket("demo");
        }

        // 3. 上传（超过 8MB 自动分片）
        oss.putObject("demo", "hello.txt", "hello".getBytes(StandardCharsets.UTF_8),
                PutObjectOptions.builder().contentType("text/plain").build());

        // 4. 预签名下载链接（600 秒有效）
        String url = oss.presignGet("demo", "hello.txt", 600);
        System.out.println(url);
    }
}
```

### JavaScript

```js
// quickstart.js（CommonJS）
const { OSSClient } = require('oss-s3-client');

async function main() {
  // 1. 配置
  const client = new OSSClient({
    endpoint: 'http://127.0.0.1:8080',
    accessKey: 'APP_ID',
    secretKey: 'APP_SECRET',
  });

  // 2. 建桶（已存在则跳过）
  if (!(await client.bucketExists('demo'))) {
    await client.createBucket('demo');
  }

  // 3. 上传（超过 8MB 自动分片）
  await client.putObject('demo', 'hello.txt', Buffer.from('hello'), {
    contentType: 'text/plain; charset=utf-8',
  });

  // 4. 预签名下载链接（600 秒有效）
  const url = client.presignGet('demo', 'hello.txt', 600);
  console.log(url);
  client.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
```

### TypeScript

```ts
// quickstart.ts
import { OSSClient } from '@pddon/oss-s3-client';

async function main(): Promise<void> {
  // 1. 配置
  const client = new OSSClient({
    endpoint: 'http://127.0.0.1:8080',
    accessKey: 'APP_ID',
    secretKey: 'APP_SECRET',
    region: 'us-east-1',
  });

  // 2. 建桶（已存在则跳过）
  if (!(await client.bucketExists('demo'))) {
    await client.createBucket('demo');
  }

  // 3. 上传（超过 8MB 自动分片）
  await client.putObject('demo', 'hello.txt', Buffer.from('hello'), {
    contentType: 'text/plain; charset=utf-8',
  });

  // 4. 预签名下载链接（600 秒有效）
  const url = client.presignGet('demo', 'hello.txt', 600);
  console.log(url);
  client.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
```

本机编译运行：`npx tsc quickstart.ts --module commonjs --target es2019 --esModuleInterop`，然后 `node quickstart.js`。

### Python

```python
from oss_client import OSSClient

# 1. 配置
client = OSSClient("http://127.0.0.1:8080", "APP_ID", "APP_SECRET")

# 2. 建桶（已存在则跳过）
if not client.bucket_exists("demo"):
    client.create_bucket("demo")

# 3. 上传（超过 8MB 自动分片）
client.put_object("demo", "hello.txt", b"hello", {"content_type": "text/plain"})

# 4. 预签名下载链接（600 秒有效）
url = client.presign_get("demo", "hello.txt", 600)
print(url)
```

## 统一契约：23 个方法对照

规范名使用 snake_case。五门参数顺序相同；Rust / Python 用 snake_case，Java / JS / TypeScript 用 camelCase。`options` 在 Rust / Java 是类型化选项对象，在 JS / TypeScript 是对象、Python 是字典，字段对应关系为：

| 契约字段 | Rust | Java / JS / TypeScript | Python |
| --- | --- | --- | --- |
| 对象 Content-Type | `content_type` | `contentType` | `content_type` |
| 自定义元数据 | `metadata` | `metadata` | `metadata` |
| 整对象 Content-MD5 | `content_md5` | `contentMd5` | `content_md5` |
| 分片阈值 / 分片大小 | `part_size` | `partSize` | `part_size` |
| 自动分片开关 | `auto_multipart` | `autoMultipart` | `auto_multipart` |

默认值：`part_size = 8MB`、`auto_multipart = true`。预签名有效期范围统一为 `1..604800` 秒，默认值统一为 `900` 秒。

下表 `bucket` 参数的省略方式：Rust 传 `None`，Java 传 `null`，JS / TypeScript 传 `undefined` 或 `null`，Python 传 `None`；省略时使用配置里的默认桶。JS / TypeScript 的多数网络方法还接受末尾 `{ timeout?, retries? }` 覆盖项。

| 方法（规范名） | Rust | Java | JavaScript | TypeScript | Python | 说明 |
| --- | --- | --- | --- | --- | --- | --- |
| `create_bucket` | `create_bucket(bucket)` | `createBucket(bucket)` | `createBucket(bucket, options?)` | `createBucket(bucket, options?)` | `create_bucket(bucket)` | 桶已存在返回 409 `BucketAlreadyOwnedByYou` |
| `delete_bucket` | `delete_bucket(bucket)` | `deleteBucket(bucket)` | `deleteBucket(bucket, options?)` | `deleteBucket(bucket, options?)` | `delete_bucket(bucket)` | 桶内仍有 active 对象时返回 409 `BucketNotEmpty` |
| `list_buckets` | `list_buckets()` | `listBuckets()` | `listBuckets(options?)` | `listBuckets(options?)` | `list_buckets()` | 返回 `name` + `createdAt/created_at` |
| `head_bucket` | `head_bucket(bucket) -> BucketHead` | `headBucket(bucket) -> BucketHeadResult` | `headBucket(bucket) -> {exists, region}` | `headBucket(bucket) -> BucketHead` | `head_bucket(bucket) -> BucketInfo` | 404 返回 `exists=false`；region 读 `x-amz-bucket-region`，缺失回退配置值 |
| `bucket_exists` | `bucket_exists(bucket) -> bool` | `bucketExists(bucket) -> boolean` | `bucketExists(bucket) -> Promise<boolean>` | `bucketExists(bucket) -> Promise<boolean>` | `bucket_exists(bucket) -> bool` | 等价于 `head_bucket` 的布尔视图 |
| `get_bucket_location` | `get_bucket_location(bucket) -> String` | `getBucketLocation(bucket) -> String` | `getBucketLocation(bucket) -> Promise<string>` | `getBucketLocation(bucket) -> Promise<string>` | `get_bucket_location(bucket) -> str` | 空值统一归一化为 `us-east-1` |
| `put_object` | `put_object(bucket, key, body, options)` | `putObject(bucket, key, body, options)` | `putObject(bucket, key, body, options?)` | `putObject(bucket, key, body, options?)` | `put_object(bucket, key, body, options=None)` | 可重放 body 超过 `part_size` 自动分片；一次性流走单次 PUT |
| `upload_file` | `upload_file(bucket, key, path, options)` | `uploadFile(bucket, key, path, options)` | `uploadFile(bucket, key, filePath, options)` | `uploadFile(bucket, key, filePath, options?)` | `upload_file(bucket, key, path, options=None)` | 语义固定为“本地文件路径”，选项与 `put_object` 一致 |
| `get_object` | `get_object(bucket, key, range) -> Vec<u8>` | `getObject(bucket, key, range) -> GetObjectResponse` | `getObject(bucket, key, range, options) -> Buffer\|Readable` | `getObject(bucket, key, range?, options?) -> Buffer\|Readable` | `get_object(bucket, key, range=None) -> bytes` | `range` 接受字符串或语言本地区间表示，最终都发 `Range` 头 |
| `get_object_to_file` | `get_object_to_file(bucket, key, path, range)` | `getObjectToFile(bucket, key, path, range)` | `getObjectToFile(bucket, key, filePath, range, options)` | `getObjectToFile(bucket, key, filePath, range?, options?)` | `get_object_to_file(bucket, key, path, range=None)` | 流式落盘，不整体读入内存；TypeScript 失败可能残留部分文件 |
| `head_object` | `head_object(bucket, key) -> ObjectHead` | `headObject(bucket, key) -> ObjectMetadata` | `headObject(bucket, key, options) -> object` | `headObject(bucket, key, options?) -> ObjectHead` | `head_object(bucket, key) -> HeadObjectResult` | 返回 size / etag / content-type / metadata / last-modified |
| `object_exists` | `object_exists(bucket, key) -> bool` | `objectExists(bucket, key) -> boolean` | `objectExists(bucket, key, options) -> Promise<boolean>` | `objectExists(bucket, key, options?) -> Promise<boolean>` | `object_exists(bucket, key) -> bool` | 404 返回 `false`，其余错误抛出 |
| `delete_object` | `delete_object(bucket, key)` | `deleteObject(bucket, key)` | `deleteObject(bucket, key, options)` | `deleteObject(bucket, key, options?)` | `delete_object(bucket, key)` | 软删除进回收站；对象不存在也返回成功 |
| `copy_object` | `copy_object(src_bucket, src_key, dst_bucket, dst_key, metadata, metadata_directive)` | `copyObject(srcBucket, srcKey, dstBucket, dstKey, metadata, metadataDirective)` | `copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata, metadataDirective)` | `copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata?, metadataDirective?)` | `copy_object(source_bucket, source_key, dest_bucket, dest_key, metadata=None, metadata_directive=None)` | 显式 directive 优先；未显式指定时，非空 metadata 推导 `REPLACE`，否则 `COPY` |
| `delete_objects` | `delete_objects(bucket, keys, quiet)` | `deleteObjects(bucket, keys, quiet)` | `deleteObjects(bucket, keys, quiet, options)` | `deleteObjects(bucket, keys, quiet?, options?)` | `delete_objects(bucket, keys, quiet=False)` | 超过 1000 个 key 自动按 1000 分批并合并结果；空 keys 返回空结果 |
| `list_objects_v2` | `list_objects_v2(bucket, prefix, delimiter, max_keys, continuation_token, start_after)` | `listObjectsV2(bucket, prefix, delimiter, maxKeys, continuationToken, startAfter)` | `listObjectsV2(bucket, prefix, delimiter, maxKeys, continuationToken, startAfter, options)` | `listObjectsV2(bucket, prefix?, delimiter?, maxKeys?, continuationToken?, startAfter?, options?)` | `list_objects_v2(bucket, prefix=None, delimiter=None, max_keys=1000, continuation_token=None, start_after=None)` | 分页、前缀、分隔符、start-after 全支持；均单次返回一页，需自行循环 `nextToken`（Python 另有 `iter_objects_v2`） |
| `create_multipart_upload` | `create_multipart_upload(bucket, key, content_type, metadata) -> upload_id` | `createMultipartUpload(bucket, key, contentType, metadata) -> uploadId` | `createMultipartUpload(bucket, key, contentType, metadata, options) -> uploadId` | `createMultipartUpload(bucket, key, contentType?, metadata?) -> Promise<string>` | `create_multipart_upload(bucket, key, content_type=None, metadata=None) -> upload_id` | 初始化分片上传 |
| `upload_part` | `upload_part(bucket, key, upload_id, part_number, body) -> etag` | `uploadPart(bucket, key, uploadId, partNumber, body) -> etag` | `uploadPart(bucket, key, uploadId, partNumber, body, options) -> etag` | `uploadPart(bucket, key, uploadId, partNumber, body, options?) -> Promise<string>` | `upload_part(bucket, key, upload_id, part_number, body, content_md5=None) -> etag` | `part_number` 取值 `1..10000`；ETag 去掉引号返回 |
| `complete_multipart_upload` | `complete_multipart_upload(bucket, key, upload_id, parts) -> etag` | `completeMultipartUpload(bucket, key, uploadId, parts) -> etag` | `completeMultipartUpload(bucket, key, uploadId, parts, options) -> etag` | `completeMultipartUpload(bucket, key, uploadId, parts, options?) -> Promise<string>` | `complete_multipart_upload(bucket, key, upload_id, parts) -> etag` | 五门都会在提交前按 `partNumber` 升序排序 |
| `abort_multipart_upload` | `abort_multipart_upload(bucket, key, upload_id)` | `abortMultipartUpload(bucket, key, uploadId)` | `abortMultipartUpload(bucket, key, uploadId, options)` | `abortMultipartUpload(bucket, key, uploadId, options?)` | `abort_multipart_upload(bucket, key, upload_id)` | 立即清理服务端临时分片 |
| `list_parts` | `list_parts(bucket, key, upload_id) -> ListPartsResult` | `listParts(bucket, key, uploadId) -> ListPartsResult` | `listParts(bucket, key, uploadId, options) -> object` | `listParts(bucket, key, uploadId, options?) -> ListPartsResult` | `list_parts(bucket, key, upload_id) -> ListPartsResult` | 服务端一次性返回全部分片，`is_truncated` 恒为 `false`；客户端不做分页 |
| `presign_get` | `presign_get(bucket, key, expires_in: Option<u64>) -> String` | `presignGet(bucket, key, expiresSeconds?) -> String` | `presignGet(bucket, key, expiresIn?) -> string` | `presignGet(bucket, key, expiresIn?) -> string` | `presign_get(bucket, key, expires_in=900) -> str` | 有效期 `1..604800` 秒，默认 900 秒 |
| `presign_put` | `presign_put(bucket, key, expires_in: Option<u64>) -> String` | `presignPut(bucket, key, expiresSeconds?) -> String` | `presignPut(bucket, key, expiresIn?) -> string` | `presignPut(bucket, key, expiresIn?) -> string` | `presign_put(bucket, key, expires_in=900) -> str` | 只签 `host`，不绑定 `Content-Type` 等请求头 |

补充：Java 的 `putObject` 另有 `File` / `Path` / `InputStream` 重载，`uploadPart` 另有 `File`、`File + offset/length`、`InputStream + contentLength` 重载；Python 的 `upload_part` 额外接受可选的 `content_md5`；TypeScript 的 `getObject` 通过重载在 `options.stream = true` 时返回 `Readable`。这些是语言本地扩展，不改变上表的规范参数顺序。

## Content-MD5 行为

五种语言都支持可选的整对象 `Content-MD5`，字段为 `content_md5` / `contentMd5`。服务端在 PutObject 与 UploadPart 上都会校验该头，不匹配返回 `400 BadDigest`；UploadPart 校验失败时该分片不会写入 upload（协议文档与 `src/s3/object.rs` 的 `verify_content_md5` 已确认）。

| 场景 | 行为 |
| --- | --- |
| **单次 PUT** | 传入的整对象 MD5 作为 `Content-MD5` 请求头随 PutObject 发送，由服务端校验。 |
| **自动分片** | 不发送整对象 MD5；SDK 对每个分片计算 MD5，并通过 UploadPart 的 `Content-MD5` 逐片发送，服务端逐片校验。 |
| **可重放 body 的本地整对象预校验** | 在发送前计算整对象 MD5 与传入值比较，不匹配直接本地报错、不发请求。触发时机见下表。 |
| **不可重放的一次性流** | 跳过本地整对象校验；单次 PUT 仍可发送整对象 `Content-MD5` 头，交由服务端校验；不参与自动分片。 |

各语言的实际触发时机：

| 语言 | 单次 PUT 本地预校验 | 自动分片本地预校验 | 逐片 MD5 | 一次性流 |
| --- | --- | --- | --- | --- |
| Rust | 是（字节 / 文件路径，传入具体值时） | 是（字节 / 文件路径） | 是 | 无公开的一次性流上传 API |
| Java | 是（`byte[]` / `File` / `Path`） | 是（`byte[]` / `File` / `Path`） | 是 | `InputStream` 只走单次 PUT、不预校验、不重试、不自动分片 |
| JavaScript | 是（Buffer / 文件，传 Base64 字符串时） | 是（Buffer / 文件） | 是 | `Readable` 不预校验、不重试、不自动分片；`contentMd5: true` 直接抛错 |
| TypeScript | 是（Buffer / Uint8Array / string / 文件，传 Base64 字符串时） | 是（Buffer / 文件） | 是 | `Readable` 不预校验、不重试、不自动分片；`contentMd5: true` 直接抛错 |
| Python | 是（可重放 body） | 是（可重放 body） | 是 | 不可重放流跳过本地校验，单次 chunked PUT，由服务端校验 |

## 能力矩阵

`✅` 表示该语言存在可直接调用的公开方法。下表逐格对照了各子项目源码中的方法定义与行为。

| 能力 | Rust | Java | JavaScript | TypeScript | Python |
| --- | --- | --- | --- | --- | --- |
| 桶创建 / 删除 / 列表 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 桶 HEAD（`exists` + `region`） | ✅ 读 `x-amz-bucket-region` | ✅ 读 header，缺失回退配置 | ✅ 读 header，缺失回退配置 | ✅ 读 header，缺失回退配置 | ✅ 读 header，缺失回退配置 |
| 桶 location 归一化 | ✅ `us-east-1` | ✅ `us-east-1` | ✅ `us-east-1` | ✅ `us-east-1` | ✅ `us-east-1` |
| 对象上传（`put_object`） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 超阈值自动分片（`put_object`） | ✅ 字节 / 文件路径 | ✅ `byte[]` / `File` / `Path`（`InputStream` 除外） | ✅ Buffer / 文件；`Readable` 一律单次 PUT | ✅ Buffer / Uint8Array / string / 文件；`Readable` 一律单次 PUT（不分片、不重试） | ✅ 可重放 body（可 seek 文件对象） |
| 按文件路径上传（`upload_file`） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 对象下载（内存 / 流式落盘） | ✅ | ✅（`GetObjectResponse` 可读流） | ✅（`{ stream: true }`） | ✅（`{ stream: true }`） | ✅ |
| 对象 HEAD（含自定义元数据） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 对象存在性（`object_exists`） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 对象复制（自动推导 directive） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 对象删除（软删除） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 列表分页 / 前缀 / delimiter / start-after | ✅ | ✅ | ✅ | ✅ | ✅（另有 `iter_objects_v2` 自动翻页） |
| 批量删除（>1000 自动分批） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 分片上传（低层 5 方法） | ✅ | ✅ | ✅ | ✅（`listParts` 取单页，不自动翻页） | ✅ |
| 预签名 GET | ✅ | ✅ | ✅ | ✅ | ✅ |
| 预签名 PUT | ✅ | ✅ | ✅ | ✅ | ✅ |
| 自定义元数据（写入 / 读取） | ✅ | ✅ | ✅ | ✅ | ✅ |
| 元数据校验（key 为 RFC 7230 token，value 为可打印 ASCII） | ✅ | ✅ | ✅ | ✅ | ✅ |
| Content-MD5 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Range 下载 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 错误码映射 | ✅ `Error` / `S3Error` | ✅ `OssException` | ✅ `OSSError` | ✅ `OSSError` | ✅ `OSSServiceError` 及子类 |
| endpoint 路径前缀 | ✅ | ✅ | ✅ | ✅ | ✅ |

## 已知限制与差异汇总

### 跨语言共同限制

- **仅 path-style**：统一使用 `http://host:port[/prefix]/bucket/key`，不支持 virtual-hosted-style（`bucket.host`）。Java 的 `pathStyle(false)` 仅对支持 vhost 的 S3 服务有意义，本服务端不适用。
- **ListParts 无分页**：协议文档明确服务端一次性返回全部分片、`IsTruncated` 恒为 `false`；五种 SDK 都没有提供 `ListMultipartUploads`。
- **预签名只签 `host`**：payload hash 为 `UNSIGNED-PAYLOAD`，预签名 PUT 不绑定 `Content-Type`、`Content-MD5` 等请求头。
- **预签名有效期**：统一为 `1..604800` 秒（7 天），超出范围直接抛错。
- **删除是软删除**：对象进入回收站，删除不存在的 key 也可能返回成功；彻底清除由服务端保留策略负责。
- **自定义元数据受 HTTP 头限制**：key 必须是 RFC 7230 token；value 只允许可打印 ASCII（`0x20..0x7E`），中文需自行编码后存放；读回时 key 可能被服务端统一为小写。
- **region 不校验**：五种客户端都会把 region 放进 SigV4 scope，服务端只把它参与签名、不校验；签名 service 固定为 `s3`。
- **未覆盖的 S3 扩展**：ACL、Bucket Policy、Versioning、Object Lock、Tagging、Select、UploadPartCopy 等均未实现。

### 各语言特有差异

**Rust**

- 异步 SDK，基于 `reqwest` + `tokio`；`OssClient: Clone`，可跨任务共享。
- 最低 Rust 1.75（edition 2021）。
- 公开上传 body 只有字节和文件路径两种，二者都可重放，因此自动分片与重试都按可重放语义处理。
- bucket 参数为 `Option<&str>`：传 `None` 时使用 `ClientConfig::with_default_bucket` 配置的默认桶，两者都没有则报错。
- 重试：`429` / `5xx` / 网络错误指数退避（100ms × 2^n），`404` / `403` 不重试。

**Java**

- 最低 JDK 8；运行期零第三方依赖，使用 JDK 自带的 `HttpURLConnection` 与 `javax.xml`。
- `OssClient` 无可变状态，可多线程共享。
- `byte[]` / `File` / `Path` 可重放，支持自动分片与重试；`InputStream` 是一次性流，不自动分片、不重试，且无法做本地整对象 MD5 预校验。
- 重试：`408` / `429` / `5xx` 指数退避（200ms 起翻倍、上限 2s）。
- bucket 传 `null` 时使用配置的 `defaultBucket`。

**JavaScript**

- 最低 Node.js 14.17；内部只用内置 `http` / `https`，不依赖全局 `fetch`。
- 同时提供 CommonJS、ESM 和 TypeScript 类型。
- 流式 PUT：`Readable` 一律走单次 PUT，无论是否已知长度；长度未知时用 chunked 传输，不自动分片、不重试。
- 一次性 `Readable` 不重试；字节、文件路径可重试，且每次重试重新签名。
- bucket 传 `undefined` / `null` 时使用配置的默认桶。

**TypeScript**

- 最低 Node.js 14.17；Node 14 环境用内置 `http` / `https` 而非全局 `fetch`；使用内置 `crypto` 做 SigV4 与 MD5。
- 运行期零第三方依赖；构建产物为 `dist/cjs`（CommonJS）、`dist/esm`（ESM）、`dist/types`（`.d.ts`）；入口由 `package.json` 的 `exports` 分流。
- 公共 API 与内部实现分离，`strict: true` 全量类型检查（`npm run typecheck`）。
- 流式上传：`Readable` 一律单次 PUT，不自动分片、不重试；未知长度用 chunked 传输。
- `Buffer` / `Uint8Array` / `string` 路径会在内存中持有完整对象；超大文件请用 `uploadFile` 或流式上传。
- `listParts` 依赖服务端一次性返回全部分片，只取单页，不自动翻页。
- 预签名 `expiresIn` 上限为 7 天（604800 秒），默认 900 秒。
- `getObjectToFile` 失败时可能留下部分写入的本地文件，需要调用方自行清理。
- 测试使用自带零依赖 harness（Node 14 没有 `node:test`），不依赖 jest / vitest；集成测试会自行启动真实服务端（固定 `127.0.0.1:18104`）。

**Python**

- 最低 Python 3.8；运行期零第三方依赖，使用标准库 `http.client` / `urllib` / `hmac` / `xml.etree`。
- 复用单个 HTTP 连接，非线程安全；多线程场景请为每个线程创建独立客户端。支持 `with OSSClient(...) as client`。
- 只有可重放请求体（bytes、路径、可 seek 文件对象）才重试；不可 seek 的一次性流只尝试一次。
- `put_object` 对可重放 body 默认自动分片；不可重放的一次性流走单次 PUT，使用 `UNSIGNED-PAYLOAD`。
- 提供 `iter_objects_v2` 生成器，按键自动翻页。
- bucket 传 `None` 时使用配置的默认桶。

## 统一契约核对结果

原先“跨语言不一致待统一”的 13 项，逐项对照最新源码后的结论如下：

| # | 原差异项 | 当前结论 | 核对依据 |
| --- | --- | --- | --- |
| 1 | 对象复制元数据指令 | 已统一 | 五门都有 `copy_object` 的 directive 推导与显式覆盖；空 metadata 一律视为 `COPY` |
| 2 | 批量删除超过 1000 个 key | 已统一 | 五门都自动按 1000 分批并合并结果 |
| 3 | `getBucketLocation` 空值处理 | 已统一 | 五门都归一化为 `us-east-1` |
| 4 | 桶 HEAD 的 region 来源 | 已统一 | 五门都优先读 `x-amz-bucket-region`，缺失回退配置 region |
| 5 | 对象存在性便捷方法 | 已统一 | 五门都有 `object_exists` / `objectExists` |
| 6 | 流式请求体的重试语义 | 已统一 | 五门都是“可重放 body 才重试”；一次性流不重试 |
| 7 | `putObject` 的自动分片语义 | 已统一 | 五门默认 `autoMultipart=true`、阈值 8MB；可重放 body 超阈值自动分片；一次性流走单次 PUT |
| 8 | 高层自动分片的输入类型 | 已统一 | 统一由 `put_object` 承担自动分片，具体 body 类型按语言映射；流式输入统一不分片 |
| 9 | endpoint 路径前缀 | 已统一 | 五门都能拼接并签名 `/prefix/{bucket}/{key}` |
| 10 | `deleteObjects` 的 `quiet` 参数 | 已统一 | 五门都有 `quiet`，默认 `false` |
| 11 | `Content-MD5` | 已统一 | 五门都支持可选的整对象 MD5；自动分片逐片发送 MD5；可重放 body 单次与分片都做本地整对象预校验 |
| 12 | metadata key 校验规则 | 已统一 | 五门都要求 RFC 7230 token；value 统一限制为可打印 ASCII `0x20..0x7E` |
| 13 | API 命名与参数形态 | 已统一 | 23 个方法参数顺序一致，仅 camelCase / snake_case 与 options 载体不同 |

### 原先 8 项残留差异的核对结果

上一版列出的 8 项边界差异，按最新源码逐项核对后已全部统一：

| # | 原残留差异 | 当前结论 | 核对依据 |
| --- | --- | --- | --- |
| 1 | `copy_object` 传空 metadata map 时的 directive | 已统一为 `COPY` | Rust `metadata.is_empty()`；Java `metadata != null && !metadata.isEmpty()`；JS/TypeScript `Object.keys(metadata).length > 0`；Python `if meta` |
| 2 | `delete_objects` 传空 keys | 已统一为空结果、不发请求 | Rust `keys.is_empty()`；Java `keys.isEmpty()`；JS/TypeScript 循环不执行；Python `range(0, 0)` |
| 3 | metadata value 的控制字符边界 | 已统一为仅可打印 ASCII `0x20..0x7E` | Rust `validate_header_value`；Java `S3Models.validateMetadata`；JS/TypeScript `metadataHeaders`；Python `validate_metadata` |
| 4 | Content-MD5 本地整对象预校验时机 | 已统一为单次 PUT 与自动分片都预校验（可重放 body） | Rust `verify_bytes_md5` / `verify_file_md5` 在分支前；Java `verifyContentMd5` 在单次与分片两处；JS/TypeScript 在 `_putBuffer` / `_putFile` 与分片前；Python `_verify_whole_md5` 在分支前 |
| 5 | `complete_multipart_upload` 的分片排序 | 已统一为提交前按 `partNumber` 升序排序 | Rust `sort_by_key`；Java `sortedParts`；JS/TypeScript `.sort(...)`；Python `normalized.sort(...)` |
| 6 | 预签名默认有效期 | 已统一为 900 秒 | Rust `DEFAULT_PRESIGN_EXPIRES_SECS`；Java `DEFAULT_PRESIGN_EXPIRES_SECONDS`；JS/TypeScript `900`；Python `expires_in=900` |
| 7 | 默认桶 | 已统一为 bucket 可省略并回退默认桶 | Rust `resolve_bucket` + `default_bucket`；Java `resolveBucket`；JS/TypeScript `_resolveBucket`；Python `_resolve_bucket` |
| 8 | 未知长度流的自动分片覆盖 | 已统一为单次 PUT、不分片、不重试 | Java `InputStream` 明确单次 PUT；JS/TypeScript `Readable` 一律 `_putStream`；Python 不可 seek 流 `needs_chunked` 单次 PUT；Rust 无公开流式上传 API（不适用） |

本轮未发现新的契约级残留差异。各语言的本地扩展（Java 的 `File` / `Path` / `InputStream` 重载、Python 的 `iter_objects_v2`、JS/TypeScript 的逐调用 timeout/retries 覆盖）不影响 23 方法的参数顺序与语义。

### 其他不影响契约的边界差异

以下两处是实现细节差异，服务端语义一致，不影响正常调用：

1. **`copy_object` 的 metadata 校验时机**：Java / Python 无论 directive 是否为 `COPY` 都会先校验 metadata；Rust / JS / TypeScript 只在 `REPLACE` 分支校验。若同时传入非法 metadata 且显式指定 `COPY`，Java / Python 会本地抛错，另外三门忽略该 metadata。
2. **`delete_objects` 的 `Quiet` 节点**：Rust 在 `quiet=false` 时也会显式发送 `<Quiet>false</Quiet>`；Java / JS / TypeScript / Python 只在 `quiet=true` 时发送 `<Quiet>true</Quiet>`。服务端对缺失节点的默认行为即为 `false`。

## 协议文档

如果对某个请求/响应的行为有疑问（例如错误码、分页字段、Range 语义、Content-MD5 校验、未兼容清单），以服务端协议文档 [`docs/s3-compatible-api.md`](../docs/s3-compatible-api.md) 为准；各语言 README 只描述客户端封装方式，不替代协议定义。
