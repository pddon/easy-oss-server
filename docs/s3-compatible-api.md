# S3 兼容接口协议（冻结版 v1）

本服务在保留原有 OSS 接口的前提下，额外提供一套 **AWS S3 兼容接口**，目标是让主流
S3 客户端（aws cli、boto3、aws-sdk-java、aws-sdk-js、MinIO SDK 等）可以基本无改造地接入。

> 设计原则：**优先保留既有功能，只兼容 S3 客户端接口规范与主要业务 API**，不追求
> 100% 语义一致。凡是与既有能力冲突的地方，一律保留既有行为，并在本文档
> [与真实 S3 的差异](#与真实-s3-的差异) 中显式列出。

---

## 1. 接入信息

| 项 | 值 |
| --- | --- |
| Endpoint | `http://<host>:<port>`（与旧 OSS 接口同端口） |
| Region | 任意（服务端不校验签名 region，建议填 `us-east-1`） |
| AccessKeyId | 应用的 **App ID** |
| SecretAccessKey | 应用的 **App Secret** |
| 寻址方式 | **必须使用 path-style**（`http://host:port/<bucket>/<key>`），不支持 virtual-host-style |
| 签名算法 | AWS Signature Version 4（header 签名与 presigned URL 均支持） |
| 时间窗 | 请求时间与服务器时间相差不超过 ±15 分钟 |
| 预签名有效期 | `X-Amz-Expires` ≤ 604800 秒（7 天） |

客户端需要显式开启 path-style：

- boto3 / botocore：`Config(s3={"addressing_style": "path"})`
- aws cli：`aws s3api ... --endpoint-url http://host:port`（自定义 endpoint 默认走 path-style；
  如客户端强制 virtual-host，请改用 path-style 配置）
- aws-sdk-java v2：`S3Configuration.builder().pathStyleAccessEnabled(true)`
- aws-sdk-js v3：`forcePathStyle: true`

## 2. 鉴权

### 2.1 Header 签名（SigV4）

必带头：

```
Host: <endpoint host[:port]>
x-amz-date: 20260101T000000Z
x-amz-content-sha256: <正文 SHA256 十六进制 或 UNSIGNED-PAYLOAD>
Authorization: AWS4-HMAC-SHA256 Credential=<App ID>/<yyyymmdd>/<region>/s3/aws4_request,
               SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=<hex>
```

规则要点：

- `SignedHeaders` 必须包含 `host`；
- `x-amz-content-sha256` 参与签名，但服务端**不校验正文哈希**（支持流式上传）；
- `x-amz-security-token` 可带，服务端忽略；
- 凭证解析失败 / 签名不匹配 / 时间偏移 / 过期分别返回
  `InvalidAccessKeyId` / `SignatureDoesNotMatch` / `RequestTimeTooSkewed` / `AccessDenied`。

### 2.2 预签名 URL（Presigned）

Query 参数：

```
X-Amz-Algorithm=AWS4-HMAC-SHA256
X-Amz-Credential=<App ID>/<yyyymmdd>/<region>/s3/aws4_request
X-Amz-Date=20260101T000000Z
X-Amz-Expires=300
X-Amz-SignedHeaders=host
X-Amz-Signature=<hex>
```

- 预签名请求的 payload hash 固定为 `UNSIGNED-PAYLOAD`；
- **GET 与 PUT 均支持**（第三方客户端最常用）；DELETE / HEAD 等其他方法同样可用；
- 过期返回 `403 AccessDenied`（`Request has expired`）。

## 3. 已兼容接口

| 方法 | 路径 | 说明 | 备注 |
| --- | --- | --- | --- |
| GET | `/` | ListBuckets | 有 S3 凭证时返回 XML；无凭证仍重定向到管理后台 |
| PUT | `/{bucket}` | CreateBucket | S3 桶名规则在此校验；已存在返回 `409` |
| DELETE | `/{bucket}` | DeleteBucket | 桶内无 active 对象才允许删除 |
| HEAD | `/{bucket}` | HeadBucket | 成功返回 `200` + `x-amz-bucket-region` |
| GET | `/{bucket}?location` | GetBucketLocation | 恒返回空 LocationConstraint（视作 `us-east-1`） |
| POST | `/{bucket}?delete` | DeleteObjects | 支持 `Quiet`；最多 1000 个 Key |
| PUT | `/{bucket}/{key}` | PutObject | 支持 `Content-Type`、`Content-MD5`、`x-amz-meta-*` |
| GET | `/{bucket}/{key}` | GetObject | 支持 `Range`、`If-Match`、`If-None-Match`、meta 响应头 |
| HEAD | `/{bucket}/{key}` | HeadObject | 与 GetObject 相同响应头，无 body |
| DELETE | `/{bucket}/{key}` | DeleteObject | 软删除进回收站；对象不存在也返回 `204` |
| PUT | `/{bucket}/{key}` + `x-amz-copy-source` | CopyObject | 同一 App 内跨桶/同桶复制，支持 metadata COPY/REPLACE |
| GET | `/{bucket}?list-type=2` | ListObjectsV2 | `prefix` / `delimiter` / `max-keys` / `continuation-token` / `start-after` |
| GET | `/{bucket}`（无 `list-type`） | ListObjects（V1） | 支持 `prefix` / `marker` / `delimiter` / `max-keys` |
| POST | `/{bucket}/{key}?uploads` | CreateMultipartUpload | 分片上传初始化 |
| PUT | `/{bucket}/{key}?partNumber=N&uploadId=...` | UploadPart | 返回分片 `ETag`；可选 `Content-MD5` 校验 |
| POST | `/{bucket}/{key}?uploadId=...` | CompleteMultipartUpload | 合并分片并创建对象 |
| DELETE | `/{bucket}/{key}?uploadId=...` | AbortMultipartUpload | 清理分片临时目录 |
| GET | `/{bucket}/{key}?uploadId=...` | ListParts | 返回该 upload 的全部分片 |

每个端点的可直接复制 curl 示例见 [第 9 节](#9-请求示例可直接复制)。

支持的请求头：

- 通用：`Content-Length`、`Content-Type`、`Content-MD5`、`x-amz-date`、
  `x-amz-content-sha256`、`x-amz-security-token`（忽略）
- 读取：`Range`（含 `bytes=start-end` / `bytes=-suffix`）、`If-Match`、`If-None-Match`
- 写入：`x-amz-meta-*`（存入对象旁 sidecar 文件）
- 复制：`x-amz-copy-source`、`x-amz-metadata-directive`（`COPY` / `REPLACE`）

## 4. 错误码

所有 S3 错误响应为 XML：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<Error>
  <Code>NoSuchKey</Code>
  <Message>The specified key does not exist.</Message>
  <Resource>/bucket/key</Resource>
  <RequestId>unknown</RequestId>
</Error>
```

| HTTP | Code | 触发场景 |
| --- | --- | --- |
| 400 | `InvalidBucketName` | S3 CreateBucket 桶名不合规 |
| 400 | `InvalidArgument` | 参数缺失/非法（如 `partNumber`、`max-keys`、`x-amz-copy-source` 格式） |
| 400 | `MalformedXML` | 请求体 XML 非法或未包含有效 Part/Key |
| 400 | `BadDigest` | `Content-MD5` 与实际内容不符（PutObject / UploadPart） |
| 400 | `InvalidPart` / `InvalidPartOrder` | Complete 时分片缺失、ETag 不匹配或顺序错误 |
| 403 | `AccessDenied` | 无权限访问、预签名过期 |
| 403 | `InvalidAccessKeyId` | App ID 不存在 |
| 403 | `SignatureDoesNotMatch` | 签名校验失败 |
| 403 | `RequestTimeTooSkewed` | 请求时间偏移超过 ±15 分钟 |
| 404 | `NoSuchBucket` | 桶不存在 |
| 404 | `NoSuchKey` | 对象不存在 |
| 404 | `NoSuchUpload` | uploadId 不存在或归属不匹配 |
| 409 | `BucketAlreadyExists` / `BucketAlreadyOwnedByYou` | CreateBucket 重名 |
| 409 | `BucketNotEmpty` | DeleteBucket 时桶内仍有 active 对象 |
| 412 | `PreconditionFailed` | `If-Match` 不满足 |
| 413 | `EntityTooLarge` | 超过 `OSS_MAX_UPLOAD_SIZE` |
| 416 | `InvalidRange` | Range 超出对象大小 |
| 501 | `NotImplemented` | 明确未实现的 S3 能力（见差异表） |

## 5. 分页与分片规则

### 5.1 ListObjectsV2 分页

- `max-keys` 默认 `1000`，上限 `1000`（超过按 `1000` 处理，负数/非法值返回 `InvalidArgument`）；
- `continuation-token` 与 `start-after` 同时出现时，token 优先；
- 返回 `KeyCount` = 本次 `Contents` + `CommonPrefixes` 数量；
- 需继续翻页时返回 `NextContinuationToken`（内部就是上一页最后一个原始对象 Key，
  对客户端是不透明字符串）；
- `delimiter`（通常是 `/`）会把同前缀的 Key 折叠到 `CommonPrefixes`，折叠后的条目也计入
  `max-keys`。

### 5.2 分片上传

- 分片临时文件与 manifest 落在 `{OSS_DATA_DIR}/multipart/<uploadId>/`，不新增数据库表；
- 分片大小上限同 `OSS_MAX_UPLOAD_SIZE`；**不强制 5MB 最小分片**；
- `UploadPart` 支持可选的 `Content-MD5`：带该头时校验分片内容 MD5，不一致返回
  `400 BadDigest` 且该分片不写入 upload（可通过 ListParts 确认未被记录）；不带该头时不做校验，
  与之前行为一致。`CompleteMultipartUpload` 不做整对象 MD5 校验；
- `CompleteMultipartUpload` 要求 Part 升序、Part 存在且 ETag 匹配；
- 最终对象 ETag 采用真实 S3 的 `md5-of-md5s-N` 形式（`N` 为分片数）；
- 合并成功后自动删除分片目录；`Abort` 立即删除分片目录；
- 分片目录在启动自检/清理任务中不会被当作孤儿对象文件删除。

## 6. 与真实 S3 的差异

### 6.1 已兼容

- SigV4 header 签名与 presigned URL（GET/PUT 等）；
- 主要的桶/对象 CRUD、复制、批量删除、ListObjectsV1/V2（前缀、分页、delimiter）、
  分片上传全流程；
- `ETag`、`Last-Modified`、`Range`、`If-Match` / `If-None-Match`、`Content-MD5`、
  `x-amz-meta-*` 读写；
- 常见第三方客户端的 path-style 接入（已提供 boto3 冒烟脚本，见第 8 节）。

### 6.2 未兼容 / 有差异

| 能力 | 真实 S3 | 本服务 | 说明 |
| --- | --- | --- | --- |
| 删除语义 | 直接删除 | **软删除进回收站** | `DeleteObject` / `DeleteObjects` 都进回收站，`List` 不可见；同名重传按新对象处理；回收站在管理后台可见/可恢复/可彻底删除 |
| 删除后磁盘 | 立即释放 | 回收站保留到清理策略触发 | 受 `OSS_TRASH_RETENTION_DAYS` 与清理任务控制 |
| DeleteBucket | 要求桶完全为空 | 只要求 **active 对象为空** | 回收站中的对象不计入 `BucketNotEmpty`；桶记录删除后回收站对象仍可恢复 |
| 凭证 | AccessKey/SecretKey + IAM/STS | **App ID / App Secret** | 不新增密钥体系；无 IAM 用户、角色、策略、临时凭证；`x-amz-security-token` 忽略；Secret 轮换即旧密钥立即失效 |
| 权限模型 | ACL / Bucket Policy / 预签名授权 | 仅有 App 归属 + 桶 `public` 标志 | 跨 App 的 Copy/读写一律 `AccessDenied` |
| 寻址 | path-style + virtual-host-style | **仅 path-style** | virtual-host-style 未实现；aws cli 的自定义 endpoint 默认即 path-style |
| 桶名 | 全局唯一、严格 DNS 规则 | S3 规则**只在新桶 CreateBucket 时校验** | 存量桶（如含下划线）不回溯校验，S3 客户端下仍可正常读写 |
| GetBucketLocation | 返回真实 region | 恒返回空（等价 `us-east-1`） | 签名时任意 region 均可通过 |
| 列表 token | 不透明服务端 token | 上一页最后一个 Key | 对外仍是不透明字符串，不应依赖其格式 |
| ListObjectsV2 | 支持 `encoding-type=url`、Owner 信息 | 未实现 `encoding-type`；V2 不返回 Owner | XML 中 Key 始终为原样 UTF-8 |
| 列表上限 | 1000 | 1000（超出部分忽略） | 与 S3 一致 |
| ListParts | 支持 `part-number-marker`/`max-parts` 分页 | 一次性返回全部分片 | `IsTruncated` 恒为 `false` |
| UploadPartCopy | 支持 | **未实现**（501 `NotImplemented`） | 依赖 Range 复制的客户端需改为下载再上传 |
| 分片最小尺寸 | 除最后一片外 ≥ 5MB | 不校验 | 兼容性更宽松 |
| 版本控制 | Versioning / DeleteMarker | 未实现 | 无 `versionId` |
| 对象锁 / 保留 | Object Lock / Legal Hold | 未实现 | — |
| 生命周期 | Lifecycle 规则 | 未实现 | 仅服务端回收站清理策略 |
| 对象标签 | Tagging / Tagging API | 未实现 | `x-amz-tagging` 忽略 |
| 事件通知 | EventBridge / Lambda 通知 | 未实现 | — |
| 静态加密 / KMS | SSE-S3/KMS | 未实现 | — |
| Select / 多段读 | SelectObjectContent | 未实现 | — |
| 批量删除校验 | 可带 Content-MD5 | 接受但未校验 | 超过 1000 个 Key 返回 `MalformedXML` |
| 对象 Key | 任意 UTF-8 | 禁止 `\ : * ? " < > \|`、控制字符、`.`/`..` 路径段、首尾斜杠规则（长度 1–1024） | 目的：避免 Windows/路径穿越问题；普通业务 Key 不受影响 |
| 请求 ID 响应头 | `x-amz-request-id` / `x-amz-id-2` | 未返回 | 错误 XML 中 `RequestId` 固定 `unknown` |
| `x-amz-content-sha256` | 校验正文哈希 | 参与签名，不校验正文 | 支持流式上传 |
| `If-Range` | 支持 | 未实现 | — |
| 条件写 | `If-Match` 写入前置条件 | 仅 Get/Head 生效 | PutObject 不检查条件头 |
| 错误重试语义 | 部分错误可重试 | 返回 S3 错误码，不附加重试提示 | — |
| 时区/日期格式 | RFC7231 GMT | 一致 | `Last-Modified` 使用 GMT |

## 7. 与旧 OSS 接口的对应关系

| 旧 OSS 接口 | S3 接口 | 关系 |
| --- | --- | --- |
| `GET /buckets` | `GET /` | 返回各自格式；`GET /` 有 V4 凭证时走 S3，否则仍重定向管理后台 |
| `POST /buckets` | `PUT /{bucket}` | 同一 Bucket 元数据；S3 额外做桶名校验 |
| `DELETE /buckets/{name}` | `DELETE /{bucket}` | S3 版本要求 active 对象为空 |
| `GET /{bucket}`（JSON 列表） | `GET /{bucket}?list-type=2`（XML） | 同一路由内按凭证/参数分流 |
| `GET /{bucket}/{key}` | `GET /{bucket}/{key}` | header/query 完全相同，S3 额外返回 `x-amz-meta-*` 与条件请求 |
| `PUT /{bucket}/{key}` | `PUT /{bucket}/{key}` | S3 额外支持 `Content-MD5`、meta、CopyObject、UploadPart |
| `DELETE /{bucket}/{key}` | `DELETE /{bucket}/{key}` | 旧接口不存在返回 404；S3 返回 204 |
| 旧签名（Query/Header） | SigV4 | 两套鉴权并存；旧签名请求不会被 S3 分流 |

> 两套路由共享同一份 App / Bucket / Object 元数据与同一套磁盘布局，
> 不存在数据迁移问题。

## 8. 验证情况

- 单元测试：`cargo test`（含 SigV4 官方向量、分片服务、sidecar 生命周期、启动自检孤儿清理）；
- S3 端到端测试：`tests/test_s3_api.rs`、`tests/test_s3_auth.rs`（测试侧独立实现签名）；
- 真实 SDK 冒烟：`scripts/s3_sdk_smoke.py`（boto3，覆盖建桶/对象 CRUD/复制/分页/delimiter/
  批量删除/分片全流程/预签名 GET+PUT）。脚本已随仓库提供，运行方式见脚本头部注释；
- `cargo clippy --all-targets -- -D warnings` 为 0。

> **验证边界说明**：本机未安装 aws cli、MinIO client，且当前环境无法完成 boto3 依赖下载，
> 因此上述 SDK 冒烟脚本尚未在本机执行。当前已验证的是：SigV4 官方签名向量、按真实
> SigV4 规则独立实现的端到端请求（header + presigned）、完整错误路径与状态码。
> 客户端团队可先按本文档开发；拿到可用网络环境后直接运行脚本复核。

## 9. 请求示例（可直接复制）

以下 curl 示例使用 curl 7.75+ 的 `--aws-sigv4` 自动完成 header 签名。
先设置变量（`APP_ID` / `APP_SECRET` 来自管理后台 App 详情）：

```bash
export EP="http://127.0.0.1:8080"        # S3 Endpoint
export AK="<App ID>"
export SK="<App Secret>"
export B="my-bucket"
s3curl() { curl -sS --aws-sigv4 "aws:amz:us-east-1:s3" --user "$AK:$SK" "$@"; }
```

### 9.1 预签名 PUT 与 Content-Type（客户端最易踩坑）

1. **默认签名范围只有 `host`**：presigned URL 生成时 `X-Amz-SignedHeaders=host`，
   此时 `Content-Type` 不参与签名，PUT 请求带什么 Content-Type 就存什么；
   不带则落库为 `application/octet-stream`。
2. **如果客户端把 `content-type` 纳入 SignedHeaders**（部分 SDK 会这样做），
   那么实际 PUT 必须发送**完全相同**的 Content-Type（按 header 值规范化比较），
   否则返回 `403 SignatureDoesNotMatch`。`Content-MD5` 同理。
3. 推荐做法：生成预签名 PUT 时就显式指定 Content-Type，并在上传请求中原样携带：
   ```python
   url = client.generate_presigned_url(
       "put_object",
       Params={"Bucket": "my-bucket", "Key": "a.txt", "ContentType": "text/plain"},
       ExpiresIn=300,
   )
   # 上传时必须带 Content-Type: text/plain
   ```
   若 SDK 没有把 Content-Type 纳入签名，仍然建议显式携带，避免对象被存成二进制类型。
4. 预签名 URL 必须使用 path-style；bucket/key 需要 URL 编码；query 参数顺序参与签名，
   **必须由 SDK 生成整条 URL，不要手工改参数后再使用**。
5. 常见错误：`403 AccessDenied`（过期或 `X-Amz-Expires` 超过 604800）、
   `403 RequestTimeTooSkewed`（本机时间与服务器相差 > 15 分钟）、
   `403 SignatureDoesNotMatch`（SignedHeaders 与实际请求头不一致）。

### 9.2 桶

```bash
# ListBuckets
s3curl "$EP/"

# CreateBucket
s3curl -X PUT "$EP/$B"

# HeadBucket
s3curl -I "$EP/$B"

# GetBucketLocation
s3curl "$EP/$B?location"

# DeleteBucket（桶内 active 对象必须为空）
s3curl -X DELETE "$EP/$B"
```

> 注：CreateBucket 不需要 body；如需携带 `<CreateBucketConfiguration>` 也会被忽略。

### 9.3 对象 CRUD

```bash
# PutObject（带 Content-Type 与自定义元数据）
printf 'hello' > /tmp/hello.txt
s3curl -X PUT \
  -H "Content-Type: text/plain" \
  -H "x-amz-meta-color: blue" \
  --data-binary @/tmp/hello.txt \
  "$EP/$B/hello.txt"

# PutObject（Content-MD5 校验，值为文件 MD5 的 base64）
MD5_B64=$(openssl dgst -md5 -binary /tmp/hello.txt | base64)
s3curl -X PUT -H "Content-MD5: $MD5_B64" \
  --data-binary @/tmp/hello.txt "$EP/$B/hello2.txt"

# GetObject / 下载
s3curl "$EP/$B/hello.txt" -o /tmp/out.txt

# GetObject + Range
s3curl -H "Range: bytes=0-3" "$EP/$B/hello.txt"

# HeadObject
s3curl -I "$EP/$B/hello.txt"

# If-None-Match（ETag 来自 Put/Get 响应头）
s3curl -H 'If-None-Match: "<etag>"' "$EP/$B/hello.txt"

# DeleteObject（204；对象不存在也返回 204，但会进回收站）
s3curl -X DELETE "$EP/$B/hello.txt"

# CopyObject（源与目标必须属于同一个 App）
s3curl -X PUT -H "x-amz-copy-source: /$B/hello.txt" "$EP/$B/copy.txt"

# CopyObject + REPLACE 元数据
s3curl -X PUT \
  -H "x-amz-copy-source: /$B/hello.txt" \
  -H "x-amz-metadata-directive: REPLACE" \
  -H "Content-Type: text/markdown" \
  -H "x-amz-meta-owner: docs" \
  "$EP/$B/copy2.txt"
```

### 9.4 列表

```bash
# ListObjectsV2：前缀
s3curl "$EP/$B?list-type=2&prefix=photos/"

# ListObjectsV2：每页 2 条（下一页用返回的 NextContinuationToken）
s3curl "$EP/$B?list-type=2&max-keys=2"
s3curl "$EP/$B?list-type=2&max-keys=2&continuation-token=<NextContinuationToken>"

# ListObjectsV2：delimiter 折叠目录
s3curl "$EP/$B?list-type=2&delimiter=/"

# ListObjectsV2：从某个 key 之后开始
s3curl "$EP/$B?list-type=2&start-after=photos/001.jpg"

# ListObjects（V1，marker 分页）
s3curl "$EP/$B?prefix=photos/&marker=photos/001.jpg&max-keys=100"
```

### 9.5 批量删除

```bash
s3curl -X POST \
  -H "Content-Type: application/xml" \
  --data-binary '<Delete><Object><Key>a.txt</Key></Object><Object><Key>b.txt</Key></Object></Delete>' \
  "$EP/$B?delete"
```

### 9.6 分片上传全流程

```bash
# 1) 初始化，返回 XML 中的 <UploadId>
UPLOAD_ID=$(s3curl -X POST "$EP/$B/big.bin?uploads" \
  | sed -n 's:.*<UploadId>\(.*\)</UploadId>.*:\1:p')

# 2) 上传分片（ETag 在响应头，后续 Complete 需要）
#    可选：带 Content-MD5 让服务端校验分片内容，不一致返回 400 BadDigest
MD5_1=$(openssl dgst -md5 -binary part1.bin | base64)
ETAG1=$(s3curl -X PUT --data-binary @part1.bin \
  -H "Content-MD5: $MD5_1" \
  "$EP/$B/big.bin?partNumber=1&uploadId=$UPLOAD_ID" -D - -o /dev/null \
  | tr -d '\r' | awk 'tolower($1)=="etag:"{print $2}')
ETAG2=$(s3curl -X PUT --data-binary @part2.bin \
  "$EP/$B/big.bin?partNumber=2&uploadId=$UPLOAD_ID" -D - -o /dev/null \
  | tr -d '\r' | awk 'tolower($1)=="etag:"{print $2}')

# 3) 查看已上传分片
s3curl "$EP/$B/big.bin?uploadId=$UPLOAD_ID"

# 4) 完成合并（Part 必须升序，ETag 必须与上传时一致）
s3curl -X POST -H "Content-Type: application/xml" \
  --data-binary "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>$ETAG1</ETag></Part><Part><PartNumber>2</PartNumber><ETag>$ETAG2</ETag></Part></CompleteMultipartUpload>" \
  "$EP/$B/big.bin?uploadId=$UPLOAD_ID"

# 5) 放弃上传（清理分片目录）
s3curl -X DELETE "$EP/$B/big.bin?uploadId=$UPLOAD_ID"
```

### 9.7 预签名 URL

预签名 URL 由客户端 SDK 生成（签名对 query 顺序敏感，不要手写）。生成后可直接用普通 curl：

```bash
# GET
curl -sS "<generate_presigned_url 的结果>"

# PUT（无签名头，Content-Type 可选；见 9.1）
curl -sS -X PUT -H "Content-Type: text/plain" \
  --data-binary @/tmp/hello.txt "<generate_presigned_url 的结果>"
```

boto3 生成示例：

```python
import boto3
from botocore.client import Config

client = boto3.client(
    "s3",
    endpoint_url="http://127.0.0.1:8080",
    aws_access_key_id="<App ID>",
    aws_secret_access_key="<App Secret>",
    region_name="us-east-1",
    config=Config(s3={"addressing_style": "path"}),
)

get_url = client.generate_presigned_url(
    "get_object", Params={"Bucket": "my-bucket", "Key": "hello.txt"}, ExpiresIn=300
)
put_url = client.generate_presigned_url(
    "put_object",
    Params={"Bucket": "my-bucket", "Key": "upload.txt", "ContentType": "text/plain"},
    ExpiresIn=300,
)
```
