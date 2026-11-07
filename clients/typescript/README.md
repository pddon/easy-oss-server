# com.pddon.oss-client-ts — TypeScript 客户端 SDK

Rust 版 OSS Server 的官方 TypeScript / JavaScript 客户端，基于 S3 兼容协议（SigV4、path-style URL）。
与 Rust / Java / JavaScript / Python 客户端保持统一的 23 个方法契约，参数顺序一致。

- 严格模式 TypeScript（`strict: true`），公共 API 与内部实现分离
- 同时产出 CommonJS、ESM 与完整 `.d.ts`
- 零运行时依赖：签名使用 Node 内置 `crypto`，HTTP 使用内置 `http` / `https`
- 支持流式上传 / 下载、自动分片、批量删除分批、预签名 URL

要求 Node.js >= 14.17。Node 18+ 也可直接使用。

---

## 安装

发布后从 npm 安装：

```bash
npm install com.pddon.oss-client-ts
```

本地开发或从源码使用：

```bash
cd clients/typescript
npm install
npm run build
```

包入口：

| 入口 | 文件 |
| --- | --- |
| CommonJS `require` | `dist/cjs/index.js` |
| ESM `import` | `dist/esm/index.js` |
| 类型声明 | `dist/types/index.d.ts` |

---

## 快速开始

```ts
import { OSSClient } from 'com.pddon.oss-client-ts';

const client = new OSSClient({
  endpoint: 'http://127.0.0.1:8080',
  accessKey: 'your-app-id',
  secretKey: 'your-app-secret',
  region: 'us-east-1',      // 任意值，只参与 SigV4 签名
  bucket: 'my-bucket',      // 可选默认桶
});

await client.createBucket('my-bucket');

const result = await client.putObject('my-bucket', 'hello.txt', Buffer.from('hello'), {
  contentType: 'text/plain',
});

console.log(result.etag, result.size);
```

CommonJS：

```js
const { OSSClient } = require('com.pddon.oss-client-ts');
```

---

## 配置项（ClientOptions）

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `endpoint` | `string` | 必填 | 服务端地址；允许路径前缀，如 `http://host:8080/api` |
| `accessKey` | `string` | 必填 | App ID |
| `secretKey` | `string` | 必填 | App Secret |
| `region` | `string` | `us-east-1` | 仅参与签名，服务端不校验取值 |
| `bucket` | `string` | `''` | 默认桶：方法传 `undefined` / `null` 时使用 |
| `timeout` | `number` | `30000` | 单次请求超时（毫秒） |
| `retries` | `number` | `2` | 可重放请求的重试次数 |
| `retryDelay` | `number` | `100` | 指数退避基础间隔（毫秒） |
| `partSize` | `number` | `8MB` | 分片大小，同时作为自动分片阈值 |
| `sessionToken` | `string` | `''` | 透传 `x-amz-security-token`（服务端忽略） |

`endpoint` 带路径前缀时，请求路径、SigV4 canonical URI、预签名 URL 三处都会包含该前缀。

所有网络方法都可选地接收末尾的 `{ timeout?, retries? }` 覆盖项。

---

## API

### 桶操作

#### `createBucket(bucket): Promise<void>`

```ts
await client.createBucket('new-bucket');
```

#### `deleteBucket(bucket): Promise<void>`

```ts
await client.deleteBucket('new-bucket');
```

#### `listBuckets(): Promise<BucketListItem[]>`

每项包含 `name`、`createdAt`。

```ts
const buckets = await client.listBuckets();
for (const item of buckets) {
  console.log(item.name, item.createdAt);
}
```

#### `headBucket(bucket): Promise<{ exists: boolean; region: string }>`

`region` 优先取响应头 `x-amz-bucket-region`，缺失时回退配置的 `region`。

```ts
const info = await client.headBucket('my-bucket');
if (!info.exists) {
  console.log('bucket 不存在');
}
console.log(info.region);
```

#### `bucketExists(bucket): Promise<boolean>`

```ts
if (await client.bucketExists('my-bucket')) {
  // ...
}
```

#### `getBucketLocation(bucket): Promise<string>`

空值归一化为 `"us-east-1"`。

```ts
const location = await client.getBucketLocation('my-bucket'); // "us-east-1"
```

### 对象上传

#### `putObject(bucket, key, body, options?): Promise<{ etag, size }>`

`body` 支持 `Buffer | Uint8Array | string | Readable`。
选项：`contentType`、`metadata`、`contentMd5`、`partSize`、`autoMultipart`（默认 `true`）、`contentLength`、`timeout`、`retries`。

超过 `partSize` 的 Buffer / 文件会自动分片；`Readable` 一律走单次 PUT（不整体读入内存）。

```ts
const result = await client.putObject('my-bucket', 'docs/a.txt', Buffer.from('content'), {
  contentType: 'text/plain; charset=utf-8',
  metadata: { owner: 'team-a' },
  contentMd5: true,
});
console.log(result.etag, result.size);
```

#### `uploadFile(bucket, key, path, options?): Promise<{ etag, size }>`

本地文件上传，大文件按 `partSize` 流式分片，不会整体读入内存。

```ts
const uploaded = await client.uploadFile('my-bucket', 'backup/data.bin', 'D:/data.bin', {
  partSize: 8 * 1024 * 1024,
  contentMd5: true,
});
```

### 对象下载

#### `getObject(bucket, key, range?): Promise<Buffer>`

默认返回 `Buffer`；传入 `options.stream = true` 时返回 `Readable`。类型声明用**两个重载**表达（不是 `Promise<Buffer | Readable>` 联合类型），所以不传 `stream` 时可以直接当 `Buffer` 用。
`range` 支持 `'bytes=0-99'`、`{ start, end }`、`{ suffix }`。

```ts
import * as fs from 'fs';

const buffer = await client.getObject('my-bucket', 'docs/a.txt'); // 推断为 Buffer
console.log(buffer.toString('utf8'));

const part = await client.getObject('my-bucket', 'docs/a.txt', { start: 0, end: 9 }); // 仍是 Buffer

const stream = await client.getObject('my-bucket', 'big.bin', undefined, { stream: true }); // 推断为 Readable
stream.pipe(fs.createWriteStream('big.bin'));
```

如果同一段代码里两种返回都可能出现（比如用一个布尔参数决定是否流式），TypeScript 会把结果推断为 `Buffer | Readable`，此时直接当 `Buffer` 用会编译失败，需要先 `Buffer.isBuffer()` 收窄：

```ts
import * as fs from 'fs';

async function saveOrReturn(bucket: string, key: string, asStream: boolean) {
  const result = asStream
    ? await client.getObject(bucket, key, undefined, { stream: true }) // Readable
    : await client.getObject(bucket, key); // Buffer
  if (Buffer.isBuffer(result)) {
    return result; // 这里收窄为 Buffer
  }
  return result.pipe(fs.createWriteStream('stream.bin')); // 这里收窄为 Readable
}
```

#### `getObjectToFile(bucket, key, path, range?, options?): Promise<void>`

流式落盘，不整体读入内存。

```ts
await client.getObjectToFile('my-bucket', 'big.bin', 'D:/download/big.bin');
await client.getObjectToFile('my-bucket', 'docs/a.txt', 'D:/download/a.txt', { suffix: 100 });
```

#### `headObject(bucket, key): Promise<ObjectHead>`

返回 `{ size, etag, contentType, metadata, lastModified? }`。

```ts
const head = await client.headObject('my-bucket', 'docs/a.txt');
console.log(head.size, head.etag, head.metadata);
```

#### `objectExists(bucket, key): Promise<boolean>`

```ts
if (!(await client.objectExists('my-bucket', 'docs/a.txt'))) {
  // ...
}
```

### 删除与复制

#### `deleteObject(bucket, key): Promise<void>`

软删除进回收站；对象不存在也视为成功。

```ts
await client.deleteObject('my-bucket', 'docs/a.txt');
```

#### `deleteObjects(bucket, keys, quiet?): Promise<{ deleted, errors }>`

超过 1000 个 key 自动分批并合并结果；`quiet` 默认 `false`；空 `keys` 返回空结果且不发请求。

```ts
const result = await client.deleteObjects('my-bucket', ['a.txt', 'b.txt']);
console.log(result.deleted, result.errors);

const silent = await client.deleteObjects('my-bucket', ['c.txt'], true);
```

#### `copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata?, metadataDirective?): Promise<{ etag }>`

未指定 directive 时：`metadata` 为非空对象用 `REPLACE`，未传或空对象用 `COPY`。

```ts
const copied = await client.copyObject('my-bucket', 'a.txt', 'my-bucket', 'backup/a.txt');

// 覆盖元数据
await client.copyObject('src', 'a.txt', 'dst', 'a.txt', { owner: 'team-b' }, 'REPLACE');
```

### 列表

#### `listObjectsV2(bucket, prefix?, delimiter?, maxKeys?, continuationToken?, startAfter?): Promise<ListObjectsV2Result>`

返回 `{ items, commonPrefixes, isTruncated, nextToken? }`。

```ts
const page1 = await client.listObjectsV2('my-bucket', 'docs/', '/', 100);
console.log(page1.items, page1.commonPrefixes);

let token = page1.nextToken;
while (token) {
  const page = await client.listObjectsV2('my-bucket', 'docs/', '/', 100, token);
  token = page.nextToken;
}
```

### 分片上传

#### `createMultipartUpload(bucket, key, contentType?, metadata?): Promise<string>`

返回 `uploadId`。

```ts
const uploadId = await client.createMultipartUpload('my-bucket', 'big.bin', 'application/octet-stream');
```

#### `uploadPart(bucket, key, uploadId, partNumber, body): Promise<string>`

返回该片 ETag（已去掉引号），即分片 MD5 的十六进制。

```ts
const etag1 = await client.uploadPart('my-bucket', 'big.bin', uploadId, 1, part1);
const etag2 = await client.uploadPart('my-bucket', 'big.bin', uploadId, 2, part2);
```

#### `completeMultipartUpload(bucket, key, uploadId, parts): Promise<string>`

`parts` 为 `{ partNumber, etag }[]`；提交前客户端会按 `partNumber` 升序排序。

```ts
const etag = await client.completeMultipartUpload('my-bucket', 'big.bin', uploadId, [
  { partNumber: 2, etag: etag2 },
  { partNumber: 1, etag: etag1 },
]);
```

#### `abortMultipartUpload(bucket, key, uploadId): Promise<void>`

```ts
await client.abortMultipartUpload('my-bucket', 'big.bin', uploadId);
```

#### `listParts(bucket, key, uploadId): Promise<{ parts, isTruncated }>`

```ts
const listed = await client.listParts('my-bucket', 'big.bin', uploadId);
console.log(listed.parts);
```

### 预签名 URL

#### `presignGet(bucket, key, expiresIn?): string`

`expiresIn` 可选，默认 `900` 秒，范围 1..604800。

```ts
const getUrl = client.presignGet('my-bucket', 'docs/a.txt', 600);
```

#### `presignPut(bucket, key, expiresIn?): string`

```ts
const putUrl = client.presignPut('my-bucket', 'upload/new.txt', 600);
// 任意 HTTP 客户端直接 PUT，不需要 Authorization 头
```

### 其他

```ts
client.close(); // 关闭 keep-alive 连接池
```

---

## 错误处理

失败统一抛出 `OSSError`，字段与四语言契约一致：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `code` | `string` | 如 `NoSuchKey`、`NoSuchBucket`、`BadDigest`、`AccessDenied`、`NetworkError` |
| `message` | `string` | 服务端错误消息或网络错误描述 |
| `status` | `number` | HTTP 状态码；网络错误为 0 |
| `requestId` | `string` | 服务端 `RequestId`；网络错误为空字符串 |

另外提供 `statusCode`、`resource`、`retryable`，以及 `isOSSError(value)` 类型守卫。

```ts
import { OSSClient, OSSError, isOSSError } from 'com.pddon.oss-client-ts';

try {
  await client.getObject('my-bucket', 'missing.txt');
} catch (err) {
  if (isOSSError(err)) {
    console.error(err.code, err.status, err.requestId, err.message);
    if (err.status === 404) {
      // 对象不存在
    }
  }
}
```

重试规则：

- 仅可重放请求体（`Buffer` / `string` / 本地文件）会重试；调用方传入的 `Readable` 只发一次
- `429`、`5xx`、网络错误按指数退避重试
- `403`、`404` 等 4xx 不重试

---

## 关键语义

### 元数据

- 元数据 key 去掉可选前缀后必须是 RFC 7230 token
- value 只允许可打印 ASCII（`0x20..0x7E`）
- 非法时本地抛出 `TypeError`，不会发出请求

### Content-MD5

- `contentMd5: true`：单次 PUT 发送整对象 MD5；自动分片时逐片计算并发送该片 MD5，不发整对象 MD5
- `contentMd5: '<base64>'`：数据可重放时（Buffer / 文件）先本地校验整对象 MD5，不一致直接抛 `BadDigest`、不发请求；流式不可重放数据由服务端校验并返回 `400 BadDigest`

### 自动分片

- `putObject` / `uploadFile` 默认 `autoMultipart: true`，超过 `partSize` 自动 create → uploadPart → complete
- 任意一步失败会自动 abort，避免残留临时分片
- 分片按文件流逐片读取，不整体读入内存
- 未知长度的 `Readable` 不做分片，走单次 PUT（chunked 传输）

### 默认桶

`bucket` 传 `undefined` 或 `null` 时使用 `options.bucket`；两者都没有则抛出 `TypeError` 并给出明确说明。

### 预签名

- SigV4 预签名，payload hash 固定 `UNSIGNED-PAYLOAD`，只签 `host`
- 过期、签名被篡改均返回 `403`
- `endpoint` 带路径前缀时 URL 中同样包含前缀

---

## 构建与测试

```bash
npm run typecheck         # tsc --noEmit，strict 全量类型检查
npm run build             # dist/cjs + dist/esm + dist/types
npm run test:unit         # 签名官方向量 + 本地校验/重试/解析单测
npm run test:integration  # 真实服务端集成测试
npm test                  # build + 单测 + 集成测试
```

### 单元测试

- 使用 AWS 官方 SigV4 测试向量（GET、PUT、预签名 URL）验证签名，不自签自验
- 覆盖 URI 编码、XML 解析/构造、元数据校验、默认桶、MD5 本地预校验、Range、复制 directive 默认值、分片排序、分页解析、重试策略

### 集成测试

集成测试会自行启动真实服务端：

- 固定端口 `127.0.0.1:18104`
- 数据目录 `%TEMP%\oss-server-ts-data`
- 优先使用 `target/release/oss-server.exe`；当它比 `src/` 源码旧时，使用独立 `CARGO_TARGET_DIR=%TEMP%\oss-server-build-ts` 重新构建（避免与其他客户端构建抢锁）
- 启动时设置 `OSS_HOST` / `OSS_DATA_DIR` / `OSS_ADMIN_TOKEN` / `OSS_MASTER_KEY` / `OSS_STORAGE_BACKEND=memory` / `OSS_CLEANUP_INTERVAL_MINUTES=0`
- 通过管理 API `POST /api/admin/apps` 创建 App 获取 `accessKey` / `secretKey`
- 跑完自动停止进程并确认端口释放

覆盖链路：建桶、headBucket（region 来自响应头）、location 归一化、单次/文件/自动分片上传、下载与 Range、head、COPY / REPLACE、批量删除（含 1005 个 key 自动分批）、列表分页与 delimiter、预签名 GET/PUT 与过期 403、分片全流程与 abort、Content-MD5 正确与错误 400 BadDigest、404/403 错误字段。

测试 runner 为自带零依赖实现（Node 14 无 `node:test`），不依赖 jest / vitest。

---

## 限制

- 仅实现本服务端已兼容的 S3 子集；未兼容项（ACL、版本控制、对象锁、事件通知等）不提供封装
- 预签名仅提供 GET / PUT，不提供 POST Policy
- `Readable` 上传不自动分片、不重试；如需大文件分片请使用 `Buffer`、`uploadFile` 或低层分片 API
- `listParts` 依赖服务端一次性返回全部分片，未实现分片列表分页
- 预签名 URL 的 `expiresIn` 上限为 7 天（604800 秒）
- 元数据值限制为可打印 ASCII，不支持中文等非 ASCII 元数据值
- 对象 key 长度上限 1024 字节，不允许前导 `/` 以及 `\ : * ? " < > |` 等字符
- `getObjectToFile` 失败时可能留下部分写入的本地文件，需要调用方自行清理
- `putObject` 的 `Buffer` 路径会在内存中持有完整对象；超大文件请用 `uploadFile` 或流式上传
