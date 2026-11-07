# OSS Java SDK（oss-client）

Rust 版 OSS 服务器的官方 Java 客户端，兼容 S3 协议（AWS Signature V4），运行期零第三方依赖（仅用 JDK 8 自带 HTTP/XML/MD5/SHA256）。

本 SDK 与 Rust / JavaScript / Python 客户端遵循**同一套统一契约**：语义、参数顺序、参数含义、返回值结构完全一致，方法名在 Java 侧映射为 camelCase。

## 环境要求

- JDK 8 及以上（`java -version`、`javac -version`）
- Maven 3.3+（`mvn -v`）
- 运行期无第三方依赖；测试仅依赖 JUnit 4

## 安装

本地安装到 Maven 仓库：

```bash
cd clients/java
mvn install
```

业务项目引入：

```xml
<dependency>
    <groupId>com.oss</groupId>
    <artifactId>oss-client</artifactId>
    <version>1.0.0</version>
</dependency>
```

## 配置

```java
ClientConfig config = ClientConfig.builder(
        "http://127.0.0.1:18101", // endpoint，可带路径前缀，如 http://host:8080/api
        "your-app-id",            // accessKey = App ID
        "your-app-secret")        // secretKey = App Secret
        .region("us-east-1")      // 任意值，仅参与 SigV4 签名
        .defaultBucket("demo")    // 可选：方法未传 bucket 时回退
        .connectTimeoutMillis(10_000)
        .readTimeoutMillis(30_000)
        .maxRetries(3)            // 仅对可重放请求生效
        .pathStyle(true)          // 本服务端只支持 path-style
        .multipartPartSizeBytes(8L * 1024 * 1024) // 默认分片大小/阈值
        .build();

OssClient oss = new OssClient(config);
```

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `endpoint` | 必填 | 服务地址，允许路径前缀（前缀会参与签名与预签名 URL） |
| `accessKey` | 必填 | 应用 App ID |
| `secretKey` | 必填 | 应用 App Secret |
| `region` | `us-east-1` | 任意值，仅参与签名；服务端不校验 |
| `defaultBucket` | 无 | 可选默认桶：方法传入 `null` 时回退；也没有则抛 `IllegalArgumentException` 并说明原因 |
| `connectTimeoutMillis` | `10000` | 建连超时 |
| `readTimeoutMillis` | `30000` | 读超时 |
| `maxRetries` | `3` | 失败重试次数（不含首次） |
| `pathStyle` | `true` | path-style 寻址 |
| `multipartPartSizeBytes` | `8MB` | 默认分片大小，同时是自动分片阈值 |

## 五行快速开始

```java
ClientConfig cfg = ClientConfig.builder("http://127.0.0.1:18101", appId, appSecret).defaultBucket("demo").build();
OssClient oss = new OssClient(cfg);
oss.uploadFile("demo", "hello.txt", "hello.txt", PutObjectOptions.defaults());
oss.getObjectToFile("demo", "hello.txt", "downloaded.txt", null);
String url = oss.presignGet("demo", "hello.txt", 600);
```

## 统一方法契约

参数顺序固定，bucket 在前。返回值结构字段名与外语言保持一致。

| # | 方法 | 返回 |
| --- | --- | --- |
| 1 | `createBucket(String bucket)` | `void` |
| 2 | `deleteBucket(String bucket)` | `void` |
| 3 | `listBuckets()` | `List<BucketInfo>`，每项含 `name`、`createdAt` |
| 4 | `headBucket(String bucket)` | `BucketHeadResult`，含 `exists`、`region` |
| 5 | `bucketExists(String bucket)` | `boolean` |
| 6 | `getBucketLocation(String bucket)` | `String`（空值归一化为 `us-east-1`） |
| 7 | `putObject(String bucket, String key, body, PutObjectOptions options)` | `PutObjectResult`，含 `etag`、`size` |
| 8 | `uploadFile(String bucket, String key, String path, PutObjectOptions options)` | 同上 |
| 9 | `getObject(String bucket, String key, String range)` | `GetObjectResponse`（流） |
| 10 | `getObjectToFile(String bucket, String key, String path, String range)` | `void` |
| 11 | `headObject(String bucket, String key)` | `ObjectMetadata`，含 `size`、`etag`、`contentType`、`metadata`、`lastModified` |
| 12 | `objectExists(String bucket, String key)` | `boolean` |
| 13 | `deleteObject(String bucket, String key)` | `void` |
| 14 | `deleteObjects(String bucket, Collection<String> keys, boolean quiet)` | `DeleteObjectsResult`，含 `deleted`、`errors` |
| 15 | `copyObject(srcBucket, srcKey, dstBucket, dstKey, Map metadata, String metadataDirective)` | `CopyObjectResult`，含 `etag` |
| 16 | `listObjectsV2(bucket, prefix, delimiter, maxKeys, continuationToken, startAfter)` | `ListObjectsResult`，含 `items`、`commonPrefixes`、`isTruncated`、`nextToken` |
| 17 | `createMultipartUpload(bucket, key, contentType, metadata)` | `String uploadId` |
| 18 | `uploadPart(bucket, key, uploadId, partNumber, body)` | `String etag` |
| 19 | `completeMultipartUpload(bucket, key, uploadId, List<CompletedPart> parts)` | `String etag` |
| 20 | `abortMultipartUpload(bucket, key, uploadId)` | `void` |
| 21 | `listParts(bucket, key, uploadId)` | `ListPartsResult`，含 `parts`、`isTruncated()` |
| 22 | `presignGet(String bucket, String key, long expiresIn)` | `String url`；`expiresIn` 可省略，默认 `900` 秒 |
| 23 | `presignPut(String bucket, String key, long expiresIn)` | `String url`；`expiresIn` 可省略，默认 `900` 秒 |

补充说明：

- `putObject` 的 `body` 支持 `byte[]`、`File`、`Path`、`InputStream`（另有带 `contentLength` 的流式重载）。
- `PutObjectOptions`：`contentType`、`metadata`、`contentMd5`、`partSize`、`autoMultipart`（默认 `true`）。
- `autoMultipart` 开启且对象大小超过 `partSize` 时自动走分片上传；`putObjectAuto` 是同义别名，行为一致。
- `copyObject` 未显式指定 `metadataDirective` 时：传了**非空** `metadata` 按 `REPLACE`，没传或传空 Map 按 `COPY`。
- `deleteObjects` 传入空 `keys`（`null` 或空集合）时直接返回空结果（`deleted`、`errors` 均为空），不抛异常；超过 1000 个 Key 自动分批并合并结果；`quiet` 默认 `false`。
- `completeMultipartUpload` 提交前按 `partNumber` 升序排序，调用方传入顺序不影响结果。
- `metadata` 的 key 必须是 RFC 7230 token 字符；value 只允许可打印 ASCII `0x20..0x7E`（禁止 Tab、CR、LF、NUL 等控制字符）；非法时本地抛异常、不发请求。
- `bucket` 参数允许传 `null`，此时使用配置的 `defaultBucket`；配置也没有默认桶时抛 `IllegalArgumentException` 并说明原因。

## API 示例

### 桶

```java
oss.createBucket("demo");
BucketHeadResult head = oss.headBucket("demo");
System.out.println(head.exists + " / " + head.region);
boolean exists = oss.bucketExists("demo");
String region = oss.getBucketLocation("demo"); // 空值返回 us-east-1
List<BucketInfo> buckets = oss.listBuckets();
oss.deleteBucket("demo"); // 非空桶会抛 409 BucketNotEmpty
```

### 上传（单次 / 自动分片）

```java
Map<String, String> meta = new HashMap<>();
meta.put("owner", "java-sdk");

PutObjectOptions options = PutObjectOptions.builder()
        .contentType("text/plain")
        .metadata(meta)
        .contentMd5("XUFAKrxLKna5cZ2REBfFkg==") // 可选，Base64
        .partSize(8L * 1024 * 1024)                  // 超过即自动分片
        .autoMultipart(true)
        .build();

PutObjectResult r1 = oss.putObject("demo", "a.txt", new File("a.txt"), options);
PutObjectResult r2 = oss.uploadFile("demo", "b.txt", "b.txt", options);
PutObjectResult r3 = oss.putObject("demo", "c.txt", bytes, PutObjectOptions.defaults());

System.out.println(r1.etag + " / " + r1.size);
```

流式上传不会整体进内存：

```java
try (InputStream in = new FileInputStream("big.bin")) {
    oss.putObject("demo", "big.bin", in, 10_000_000L, PutObjectOptions.defaults());
}
```

> 流式输入的统一规则：
>
> - 未知长度的流一律走单次 PUT（chunked 传输），**不尝试分片**。
> - 已知长度且可重放的数据（字节数组、文件）才参与自动分片；一次性 `InputStream` 即使传入 `contentLength` 也不可重放，因此同样只走单次 PUT，且永不重试。
> - Java SDK 不会假装支持“不可重放流的分片”；需要分片请使用 `byte[]` / `File` / `Path`，或自行调用低层 `uploadPart`。

### 下载 / Range

```java
import java.io.ByteArrayOutputStream;
import java.io.InputStream;

try (GetObjectResponse response = oss.getObject("demo", "a.txt", null)) {
    // GetObjectResponse 是流式响应，公开 API 只有 body()（InputStream）；
    // 没有 readAll()，需要自己读流。
    InputStream in = response.body();
    ByteArrayOutputStream out = new ByteArrayOutputStream();
    byte[] buffer = new byte[8192];
    int n;
    while ((n = in.read(buffer)) != -1) {
        out.write(buffer, 0, n);
    }
    byte[] data = out.toByteArray();
}

try (GetObjectResponse range = oss.getObject("demo", "a.txt", "bytes=0-99")) {
    System.out.println(range.statusCode);              // 206
    System.out.println(range.metadata.contentRange);   // bytes 0-99/...
}

oss.getObjectToFile("demo", "a.txt", "download.txt", null);
oss.getObjectToFile("demo", "a.txt", "part.txt", "bytes=100-");
```

### Head / 存在性 / 删除

```java
ObjectMetadata meta = oss.headObject("demo", "a.txt");
System.out.println(meta.size + " " + meta.etag + " " + meta.contentType + " " + meta.lastModified);
System.out.println(meta.metadata.get("owner"));

boolean found = oss.objectExists("demo", "a.txt");
oss.deleteObject("demo", "a.txt"); // 软删除，进回收站；重复删除也成功

DeleteObjectsResult batch = oss.deleteObjects("demo", keys, false);
System.out.println(batch.deleted.size() + " deleted, " + batch.errors.size() + " errors");

// 空 keys 直接返回空结果，不发请求
DeleteObjectsResult empty = oss.deleteObjects("demo", Collections.emptyList(), false);
```

### 复制

```java
// 只传非空 metadata -> 自动 REPLACE；不传 metadata 或传空 Map -> COPY
oss.copyObject("demo", "a.txt", "demo", "copy.txt", meta, null);
oss.copyObject("demo", "a.txt", "demo", "copy-empty.txt", new HashMap<>(), null); // COPY

// 显式 COPY 时 metadata 不生效
oss.copyObject("demo", "a.txt", "demo", "copy2.txt", meta, "COPY");
```

### 列表

```java
ListObjectsResult page = oss.listObjectsV2("demo", "dir/", "/", 100, null, null);
for (ObjectSummary item : page.items) {
    System.out.println(item.key + " " + item.size + " " + item.etag);
}
System.out.println(page.commonPrefixes + " " + page.isTruncated() + " " + page.nextToken());
```

### 分片上传（低层）

```java
String uploadId = oss.createMultipartUpload("demo", "big.bin", "application/octet-stream", meta);

List<CompletedPart> completed = new ArrayList<>();
completed.add(new CompletedPart(1, oss.uploadPart("demo", "big.bin", uploadId, 1, file, 0, partSize)));
completed.add(new CompletedPart(2, oss.uploadPart("demo", "big.bin", uploadId, 2, file, partSize, partSize)));

ListPartsResult parts = oss.listParts("demo", "big.bin", uploadId);
String etag = oss.completeMultipartUpload("demo", "big.bin", uploadId, completed);

// 放弃上传
oss.abortMultipartUpload("demo", "big.bin", uploadId);
```

### 预签名 URL

```java
String getUrl = oss.presignGet("demo", "a.txt", 600);   // 10 分钟
String putUrl = oss.presignPut("demo", "b.txt", 600);   // 第三方可直接 PUT
String defaultUrl = oss.presignGet("demo", "a.txt");    // 省略 expiresIn，默认 900 秒
String headUrl = oss.presign("HEAD", "demo", "a.txt", 600);
```

预签名请求 payload hash 为 `UNSIGNED-PAYLOAD`，`expiresIn` 可省略（默认 900 秒），有效范围 1..604800 秒。

## 错误处理

```java
try {
    oss.getObject("demo", "missing.txt", null);
} catch (OssException e) {
    System.out.println(e.code());       // NoSuchKey
    System.out.println(e.message());    // 服务端原始 message
    System.out.println(e.status());     // 404
    System.out.println(e.requestId());  // 服务端 request id
}
```

统一错误对象字段：`code`、`message`、`status`、`requestId`。

| 场景 | 行为 |
| --- | --- |
| 404（NoSuchKey / NoSuchBucket / NoSuchUpload） | 抛 `OssException`，`status=404`，不重试 |
| 403（AccessDenied / SignatureDoesNotMatch / InvalidAccessKeyId） | 抛 `OssException`，`status=403`，不重试 |
| 408 / 429 / 5xx | 指数退避重试（200ms 起，上限 2s），用尽后抛异常 |
| 连接/读超时 | 重试用尽后抛 `OssTimeoutException`（`code=RequestTimeout`、`status=0`） |
| 一次性 InputStream 上传失败 | 永不重试（请求体不可重放） |
| 错误 secret / AccessKey | 服务端返回 403，不重试 |
| 重复建桶 | 409 `BucketAlreadyOwnedByYou` |
| 非空桶删除 | 409 `BucketNotEmpty` |
| 本地参数/元数据非法 | 直接抛 `IllegalArgumentException`，不发请求 |

辅助判断：`isNotFound()`、`isAccessDenied()`、`isServerError()`。

### Content-MD5 行为

- **单次 PUT**（对象不超过 `partSize`，或 `autoMultipart=false`）：把 `contentMd5` 作为 `Content-MD5` 请求头发送，由服务端校验。
- **自动分片**：不发送整对象 MD5（不符合分片语义）；SDK 对每个分片单独计算 MD5，并作为该分片 UploadPart 请求的 `Content-MD5` 头发送，由服务端逐片校验。
- **本地整对象预校验（统一规则）**：只要传入了具体 `contentMd5` 且请求体可重放（字节数组、文件），**无论单次 PUT 还是自动分片**，SDK 都会在发请求前计算整对象 MD5 并比较，不匹配立即抛 `BadDigest`（`status=0`，不发请求）。一次性 `InputStream` 不可重放，跳过本地预校验，仅由服务端按上述规则校验。

## 测试

### 单元测试

```bash
mvn test
```

- `SigV4SignerTest`：使用 AWS 官方文档签名测试向量（header 签名、PUT 签名、预签名 URL），非自签自验。
- `SdkUnitTest`：配置与路径前缀、Key 编码、错误 XML 解析、List/ListParts/Complete/Delete XML 解析、元数据校验（含 Tab/CR/NUL 拒绝）、copy directive 推导（含空 Map → COPY）、空 keys 删除返回空结果、Content-MD5 本地预校验、分片升序排序、预签名默认 900 秒、默认桶缺失报错、location 归一化、批量删除分批、Content-MD5 选项等。

未设置环境变量时，`S3IntegrationTest` 会被 JUnit `Assume` 整类跳过。

### 集成测试（真实服务端）

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-integration.ps1
```

脚本行为：

1. 校验 `target\release\oss-server.exe` 比源码新且包含 SigV4 标记；否则依次尝试其它候选路径，最后用 `CARGO_TARGET_DIR` 指向临时目录构建。
2. 设置 `OSS_HOST=127.0.0.1:18101`、`OSS_DATA_DIR=%TEMP%\oss-server-java-data`、`OSS_STORAGE_BACKEND=redb`、`OSS_ADMIN_TOKEN` 等环境变量。
3. 启动服务端并轮询 `/healthz`；通过管理 API 自动创建 App 获取 accessKey/secret。
4. 运行 `mvn -B test`；结束后在 `finally` 中停止服务端并等待端口释放。

集成覆盖：建桶/headBucket/存在性/location/list/默认桶（bucket=null）、上传（单次/文件/流/自动分片）、下载与 Range、head、复制（COPY/REPLACE 推导/空 Map→COPY）、单个与批量删除（含 >1000 分批）、列表分组与分页/start-after、预签名 PUT/GET/默认 900 秒/过期 403/错误签名 403、分片上传全流程（乱序 parts 升序提交）与 abort、Content-MD5 正确/本地错误/逐片错误、403 与 409 错误码、网络错误。

当前结果：44 个测试全部通过（8 集成 + 27 单元 + 9 SigV4）。

## 已知限制

1. 本服务端只支持 path-style；`pathStyle(false)` 仅客户端侧可用。
2. `InputStream` 上传不整体进内存，因此无法自动分片，也不重试；未知长度流一律单次 PUT（chunked）。
3. 自动分片的最小分片大小由服务端决定；本服务端不强制 5MB 下限，但对接 AWS 等实现时应使用 ≥5MB。
4. `listParts` 服务端一次性返回全部分片，`isTruncated()` 恒为 `false`。
5. `listObjectsV2` 的 `maxKeys` 实际上限由服务端限制（通常 1000）。
6. 桶名、region 不做本地格式校验，以服务端返回为准。
7. 删除为软删除（进入回收站），服务端清理策略由 `OSS_TRASH_RETENTION_DAYS` 决定。
8. endpoint 路径前缀会正确参与请求与签名，但需要服务端实际挂载在该前缀下。
9. 自动分片时 SDK 发送逐片 `Content-MD5`，由服务端逐片校验；整对象 MD5 只在可重放的字节数组/文件上做本地预校验。

## 目录结构

```
clients/java/
├── pom.xml
├── README.md
├── scripts/run-integration.ps1
└── src/
    ├── main/java/com/oss/client/
    │   ├── ClientConfig.java      配置与 endpoint 路径前缀
    │   ├── OssClient.java         23 个统一契约方法
    │   ├── PutObjectOptions.java  上传选项
    │   ├── SigV4Signer.java       SigV4 头签名与预签名
    │   ├── HttpTransport.java     HttpURLConnection 传输、重试、超时
    │   ├── BodySource.java        空体/字节/文件/文件区间/流
    │   ├── OssException.java      code/message/status/requestId
    │   ├── OssTimeoutException.java
    │   ├── S3Models.java          XML 解析与元数据校验
    │   └── ...                    模型：BucketInfo/BucketHeadResult/ObjectMetadata/
    │                              ListObjectsResult/ListPartsResult/PutObjectResult/
    │                              CopyObjectResult/DeleteObjectsResult/GetObjectResponse 等
    └── test/java/com/oss/client/
        ├── SigV4SignerTest.java   AWS 官方签名向量
        ├── SdkUnitTest.java       纯逻辑单测
        └── S3IntegrationTest.java 真实服务端集成测试
```
