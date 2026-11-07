# oss-s3-client（Python）

Rust 版 OSS 服务端（S3 兼容协议）的官方 Python 客户端。零第三方运行时依赖，仅使用
`http.client`、`urllib`、`hmac`/`hashlib` 与 `xml.etree` 等标准库。

- Python 3.8+
- SigV4 header 签名 + 预签名 URL（GET/PUT）
- path-style 访问：`http://endpoint/bucket/key`
- 桶、对象、列表分页、复制、批量删除、分片上传与流式下载

> **跨语言统一契约**：本客户端的方法名在 Python 侧为 snake_case，参数顺序、参数含义、
> 返回值结构与 Rust / Java / JavaScript 客户端完全一致（见下方契约表）。

## 安装

```powershell
cd clients\python
python -m pip install -e .
```

不使用 pip 时，也可以直接把 `clients/python` 加入 `PYTHONPATH` 后 `import oss_client`。

## 五行完成配置 → 上传 → 下载 → 预签名

```python
from oss_client import OSSClient

client = OSSClient("http://127.0.0.1:8080", "app-id", "app-secret", bucket="demo")
client.put_object("demo", "hello.txt", b"hello oss")
data = client.get_object("demo", "hello.txt")
url = client.presign_get("demo", "hello.txt", expires_in=600)
```

`access_key` 即管理 API 创建 App 时返回的 `app_id`，`secret_key` 即 `app_secret`。

## 配置

```python
from oss_client import OSSClient, OSSConfig

config = OSSConfig(
    endpoint="http://127.0.0.1:8080",   # 允许路径前缀，如 http://host:8080/api
    access_key="<app_id>",
    secret_key="<app_secret>",
    region="us-east-1",                 # 任意值，仅参与签名
    bucket="demo",                      # 默认桶，可被各 API 的 bucket 参数覆盖
    timeout=30.0,                       # 单次操作超时（秒）
    max_retries=3,                      # 可重试错误的重试次数（不含首次）
    retry_backoff=0.5,                  # 指数退避基数（秒）
)
client = OSSClient(config)
```

| 参数 | 说明 | 默认值 |
| --- | --- | --- |
| `endpoint` | 服务地址，`http(s)://host[:port][/prefix]`；允许路径前缀 | 必填 |
| `access_key` | App ID | 必填 |
| `secret_key` | App Secret | 必填 |
| `region` | 签名作用域中的 region，服务端不校验 | `us-east-1` |
| `bucket` | 默认桶名 | `None` |
| `timeout` | 连接/读取超时（秒） | `30.0` |
| `max_retries` | 重试次数，仅对 429/500/502/503/504 与网络错误生效 | `3` |
| `retry_backoff` | 指数退避基数（秒） | `0.5` |

`endpoint` 带路径前缀时，请求路径为 `<prefix>/<bucket>/<key>`，签名也使用完整路径；
预签名 URL 同样包含前缀。

客户端支持上下文管理器，退出时关闭连接：

```python
with OSSClient("http://127.0.0.1:8080", "app-id", "app-secret", bucket="demo") as client:
    client.put_object("demo", "a.txt", b"a")
```

## 统一契约（23 个方法）

所有方法第一个参数都是 `bucket`（传 `None` 表示使用配置中的默认桶 `OSSConfig.bucket`；
配置里也没有默认桶时抛 `ValueError` 并说明原因）。
参数顺序固定，不提供关键字以外的变体。

| # | 方法签名 | 返回值 |
| --- | --- | --- |
| 1 | `create_bucket(bucket)` | 无 |
| 2 | `delete_bucket(bucket)` | 无 |
| 3 | `list_buckets()` | 桶列表，每项含 `name`、`created_at` |
| 4 | `head_bucket(bucket)` | 桶信息，含 `exists`、`region`（取 `x-amz-bucket-region`，缺失回退配置 region） |
| 5 | `bucket_exists(bucket)` | `bool` |
| 6 | `get_bucket_location(bucket)` | `str`，空值归一化为 `"us-east-1"` |
| 7 | `put_object(bucket, key, body, options)` | 含 `etag`、`size` |
| 8 | `upload_file(bucket, key, path, options)` | 含 `etag`、`size` |
| 9 | `get_object(bucket, key, range)` | `bytes` |
| 10 | `get_object_to_file(bucket, key, path, range)` | 无 |
| 11 | `head_object(bucket, key)` | 含 `size`、`etag`、`content_type`、`metadata`、`last_modified` |
| 12 | `object_exists(bucket, key)` | `bool` |
| 13 | `delete_object(bucket, key)` | 无 |
| 14 | `delete_objects(bucket, keys, quiet=False)` | 含 `deleted`、`errors` |
| 15 | `copy_object(source_bucket, source_key, dest_bucket, dest_key, metadata, metadata_directive)` | 含 `etag` |
| 16 | `list_objects_v2(bucket, prefix, delimiter, max_keys, continuation_token, start_after)` | 含 `items`、`common_prefixes`、`is_truncated`、`next_token` |
| 17 | `create_multipart_upload(bucket, key, content_type, metadata)` | `upload_id` 字符串 |
| 18 | `upload_part(bucket, key, upload_id, part_number, body)` | `etag` 字符串 |
| 19 | `complete_multipart_upload(bucket, key, upload_id, parts)` | `etag` 字符串 |
| 20 | `abort_multipart_upload(bucket, key, upload_id)` | 无 |
| 21 | `list_parts(bucket, key, upload_id)` | 含 `parts`、`is_truncated` |
| 22 | `presign_get(bucket, key, expires_in)` | `url` |
| 23 | `presign_put(bucket, key, expires_in)` | `url` |

`put_object` / `upload_file` 的 `options` 为字典，字段名四门语言一致：

| 字段 | 说明 | 默认 |
| --- | --- | --- |
| `content_type` | 对象 Content-Type | `None` |
| `metadata` | 自定义元数据字典 | `None` |
| `content_md5` | base64 字符串或 16 字节 MD5 摘要 | `None` |
| `part_size` | 自动分片阈值（字节） | `8 * 1024 * 1024` |
| `auto_multipart` | 超过 `part_size` 是否自动分片 | `True` |

## API 用法

### 桶

```python
client.create_bucket("demo")
assert client.bucket_exists("demo")

info = client.head_bucket("demo")      # BucketInfo
assert info.exists and info.region == "us-east-1"

assert client.get_bucket_location("demo") == "us-east-1"
print([(b.name, b.created_at) for b in client.list_buckets()])

client.delete_bucket("demo")           # 桶非空时抛出 BucketNotEmptyError
```

桶不存在时 `head_bucket` 返回 `exists=False`，不抛异常；其它错误照常抛出。

### 上传对象

```python
from pathlib import Path

# bytes / 文本内容
client.put_object("demo", "notes.txt", b"hello", {"content_type": "text/plain"})
client.put_object("demo", "notes2.txt", "hello")          # str 按内容处理

# 文件路径（Path；流式上传，不会整体读入内存）
client.put_object("demo", "big.bin", Path("big.bin"))

# 文件对象（流式）
with open("big.bin", "rb") as fp:
    client.put_object("demo", "big3.bin", fp)

# 文件路径上传：四门语言同名同义
client.upload_file("demo", "big4.bin", "big.bin")

# 元数据 + Content-MD5
import hashlib
client.put_object(
    "demo",
    "meta.txt",
    b"x",
    {
        "metadata": {"owner": "alice"},
        "content_md5": hashlib.md5(b"x").digest(),
    },
)
```

`put_object` 的 `body` 为 `str` 时按内容处理；按路径上传请用 `pathlib.Path` 或
`upload_file`。`auto_multipart=True`（默认）时，超过 `part_size` 会自动走分片上传，
与 `upload_file` 是同一条代码路径，行为一致（分片失败自动 abort）。

自定义元数据在本地校验：key 必须符合 RFC 7230 token 字符集；value 只允许可打印 ASCII
（`0x20..0x7E`），Tab、CR、LF、NUL 等控制字符一律拒绝。非法时直接抛出 `ValueError`，
不会发出请求。

#### Content-MD5 的三种情况

| 场景 | 发送内容 | 校验方 |
| --- | --- | --- |
| 单次 PUT（未超过 `part_size` 或 `auto_multipart=False`） | 整对象 Content-MD5 | 服务端校验 |
| 自动分片（超过 `part_size`） | **不发整对象 MD5**，每片上传前计算该片 MD5 并作为该分片 UploadPart 的 Content-MD5 | 服务端逐片校验 |
| `content_md5` 传入且请求体可重放（bytes、路径、可 seek 文件对象） | 无论单次 PUT 还是自动分片，都先做本地整对象校验；不匹配直接抛 `BadDigestError`（`status=None`，不发请求） | SDK 本地 |
| 不可重放的一次性流 + `content_md5` | 跳过本地整对象校验 | 服务端（仅单次 PUT） |

分片上传失败（含某片 MD5 不匹配）会自动 abort，不残留 upload 与临时分片。

#### 流式输入规则

- **未知长度的流**（无法通过 `len()` / `tell()` 得到长度，或不可 seek）：一律走单次 PUT
  （流式发送），**不尝试分片**。
- **已知长度且可重放**（bytes、路径、可 seek 文件对象）：超过 `part_size` 时才走自动分片；
  分片失败可重试。
- 不可重放的一次性流只尝试一次，不做重试。

### 下载对象

```python
data = client.get_object("demo", "notes.txt")       # bytes
assert data == b"hello"

# 流式写入文件（路径可为 str 或 Path）
client.get_object_to_file("demo", "big.bin", "download.bin")

# Range：字符串或 (start, end) 元组，end=None 表示到末尾
client.get_object("demo", "big.bin", "bytes=0-1023")
client.get_object("demo", "big.bin", "bytes=-100")
client.get_object("demo", "big.bin", (0, 1023))
client.get_object_to_file("demo", "big.bin", "part.bin", (0, 1023))

# 元信息
head = client.head_object("demo", "notes.txt")
print(head.size, head.etag, head.content_type, head.metadata, head.last_modified)
```

### 删除、复制、批量删除

```python
client.copy_object("demo", "source.txt", "demo", "dest.txt")      # 默认 COPY

client.copy_object(                                               # 传 metadata → REPLACE
    "demo", "source.txt", "demo", "dest.txt",
    metadata={"v": "2"},
)

client.delete_object("demo", "dest.txt")

result = client.delete_objects("demo", ["a.txt", "b.txt"])        # quiet 默认 False
assert not result.errors
assert sorted(result.deleted) == ["a.txt", "b.txt"]

assert not client.object_exists("demo", "never.txt")
```

- `delete_objects` 超过 1000 个 Key 时自动分批（每批 1000 个），返回聚合后的
  `deleted` 与 `errors`（`errors` 只包含失败项）。传入空 `keys` 时直接返回空结果
  （`deleted` 与 `errors` 均为空），不抛异常、不发请求。
- `copy_object` 未显式指定 `metadata_directive` 时：`metadata` 非空按 `REPLACE`，
  未传或传入空 dict（`{}`）一律按 `COPY`。

### 列表与分页遍历

```python
page = client.list_objects_v2("demo", prefix="logs/", delimiter="/", max_keys=100)
print(page.is_truncated, page.next_token)
print([item.key for item in page.items], page.common_prefixes)

# 手动翻页
page2 = client.list_objects_v2("demo", prefix="logs/", max_keys=100,
                               continuation_token=page.next_token)

# 生成器自动翻页
for item in client.iter_objects_v2("demo", prefix="logs/", max_keys=100):
    print(item.key, item.size, item.etag)

# 指定起点
client.list_objects_v2("demo", prefix="logs/", start_after="logs/2026-01-01.log")
```

### 分片上传

低层 API（`upload_part` 与 `complete_multipart_upload` 都只返回 ETag 字符串）：

```python
upload_id = client.create_multipart_upload("demo", "big.bin")
etag1 = client.upload_part("demo", "big.bin", upload_id, 1, chunk1)
etag2 = client.upload_part("demo", "big.bin", upload_id, 2, chunk2)

parts = client.list_parts("demo", "big.bin", upload_id)
assert [p.part_number for p in parts.parts] == [1, 2]

final_etag = client.complete_multipart_upload(
    "demo",
    "big.bin",
    upload_id,
    [
        {"part_number": 1, "etag": etag1},
        {"part_number": 2, "etag": etag2},
    ],
)
# 出错时：
# client.abort_multipart_upload("demo", "big.bin", upload_id)
```

`parts` 元素可以是 `{"part_number": n, "etag": e}`、`(n, e)` 元组或 `PartResult`。
提交前 SDK 会按 `part_number` 升序排序，不依赖调用方的传入顺序。

高层便捷方法（默认阈值 8MB，超过自动分片；失败自动 abort）：

```python
client.upload_file("demo", "big.bin", "big.bin", {"part_size": 8 * 1024 * 1024})
```

### 预签名 URL

```python
get_url = client.presign_get("demo", "report.pdf")              # 默认 900 秒
put_url = client.presign_put("demo", "upload.bin", expires_in=600)
```

第三方可直接使用，无需签名：

```python
import urllib.request

with open("upload.bin", "rb") as fp:
    request = urllib.request.Request(put_url, data=fp.read(), method="PUT")
    urllib.request.urlopen(request)

body = urllib.request.urlopen(get_url).read()
```

`expires_in` 默认 900 秒，有效范围 1..604800 秒。

## 错误处理

所有异常都继承自 `OSSError`，统一暴露 `code`、`message`、`status`、`request_id`
四个字段（网络层错误没有 HTTP 状态码，`status` 与 `request_id` 为 `None`）：

```python
from oss_client import OSSError

try:
    client.get_object("demo", "not-exist.txt")
except OSSError as exc:
    print(type(exc).__name__, exc.code, exc.message, exc.status, exc.request_id)
```

服务端错误抛出 `OSSServiceError` 的具体子类，关键属性：

- `code`：S3 错误码，如 `NoSuchKey`、`NoSuchBucket`、`AccessDenied`
- `message`：错误描述
- `status`（`status_code` 别名）：HTTP 状态码
- `request_id`：服务端 RequestId
- `resource`：错误 XML 中的资源路径
- `operation`：触发错误的 SDK 操作（如 `PutObject`）
- `retryable`：该状态码是否会触发 SDK 自动重试

常见映射：`NoSuchKeyError`(404)、`NoSuchBucketError`(404)、`NoSuchUploadError`(404)、
`BucketNotEmptyError`(409)、`BucketAlreadyOwnedByYouError`(409)、`AccessDeniedError`(403)、
`SignatureDoesNotMatchError`(403)、`InvalidAccessKeyIdError`(403)、`BadDigestError`(400)、
`PreconditionFailedError`(412)、`InvalidRangeError`(416)。

行为约定：

| 场景 | 行为 |
| --- | --- |
| 404 | 不重试；对象操作抛 `NoSuchKeyError`，桶操作抛 `NoSuchBucketError`；`object_exists` / `bucket_exists` 返回 `False`，`head_bucket` 返回 `exists=False` 的 `BucketInfo` |
| 403 | 不重试；按错误码抛 `AccessDeniedError` / `SignatureDoesNotMatchError` / `InvalidAccessKeyIdError` |
| Content-MD5 不一致 | 可重放体：SDK 本地抛 `BadDigestError`（`status=None`，不发请求）；自动分片：服务端逐片校验返回 400 `BadDigest` |
| 429/5xx | 按 `max_retries` 与指数退避自动重试，耗尽后抛对应 `OSSServiceError` |
| 超时 | 自动重试，耗尽后抛 `OSSTimeoutError`（`code="TimeoutError"`），`attempts` 为实际尝试次数 |
| 网络错误 | 自动重试，耗尽后抛 `OSSClientError`（`code="NetworkError"`），`attempts` 为实际尝试次数 |

重试策略统一：只有可重放的请求体（bytes、文件路径、可 seek 文件对象）才重试；
不可重放的一次性流一律只尝试一次。预签名 URL 过期由服务端返回 403 `AccessDenied`。

## 已知限制

- 仅支持 path-style（`/bucket/key`），不支持 virtual-hosted-style（`bucket.host/key`）。
- 服务端不校验 region，`region` 仅参与签名；签名作用域固定为 `s3`。
- 预签名 URL 只签名 `host`，PUT 链接不绑定 `Content-Type` 等请求头。
- bytes/str 请求体使用真实 `x-amz-content-sha256`；文件对象走流式上传时使用
  `UNSIGNED-PAYLOAD`（服务端不校验 body 哈希）。
- 自动分片时不发送整对象 Content-MD5，改为逐片发送分片 MD5；不可重放的一次性流
  无法做本地整对象 MD5 校验。
- 返回的自定义元数据 Key 统一为小写（服务端行为）。
- 桶删除为软删除（进入回收站）；删除后 `HeadBucket` 与 `GetBucketLocation` 一致返回
  404 `NoSuchBucket`（服务端已修复 `location` 分支的桶存在性校验）。
- `GetBucketLocation` 返回空字符串时，SDK 归一化为 `us-east-1`。
- 服务端 `ListParts` 一次性返回全部分片，SDK 未提供分页参数。
- 客户端复用单个 HTTP 连接，非线程安全；多线程场景请为每个线程创建独立客户端。
- 未支持的 S3 能力（ACL、版本控制、对象锁、加密等）以服务端协议文档
  `docs/s3-compatible-api.md` 的“未兼容清单”为准。

## 测试

单元测试（AWS 官方 SigV4 测试向量、错误映射、重试/超时、分页，以及统一契约项：
自动分片、批量分批、copy directive 推导、endpoint 路径前缀、元数据校验、
Content-MD5 校验）：

```powershell
cd clients\python
python -m unittest discover -s tests -p "test_unit.py" -v
python -m unittest discover -s tests -p "test_signer.py" -v
```

集成测试会自动拉起真实服务端，覆盖建桶、上传/下载、HEAD、分页、复制、批量删除、
预签名 GET/PUT（含过期）、分片上传全流程、`put_object` 自动分片、Content-MD5 与
失败自动 abort：

```powershell
python -m unittest discover -s tests -p "test_integration.py" -v
# 跳过集成测试：
$env:OSS_SKIP_INTEGRATION="1"
```

集成测试约定：

- 固定地址 `127.0.0.1:18103`，数据目录 `%TEMP%\oss-server-python-data`（每次启动前清空）；
- 优先复用 `target/release/oss-server.exe`，当它比 `src/**/*.rs` 旧时自动改用临时目录
  `%TEMP%\oss-server-build-py` 构建（设置 `CARGO_TARGET_DIR`，不与主构建抢锁）；
- 也可用 `OSS_SERVER_BIN` 指定二进制；
- 启动后通过管理 API 创建临时 App 获取 `app_id/app_secret`；
- 测试结束会 terminate/kill 服务端进程，不留下占用端口的后台进程。

运行全部测试：

```powershell
python -m unittest discover -s tests -v
```
