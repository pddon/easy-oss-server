# oss-s3-client

OSS Server 官方 JavaScript / TypeScript 客户端。零运行时依赖，仅使用 Node 内置模块
（`crypto` / `http` / `https` / `fs` / `stream`）。

**跨语言统一契约**：Rust / Java / Python / JavaScript 四门客户端的方法名、参数顺序、
参数含义与返回值结构完全一致。JS 侧以 camelCase 映射（Rust 为 snake_case）：

| 统一契约 | JS 写法 |
| --- | --- |
| `create_bucket` | `createBucket` |
| `list_objects_v2` | `listObjectsV2` |
| `presign_get` | `presignGet` |
| … | … |

## 环境要求

- Node.js >= 14.17（Node 14 与 18+ 已验证；内部使用内置 `http`/`https`，不依赖全局 `fetch`）
- 服务端需启用 S3 兼容接口；客户端使用 **path-style** 访问
- `endpoint` 允许带路径前缀，例如 `http://host:8080/api`

## 安装

```bash
npm install oss-s3-client
```

本地开发安装：

```bash
npm install file:E:/ai_code_proj/oss-server/clients/js
```

## 快速开始（五行核心调用）

```js
const { OSSClient } = require('oss-s3-client');
const client = new OSSClient({ endpoint: 'http://127.0.0.1:8080', accessKey: 'APP_ID', secretKey: 'APP_SECRET' });
await client.putObject('my-bucket', 'hello.txt', Buffer.from('hello oss'));
const body = await client.getObject('my-bucket', 'hello.txt');
const url = client.presignGet('my-bucket', 'hello.txt', 600);
```

## 配置项

| 选项 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `endpoint` | 是 | - | 服务端地址，允许路径前缀，如 `http://host:8080/api` |
| `accessKey` | 是 | - | App ID |
| `secretKey` | 是 | - | App Secret |
| `region` | 否 | `us-east-1` | 任意值，仅参与签名 |
| `bucket` | 否 | `''` | 默认桶；方法未传 `bucket`（`undefined` / `null`）时使用；未配置且未传则报错 |
| `timeout` | 否 | `30000` | 单次请求超时（毫秒） |
| `retries` | 否 | `2` | 失败重试次数（仅可重放请求） |
| `retryDelay` | 否 | `100` | 重试基础间隔（毫秒，指数退避） |
| `partSize` | 否 | `8MB` | 分片大小，同时作为自动分片阈值 |
| `sessionToken` | 否 | `''` | 透传 `x-amz-security-token` |

## CommonJS 与 ESM

```js
// CommonJS
const { OSSClient, OSSError, isOSSError } = require('oss-s3-client');

// ESM
import { OSSClient, OSSError, isOSSError } from 'oss-s3-client';

const client = new OSSClient({
  endpoint: 'http://127.0.0.1:8080',
  accessKey: process.env.OSS_ACCESS_KEY,
  secretKey: process.env.OSS_SECRET_KEY,
});
```

TypeScript 直接使用包内类型声明（`types/index.d.ts`）：

```ts
import { OSSClient, type PutObjectOptions } from 'oss-s3-client';

const client = new OSSClient({
  endpoint: 'http://127.0.0.1:8080',
  accessKey: 'APP_ID',
  secretKey: 'APP_SECRET',
});

const options: PutObjectOptions = { contentType: 'text/plain', metadata: { owner: 'ts' } };
await client.putObject('demo', 'a.txt', Buffer.from('hi'), options);
```

## 桶（6 个方法）

```js
await client.createBucket('demo');            // void
await client.deleteBucket('demo');            // void；非空桶抛 409 BucketNotEmpty
await client.listBuckets();                   // [{ name, createdAt }]
await client.headBucket('demo');              // { exists, region }
await client.bucketExists('demo');            // true / false
await client.getBucketLocation('demo');       // 'us-east-1'（空值归一化）
```

`headBucket` 的 `region` 优先读取响应头 `x-amz-bucket-region`，缺失时才回退本地配置。

## 对象（9 个方法）

```js
// 1) putObject(bucket, key, body, options)
await client.putObject('demo', 'docs/a.txt', Buffer.from('hello'), {
  contentType: 'text/plain; charset=utf-8',
  metadata: { owner: 'js-sdk' },
  contentMd5: true,        // 也可直接传 base64 字符串
  partSize: 8 * 1024 * 1024,
  autoMultipart: true,     // 默认 true：超过 partSize 自动分片
});
// -> { etag, size }

// 2) uploadFile(bucket, key, path, options)：按文件路径上传，同名同义
await client.uploadFile('demo', 'docs/b.bin', 'D:/data/b.bin', {
  contentType: 'application/octet-stream',
  contentMd5: true,
});

// 3) getObject(bucket, key, range) -> Buffer
const body = await client.getObject('demo', 'docs/a.txt');
await client.getObject('demo', 'docs/a.txt', { start: 0, end: 4 });
await client.getObject('demo', 'docs/a.txt', 'bytes=0-99');
await client.getObject('demo', 'docs/a.txt', { suffix: 10 });

// JS 扩展：需要流式下载时传第 4 个参数 options.stream=true
const stream = await client.getObject('demo', 'docs/a.txt', undefined, { stream: true });

// 4) getObjectToFile(bucket, key, path, range)
await client.getObjectToFile('demo', 'docs/b.bin', 'D:/tmp/b.bin');
await client.getObjectToFile('demo', 'docs/a.txt', 'D:/tmp/part.txt', 'bytes=0-99');

// 5) headObject(bucket, key)
await client.headObject('demo', 'docs/a.txt');
// -> { size, etag, contentType, metadata, lastModified }

// 6) objectExists(bucket, key) -> boolean（内部 headObject，404 视为不存在）
await client.objectExists('demo', 'docs/a.txt');

// 7) deleteObject(bucket, key) -> void
await client.deleteObject('demo', 'docs/a.txt');

// 8) deleteObjects(bucket, keys, quiet=false) -> { deleted, errors }
await client.deleteObjects('demo', ['docs/a.txt', 'docs/b.bin'], false);
await client.deleteObjects('demo', ['k1', 'k2'], true); // quiet=true 时服务端不返回 Deleted 列表
await client.deleteObjects('demo', []);                 // 空 keys：返回 { deleted: [], errors: [] }，不发请求
// 超过 1000 个 Key 自动分批

// 9) copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata?, metadataDirective?)
await client.copyObject('demo', 'docs/a.txt', 'demo', 'docs/a-copy.txt');
await client.copyObject('demo', 'docs/a.txt', 'demo', 'docs/a-new.txt', { owner: 'copy' }); // 自动 REPLACE
await client.copyObject('demo', 'docs/a.txt', 'demo', 'docs/a-new.txt', { owner: 'x' }, 'COPY'); // 显式指定
```

`copyObject` 的 directive 推导规则：未显式指定时，**只有传了非空 `metadata` 对象才用 `REPLACE`**；
未传 `metadata` 或传空对象 `{}` 一律用 `COPY`（保留源对象元数据，不会被空对象误清空）。

## 列表（1 个方法 + JS 迭代器扩展）

```js
const page = await client.listObjectsV2(
  'demo',          // bucket
  'docs/',         // prefix
  '/',             // delimiter
  100,             // maxKeys
  undefined,       // continuationToken
  undefined        // startAfter
);
// -> { items, commonPrefixes, isTruncated, nextToken }

// 按页翻页
let token;
do {
  const page = await client.listObjectsV2('demo', 'docs/', undefined, 100, token);
  token = page.isTruncated ? page.nextToken : undefined;
} while (token);

// JS 便捷扩展：自动翻页的异步迭代器
for await (const item of client.listAllObjectsV2('demo', 'docs/')) {
  console.log(item.key, item.size);
}
```

## 分片上传（5 个方法）

```js
// createMultipartUpload(bucket, key, contentType, metadata) -> uploadId
const uploadId = await client.createMultipartUpload('demo', 'big.bin', 'application/octet-stream', {
  owner: 'multipart',
});

// uploadPart(bucket, key, uploadId, partNumber, body) -> etag
const etag1 = await client.uploadPart('demo', 'big.bin', uploadId, 1, part1);
const etag2 = await client.uploadPart('demo', 'big.bin', uploadId, 2, part2);

// listParts(bucket, key, uploadId) -> { parts, isTruncated }
const listed = await client.listParts('demo', 'big.bin', uploadId);

// completeMultipartUpload(bucket, key, uploadId, parts) -> etag
const etag = await client.completeMultipartUpload('demo', 'big.bin', uploadId, [
  { partNumber: 1, etag: etag1 },
  { partNumber: 2, etag: etag2 },
]);

// abortMultipartUpload(bucket, key, uploadId) -> void
await client.abortMultipartUpload('demo', 'big.bin', uploadId);
```

高层自动分片已内置在 `putObject`（仅 Buffer 等字节数据）/ `uploadFile`（文件路径）：
超过 `partSize` 自动 create → uploadPart → complete，任一步失败自动 abort。
流式输入（Readable）不会自动分片，一律单次 PUT。也可以显式控制：

```js
await client.putObject('demo', 'big.bin', bigBuffer, { partSize: 1 * 1024 * 1024, autoMultipart: true });
await client.uploadFile('demo', 'big.bin', 'D:/data/big.bin', { partSize: 8 * 1024 * 1024 });
```

## 预签名 URL（2 个方法）

```js
const getUrl = client.presignGet('demo', 'docs/a.txt', 600); // 临时下载链接
const putUrl = client.presignPut('demo', 'uploads/new.bin', 600); // 临时上传链接
```

`expiresIn` 为秒，范围 1..604800（7 天），默认 900。URL 使用 `UNSIGNED-PAYLOAD`，
可直接交给浏览器或 curl：

```bash
curl -T D:/data/new.bin "http://127.0.0.1:8080/demo/uploads/new.bin?X-Amz-Algorithm=..."
```

## 跨语言统一语义

- **默认桶**：`bucket` 传 `undefined` 或 `null` 时使用配置里的 `bucket`；两者都没有时
  在本地抛错并说明原因（`pass a bucket name or configure the default bucket via options.bucket`）。
- **endpoint 路径前缀**：`http://host:8080/api` 时，请求路径、签名 canonical URI、
  预签名 URL 都自动带上 `/api`。
- **元数据校验**：key 必须是 RFC 7230 token 字符集（ASCII）；value 只允许可打印 ASCII
  `0x20..0x7E`，不允许 Tab、CR、LF、NUL 等控制字符；非法时在本地直接抛 `TypeError`，
  不发出请求。
- **重试策略**：字节、文件路径（重试时重新打开文件）、可重放请求才重试；
  调用方传入的一次性 Readable 一律不重试（只发一次）。
- **流式输入**：未知长度的流一律走**单次 PUT**（chunked，不尝试分片）；已知长度但
  流本身不可重放，同样只走单次 PUT。只有可重放数据（Buffer、文件路径）才自动分片。
- **Content-MD5（三种场景）**：
  1. **单次 PUT**（未超过 `partSize` 或 `autoMultipart: false`）：指定 `contentMd5` 时，
     把**整对象** MD5 放入 `Content-MD5` 头，由服务端校验。
  2. **自动分片**：不发送整对象 MD5；**逐片计算该片 MD5**，作为每个 UploadPart 的
     `Content-MD5` 头发送，服务端逐片校验，错误时返回 400 `BadDigest`。
  3. **本地预校验**：`contentMd5` 传入具体 base64 值时，数据可重放（Buffer、文件路径、
     可重放流）会先本地计算整对象 MD5 比对；**单次 PUT 与自动分片都一样**，不一致直接
     抛 `BadDigest` 且不发出请求。不可重放的一次性 Readable 跳过本地校验，交由服务端校验。
  `contentMd5: true` 表示自动计算；传 base64 字符串表示期望值。
- **分片排序**：`completeMultipartUpload` 提交前一律按 `partNumber` 升序排序，调用方
  传入顺序不影响结果。
- **预签名**：`presignGet` / `presignPut` 的 `expiresIn` 可选，默认 900 秒。
- **错误对象**：统一字段 `code`、`message`、`status`、`requestId`（另保留
  `resource`、`statusCode`、`retryable` 供排查）。

## 错误处理

```js
const { OSSError, OSSTimeoutError, OSSNetworkError, isOSSError } = require('oss-s3-client');

try {
  await client.getObject('demo', 'missing.txt');
} catch (err) {
  if (isOSSError(err)) {
    console.error(err.code, err.message, err.status, err.requestId);
  }
  if (err instanceof OSSTimeoutError) {
    console.error('请求超时');
  }
}
```

- 404 对象 → `NoSuchKey`；404 桶 → `NoSuchBucket`
- 403 签名错误 → `SignatureDoesNotMatch`；权限不足 → `AccessDenied`
- 409 → `BucketNotEmpty` / `Conflict`
- 超时 → `OSSTimeoutError`；网络中断 → `OSSNetworkError`
- 5xx / 429 / 网络错误 / 超时会按 `retries` 指数退避重试，且每次重试重新签名

## 构建与测试

```bash
npm run build        # 零依赖语法/文件检查
npm run test:unit    # SigV4 官方向量 + 本地 stub 请求层
npm run test:integration
npm test             # 单元 + 集成
```

- 单元测试使用 AWS 官方 SigV4 测试向量（header 签名与预签名 URL），不是自签自验。
- 集成测试自行拉起真实服务端：校验 `target/release` / `target/debug` 二进制是否比源码新，
  过期则用 `cargo build --release` 构建到 `%TEMP%\oss-server-build-js`（独立
  `CARGO_TARGET_DIR`，避免与主构建抢锁）。
- 固定监听 `127.0.0.1:18102`，数据目录 `%TEMP%\oss-server-js-data`；通过管理 API
  （`x-admin-token`）创建 App 获取 accessKey/secretKey；结束后杀进程并确认端口释放。

## 已知限制

- 仅支持 path-style，不支持 virtual-hosted-style（`bucket.host`）。
- 仅实现 SigV4（header 签名 + 预签名 URL），不支持 OSS V1 签名、STS 临时凭证、桶 ACL/策略。
- 流式 PUT 一律单次发送：已知长度时带 `Content-Length`，未知长度时以 chunked 传输
  （客户端统计实际字节数作为返回的 `size`）；流式输入不会自动分片，需要分片请用
  Buffer 或 `uploadFile` 文件路径。
- `contentMd5: true` 无法用于流式输入（发送前无法预计算整对象 MD5）；可改为传具体
  base64 期望值交由服务端校验。
- 分片上限沿用服务端能力；未实现 UploadPartCopy、版本控制、生命周期、对象锁等扩展接口。
- 预签名 PUT 不在 URL 中签名 `content-type` 等请求头，上传时可自由设置这些头。
- 服务端对未携带任何鉴权信息的请求返回 JSON 错误（与 S3 XML 错误格式不同），
  但状态码语义一致。
