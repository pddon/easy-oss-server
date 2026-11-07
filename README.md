# OSS Server - 轻量级分布式对象存储服务

基于 Rust + Axum 0.7 构建的轻量级对象存储服务，兼容 OSS 风格 API，支持 HMAC-SHA256 签名鉴权、App 级隔离和内置 Web 管理后台。文件内容落本地磁盘，元数据通过统一的 `Store` 抽象层持久化。

\*\*默认后端是 redb（纯 Rust 嵌入式数据库 + bincode），零外部服务、零 C 依赖。\*\*可随时通过配置切换为内存后端（测试 / 无数据库部署）或 MySQL（可选 `mysql` feature）。切换只需改环境变量，不需要改代码。

[![CI](https://github.com/pddon/easy-oss-server/actions/workflows/ci.yml/badge.svg)](https://github.com/pddon/easy-oss-server/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](https://www.rust-lang.org)
[![Docker](https://img.shields.io/badge/docker-ready-blue.svg)](dist/DEPLOY.md)

## 目录

- [快速开始](#快速开始)
  - [从源码构建](#从源码构建)
  - [Docker 运行](#docker-运行)
  - [客户端 SDK](#客户端-sdk)
- [特性](#特性)
- [配置项](#配置项)
- [存储后端与切换](#存储后端与切换)
- [管理后台](#管理后台)
- [API 文档](#api-文档)
- [使用示例](#使用示例)
- [项目结构](#项目结构)
- [测试](#测试)
- [发布](#发布)
- [贡献与安全](#贡献与安全)
- [License](#license)

## 快速开始

### 从源码构建

需要 Rust 1.87+。默认使用 redb，不需要 MySQL，也不需要 C 编译器：

```bash
export OSS_ADMIN_TOKEN=your-secret-token   # 必填，管理后台/管理 API 用
cargo build --release
./target/release/oss-server
```

开发期常用命令：

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Windows + GNU 工具链（无 MSVC）环境示例：

```powershell
$TC="$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-gnu"
$env:PATH="$TC\bin;$TC\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;$env:PATH"
& "$TC\bin\cargo.exe" build --release
```

> `target/release/oss-server.exe` 不随源码自动更新，联调前请用当前源码重新构建。
> 多任务并行构建时建议设置 `CARGO_TARGET_DIR` 使用独立产物目录，避免共用 `target/` 的构建锁。

### Docker 运行

```bash
# 方式一：直接用 compose 构建并启动
OSS_ADMIN_TOKEN=your-secret-token docker-compose up -d --build

# 方式二：先构建镜像，再运行
docker build -t oss-server:latest .
docker run -d --name oss-server \
  -e OSS_ADMIN_TOKEN=your-secret-token \
  -e OSS_MASTER_KEY=your-master-key \
  -p 8080:8080 -v "$PWD/data:/data" \
  oss-server:latest
```

`dist/` 是完整的可分发部署包（compose、`.env.example`、构建/部署/运维脚本、部署文档），详见 [dist/DEPLOY.md](dist/DEPLOY.md)。镜像以非 root 用户（uid/gid 10001）运行，内置 `/healthz` 健康检查。

服务启动后访问 <http://localhost:8080/admin/>，首次进入需输入 `OSS_ADMIN_TOKEN`。

### 客户端 SDK

| 语言 | 目录 | 安装 |
| --- | --- | --- |
| Rust | [clients/rust](clients/rust) | `cargo add pddon-oss-client` |
| Java | [clients/java](clients/java) | `mvn install`（Maven 坐标见 [README](clients/java/README.md)） |
| JavaScript | [clients/js](clients/js) | `npm install com.pddon.oss-client` |
| TypeScript | [clients/typescript](clients/typescript) | `npm install com.pddon.oss-client-ts` |
| Python | [clients/python](clients/python) | `pip install pddon-oss-client` |

> 包名以 `scripts/release/release.env` 的配置为准。客户端契约见 [clients/README.md](clients/README.md)。

## 特性

- **流式传输**：上传边读边写磁盘，下载使用 `ReaderStream`，不把整个文件读入内存；支持 `Range` 断点/分段下载（206 + `Content-Range`）
- **签名鉴权**：OSS 风格 `Authorization: OSS {app_id}:{signature}`，HMAC-SHA256 + Base64，使用常量时间比较
- **App 隔离**：每个 App 只能访问自己的桶；App Secret 使用 AES-256-GCM 加密后入库
- **安全**：对象 key 白名单校验（拒绝 `..`、前导 `/`、反斜杠等）、落盘文件名为 UUID、下载响应带 `X-Content-Type-Options: nosniff`
- **管理后台**：内置单页管理界面，支持 App/桶/文件全生命周期管理、回收站、清理与自检
- **回收站（软删除）**：普通删除 / 桶删除 / App 删除只把对象移入回收站并保留磁盘文件；还原回原桶原 key（key 占用可覆盖）；只有“彻底删除”或保留期到期后的自动清理才删除磁盘文件
- **启动自检**：启动时比对元数据与磁盘文件，识别失效记录与孤儿文件，输出结构化日志，支持 `report|fix` 两种模式
- **定时清理**：可配置周期的后台任务，清理过期回收站对象、失效记录、孤儿文件与过期 nonce；单轮串行、限量、限时执行，失败可重试
- **轻量部署**：单二进制、Docker 多阶段构建、数据卷持久化；`dist/` 提供 compose 部署包与运维脚本
- **并发安全**：redb 单写多读；MySQL 依赖唯一索引与原子 upsert；所有同步数据库调用在 `spawn_blocking` 中执行，不阻塞异步运行时
- **可切换持久化**：`OSS_STORAGE_BACKEND=redb|memory|mysql`，上层代码只依赖 `src/store` 的 trait，不感知具体数据库

## 配置项

所有配置通过环境变量设置：

| 环境变量                           | 默认值                     | 说明                                                                                   |
| ------------------------------ | ----------------------- | ------------------------------------------------------------------------------------ |
| `OSS_HOST`                     | `0.0.0.0:8080`          | 监听地址                                                                                 |
| `OSS_DATA_DIR`                 | `./data`                | 文件内容存储目录                                                                             |
| `OSS_STORAGE_BACKEND`          | `redb`                  | 元数据后端：`redb`（默认）/ `memory` / `mysql`；未设置且 `OSS_DB_URL` 以 `mysql://` 开头时自动推断为 `mysql` |
| `OSS_DB_URL`                   | `redb://./oss.db`       | redb 文件路径，或 MySQL DSN（`mysql://user:pass@host:port/db`）                              |
| `OSS_ADMIN_TOKEN`              | *(必填)*                  | 管理后台 / 管理 API Token                                                                  |
| `OSS_MASTER_KEY`               | *(自动派生)*                | App Secret 加密主密钥；留空或空字符串时由 `OSS_ADMIN_TOKEN` 派生                                      |
| `OSS_MAX_UPLOAD_SIZE`          | `10737418240`           | 最大上传字节数（默认 10GB）                                                                     |
| `OSS_CORS_ENABLED`             | `false`                 | 是否启用 CORS（启用时当前实现为 permissive）                                                       |
| `OSS_CORS_ALLOWED_ORIGINS`     | *(空)*                   | 预留字段，当前版本未生效                                                                         |
| `OSS_PUBLIC_BASE_URL`          | `http://localhost:8080` | 预留字段，当前版本未生效（签名 URL 由客户端生成）                                                          |
| `OSS_SIGNATURE_EXPIRE_SECONDS` | `900`                   | 预留字段，当前版本未生效（有效时间由签名 URL 的 `Expires` 决定）                                             |
| `OSS_CACHE_SIZE_BYTES`         | `67108864`              | 预留字段，当前版本未实现内存缓存                                                                     |
| `OSS_TRASH_RETENTION_DAYS`     | `30`                    | 回收站保留天数，到期由清理任务彻底删除；`<=0` 不自动清理                                                      |
| `OSS_CLEANUP_INTERVAL_MINUTES` | `30`                    | 定时清理周期（分钟），`0` 关闭定时任务（仍可后台手动触发）                                                      |
| `OSS_CLEANUP_MAX_OBJECTS`      | `1000`                  | 单次清理最多处理的对象/记录数                                                                      |
| `OSS_CLEANUP_MAX_SECONDS`      | `60`                    | 单次清理耗时上限（秒）                                                                          |
| `OSS_STARTUP_CHECK_MODE`       | `report`                | 启动自检模式：`report` 只报告 / `fix` 自动修复                                                     |
| `OSS_NONCE_RETENTION_SECONDS`  | `3600`                  | nonce 保留时长，超期由清理任务删除                                                                 |

## 存储后端与切换

元数据统一通过 `src/store` 中的 `Store` trait 访问，具体实现：

| 后端         | 依赖                                | 适用场景                   | 启动方式                         |
| ---------- | --------------------------------- | ---------------------- | ---------------------------- |
| `redb`（默认） | 纯 Rust `redb` + `bincode`         | 单机部署、零外部依赖             | 默认，无需额外配置                    |
| `memory`   | 无                                 | 测试、临时演示、明确不需要持久化的部署    | `OSS_STORAGE_BACKEND=memory` |
| `mysql`    | 可选 `mysql` feature（纯 Rust `sqlx`） | 多实例共享元数据、已有 MySQL 运维体系 | 编译时开 feature + 配置 DSN        |

### 默认（redb）怎么跑

```bash
export OSS_ADMIN_TOKEN=admin123
cargo run
# 元数据默认写入 ./oss.db，文件内容写入 ./data
```

### 切换到 MySQL

1. 编译时启用可选 feature（默认构建不会拉入任何 MySQL 依赖）：

```bash
cargo build --release --features mysql
cargo run --features mysql
```

1. 配置环境变量：

```bash
export OSS_STORAGE_BACKEND=mysql
export OSS_DB_URL='mysql://user:password@host:3306/oss_server'
# 密码中的 @ : / 等字符需要 URL 编码，例如 Simo@13579 -> Simo%4013579
export OSS_DATA_DIR=/data          # 文件内容仍然落本地磁盘
export OSS_ADMIN_TOKEN=...
```

1. 建库建表：**代码会自动完成**。连接时会先连到 MySQL 服务器执行
   `CREATE DATABASE IF NOT EXISTS oss_server`，再执行
   [`migrations/001_init.sql`](migrations/001_init.sql) 创建所有表，并把
   `schema_v1_mysql` 写入 `_migrations`。该 SQL 文件不是死文件，由
   `src/store/mysql_store.rs` 通过 `include_str!` 直接执行。
2. MySQL 集成测试（使用独立 schema `oss_server_test`，测试前后只清理该 schema 内的本项目表）：

```bash
cargo test --features mysql --test test_mysql_store
# 可用 OSS_MYSQL_TEST_URL 覆盖 DSN
```

### redb / MySQL / memory 数据不互通

三种后端的数据格式完全不同，**不能直接混用**：

- redb 数据是单个二进制文件（bincode 记录 + 索引），MySQL 无法读取；
- MySQL 数据在 `oss_server` 库的表中，redb 后端也不会读取；
- `memory` 后端进程退出即丢失。

切换后端时必须**重新初始化元数据**：

1. 保留原 `OSS_DATA_DIR` 中的文件内容（文件按 `app_id/bucket_id/.../object_id` 落盘，与后端无关）；
2. 在新后端上重新创建 App / Bucket，并重新上传对象，使元数据与磁盘文件重新对应；
3. 或者在切换前用管理 API 导出元数据，再按新后端 schema 导入（项目暂未内置自动迁移工具）。

不建议仅替换 `OSS_DB_URL` 后直接启动：旧元数据不会出现在新后端，已有 App / Secret 也无法继续使用。

## 管理后台

访问 `http://localhost:8080/admin/`：

- 应用管理：创建/删除应用、重置 Secret（Secret 仅在创建/重置响应中明文返回一次）
- 存储桶：创建/删除桶、设置公开/私有
- 文件浏览：上传、下载、删除（删除即移入回收站），列表支持前缀过滤
- 回收站：按应用/桶筛选、分页、批量还原与批量彻底删除
- 清理与自检：查看保留期/清理/自检配置，查看最近一次清理与自检结果，手动触发清理、自检（report/fix）

## API 文档

### 鉴权

#### 管理 API

使用 `X-Admin-Token` 请求头：

```bash
curl -H "X-Admin-Token: admin123" http://localhost:8080/api/admin/apps
```

#### OSS 风格签名（请求头）

```
Authorization: OSS {app_id}:{signature}
```

```
signature = Base64(HMAC-SHA256(app_secret, string_to_sign))
string_to_sign = VERB + "\n" + Content-MD5 + "\n" + Content-Type + "\n" + Date + "\n" + CanonicalizedResource
```

- `CanonicalizedResource` 为请求路径（如 `/my-bucket/path/to/file.txt`），不含查询参数
- `Date` 必须是 RFC2822（如 `Fri, 06 Nov 2026 10:00:00 GMT`），允许 ±15 分钟偏差
- `Content-MD5` 可省略（签名字段留空即可），服务端不做 body 校验
- `GET /{bucket}/{key}` 对公开桶可匿名访问；私有桶必须携带签名头或使用签名 URL

### 管理 API

```bash
# 应用
GET    /api/admin/apps?offset=0&limit=20
POST   /api/admin/apps                  {"name": "my-app"}
GET    /api/admin/apps/:app_id
DELETE /api/admin/apps/:app_id
POST   /api/admin/apps/:app_id/reset-secret

# 桶
GET    /api/admin/apps/:app_id/buckets?offset=0&limit=20
POST   /api/admin/apps/:app_id/buckets  {"name": "my-bucket", "is_public": false}
PUT    /api/admin/apps/:app_id/buckets/:bucket_name  {"is_public": true}
DELETE /api/admin/apps/:app_id/buckets/:bucket_name

# 对象（管理通道，免 App 签名）
GET    /api/admin/buckets/:bucket_name/objects?prefix=&marker=&max-keys=100
PUT    /api/admin/buckets/:bucket_name/objects/:key
GET    /api/admin/buckets/:bucket_name/objects/:key
DELETE /api/admin/buckets/:bucket_name/objects/:key   # 软删除：移入回收站，保留磁盘文件

# 回收站
GET    /api/admin/trash?app_id=&bucket_id=&offset=0&limit=20
POST   /api/admin/trash/:object_id/restore            {"overwrite": false}
DELETE /api/admin/trash/:object_id                    # 彻底删除（删元数据 + 磁盘文件）
POST   /api/admin/trash/batch                         {"action":"restore|purge","object_ids":[...],"overwrite":false}

# 维护
GET    /api/admin/maintenance/status                  # 配置 + 最近一次 cleanup / startup_check
POST   /api/admin/maintenance/cleanup                 # 手动触发清理（已在运行返回 409）
POST   /api/admin/maintenance/startup-check           {"mode":"report|fix"}（可省略，使用配置默认）

# 健康检查（无需鉴权）
GET    /healthz                                       # {"status":"ok","backend":"redb",...}，异常返回 503
```

### OSS 风格 API

```bash
# 桶（均需签名）
GET    /buckets
POST   /buckets            {"name": "my-bucket"}
GET    /buckets/:bucket_name
PUT    /buckets/:bucket_name    {"is_public": true}
DELETE /buckets/:bucket_name

# 对象
GET    /:bucket?prefix=&marker=&max-keys=100     # 需签名
PUT    /:bucket/*key                             # 需签名
DELETE /:bucket/*key                             # 需签名
GET    /:bucket/*key                             # 公开桶匿名 / 私有桶需签名或签名 URL
# 支持 Range: bytes=start-end、bytes=start-、bytes=-suffix
```

### 签名 URL 下载

私有桶可生成临时下载链接（无需 Authorization 头）：

```
GET /{bucket}/{key}?OSSAccessKeyId={app_id}&Expires={unix_timestamp}&Signature={url_encoded_base64}
```

签名字符串固定为：

```
GET\n\n\n{Expires}\n/{bucket}/{key}
```

`Signature` 必须做 URL 编码（Base64 中的 `+`、`/`、`=` 直接放入 query 会被改写）。

## 使用示例

### curl 上传（请求头签名）

```bash
#!/bin/bash
APP_ID="your-app-id"
APP_SECRET="your-app-secret"
BUCKET="my-bucket"
KEY="hello.txt"
FILE="hello.txt"
DATE=$(date -u +"%a, %d %b %Y %H:%M:%S GMT")
CONTENT_TYPE="text/plain"

STRING_TO_SIGN="PUT\n\n${CONTENT_TYPE}\n${DATE}\n/${BUCKET}/${KEY}"
SIGNATURE=$(printf '%b' "$STRING_TO_SIGN" | openssl dgst -sha256 -hmac "$APP_SECRET" -binary | base64)

curl -X PUT \
  -H "Date: $DATE" \
  -H "Content-Type: $CONTENT_TYPE" \
  -H "Authorization: OSS ${APP_ID}:${SIGNATURE}" \
  --data-binary "@${FILE}" \
  "http://localhost:8080/${BUCKET}/${KEY}"
```

### curl 下载（签名 URL）

```bash
APP_ID="your-app-id"
APP_SECRET="your-app-secret"
BUCKET="my-bucket"
KEY="hello.txt"
EXPIRES=$(( $(date +%s) + 600 ))

STRING_TO_SIGN="GET\n\n\n${EXPIRES}\n/${BUCKET}/${KEY}"
RAW_SIG=$(printf '%b' "$STRING_TO_SIGN" | openssl dgst -sha256 -hmac "$APP_SECRET" -binary | base64)
SIG=$(printf '%s' "$RAW_SIG" | jq -sRr @uri)   # URL 编码

curl "http://localhost:8080/${BUCKET}/${KEY}?OSSAccessKeyId=${APP_ID}&Expires=${EXPIRES}&Signature=${SIG}" -o "$KEY"
```

### Python 客户端

```python
import base64, hashlib, hmac, time, requests
from datetime import datetime, timezone

class OSSClient:
    def __init__(self, endpoint, app_id, app_secret):
        self.endpoint = endpoint.rstrip('/')
        self.app_id = app_id
        self.app_secret = app_secret.encode()

    def _sign(self, method, content_type, date, resource):
        sts = f"{method}\n\n{content_type}\n{date}\n{resource}"
        return base64.b64encode(
            hmac.new(self.app_secret, sts.encode(), hashlib.sha256).digest()
        ).decode()

    def put_object(self, bucket, key, data, content_type='application/octet-stream'):
        date = datetime.now(timezone.utc).strftime('%a, %d %b %Y %H:%M:%S GMT')
        resource = f"/{bucket}/{key}"
        headers = {
            'Date': date,
            'Content-Type': content_type,
            'Authorization': f'OSS {self.app_id}:{self._sign("PUT", content_type, date, resource)}',
        }
        return requests.put(f'{self.endpoint}{resource}', data=data, headers=headers)

    def generate_signed_url(self, bucket, key, expires=600):
        expiry = int(time.time()) + expires
        resource = f"/{bucket}/{key}"
        sts = f"GET\n\n\n{expiry}\n{resource}"
        sig = base64.b64encode(
            hmac.new(self.app_secret, sts.encode(), hashlib.sha256).digest()
        ).decode()
        from urllib.parse import quote
        return (f'{self.endpoint}{resource}?OSSAccessKeyId={self.app_id}'
                f'&Expires={expiry}&Signature={quote(sig)}')
```

## 存储结构

```
data/
└── {app_id}/
    └── {bucket_id}/
        └── {key_prefix}/     # key 的前两级目录（单级 key 时为 "root"）
            └── {object_id}    # UUID，实际文件名
```

- 元数据（App/桶/对象）默认存于 redb 文件（`./oss.db`），也可切换 MySQL / memory
- `object_id` 为 UUID，避免 key 中的特殊字符导致路径穿越
- `key_prefix` 取 key 的前两级目录，避免单目录文件过多

## 安全说明

1. **App Secret 加密存储**：AES-256-GCM，随机 nonce，密文以 base64 入库；主密钥来自 `OSS_MASTER_KEY`
2. **路径安全**：key 仅允许字母、数字、`.`、`_`、`-`、`/`；拒绝 `..`、前导 `/`、空 key；文件落盘名为 UUID；GET/DELETE 同样做 key 校验
3. **签名防重放**：请求头签名校验 `Date` ±15 分钟；签名 URL 校验 `Expires`
4. **MIME 嗅探防护**：对象下载（含 Range 响应）与管理后台静态资源均返回 `X-Content-Type-Options: nosniff`
5. **上传限制**：`RequestBodyLimitLayer` 全局限制 + 流式写入时累计校验，超限返回 413 并删除半成品文件
6. **错误信息**：对外只返回通用错误，不泄露内部路径/堆栈

## 性能

- 上传/下载全程流式，不整文件驻留内存
- redb 写事务串行、读并发；MySQL 使用连接池；DB 调用通过 `spawn_blocking` 与 Tokio 解耦
- Release 启用 `opt-level=z` + LTO + strip + panic=abort，二进制最小化

## 项目结构

```
oss-server/
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── README.md
├── migrations/
│   ├── 001_init.sql        # MySQL v2 DDL，由 mysql 后端启动时执行
│   └── 002_upgrade_v2.sql  # v1 -> v2 自动升级（回收站字段 / active 唯一索引）
├── dist/                   # 可直接分发的 compose 部署包（见 dist/DEPLOY.md）
├── src/
│   ├── main.rs             # 启动入口（按 OSS_STORAGE_BACKEND 装配后端）
│   ├── lib.rs              # 库入口：AppState + 路由装配
│   ├── config.rs
│   ├── store/
│   │   ├── mod.rs          # Store trait / Backend / 工厂
│   │   ├── redb_store.rs   # 默认 redb 实现
│   │   ├── memory_store.rs # 内存实现（测试 / 无数据库）
│   │   └── mysql_store.rs  # 可选 MySQL 实现（feature = "mysql"）
│   ├── models/{app,bucket,object,maintenance}.rs   # 仅业务规则与校验
│   ├── handlers/{app,bucket,object,admin_bucket,admin_object,admin_trash,admin_maintenance}.rs
│   ├── services/{crypto,signature,storage,maintenance}.rs
│   ├── middleware/{auth,admin_auth}.rs
│   └── web/{mod.rs,admin_handler.rs,static/}
└── tests/{test_app,test_bucket,test_object,test_signature,test_e2e,test_store_memory,test_trash,test_admin_trash_api,test_mysql_store}.rs
```

## 测试

```bash
cargo test --all-targets
cargo test --features mysql --all-targets   # 额外运行真实 MySQL 集成测试
```

默认特性下共 **111** 个测试（lib 49 + app 5 + bucket 7 + e2e 12 + object 12 + signature 16 + memory 后端 1 + 回收站/清理/自检 7 + 管理端 HTTP 2）。开启 `mysql` feature 后额外增加 1 个真库集成测试（`test_mysql_store`，使用 `oss_server_test` 独立 schema，覆盖软删除/还原冲突/回收站分页/维护记录/并发写入）。测试默认使用 memory 后端（`store::init_test_store`），不写真实元数据库；E2E 测试使用临时目录存放文件。

## 已知限制 / 语义说明

- **三种后端数据不互通**：redb / MySQL / memory 切换时必须重新初始化元数据（见「存储后端与切换」）。
- **删除语义**：普通删除、桶删除、App 删除都只把对象移入回收站并保留磁盘文件；桶/App 记录被删除后，回收站对象仍保留原桶/原 key 与 `app_id`，还原时若原桶已不存在会返回 409（`BucketMissing`）。
- **彻底删除**：只有回收站“彻底删除”和保留期到期后的定时清理会删除磁盘文件；删除文件失败时保留元数据，下一轮清理重试。
- **自检/清理不碰元数据文件**：redb 数据库默认位于数据目录内（如 `/data/oss.db`），自检与清理的磁盘扫描会显式排除它，避免被当作孤儿文件删除。
- **覆盖写**：同一 key 的新上传会把旧版本移入回收站（不是立即删文件），元数据唯一约束只作用于 active 对象。
- **并发写**：redb 单写多读；MySQL 进程内按 bucket 串行化写入并对跨进程死锁做有限重试；极端跨进程竞争仍可能返回可重试错误。
- `nonces` 表与 `Store::insert_nonce` 已实现并测试，但尚未接入鉴权中间件；防重放目前依赖 `Date` 时间窗与签名 URL 的 `Expires`。
- `OSS_PUBLIC_BASE_URL` / `OSS_SIGNATURE_EXPIRE_SECONDS` / `OSS_CACHE_SIZE_BYTES` / `OSS_CORS_ALLOWED_ORIGINS` 为预留配置，当前版本不生效。

## 发布

发布名称与地址集中在 [scripts/release/release.env](scripts/release/release.env)。拿到最终仓库地址/包名后，改这一处，然后执行：

```bash
python scripts/release/apply-names.py --apply   # 同步到各语言清单与文档
```

发布脚本默认 **dry-run**（只打包、校验、预演，不上传不推送），确认后加 `--execute`：

```bash
bash scripts/release/release-rust.sh      # crates.io
bash scripts/release/release-java.sh      # GitHub Packages / Maven Central
bash scripts/release/release-npm-js.sh    # npm
bash scripts/release/release-npm-ts.sh    # npm
bash scripts/release/release-python.sh    # TestPyPI / PyPI
bash scripts/release/release-docker.sh    # 多架构镜像
bash scripts/release/release-github.sh    # tag + GitHub Release
```

CI 见 [.github/workflows/ci.yml](.github/workflows/ci.yml)；发布工作流见 [.github/workflows/release.yml](.github/workflows/release.yml)（默认关闭，需手动触发或显式开启 `PUBLISH_ENABLED`）。

## 贡献与安全

- 贡献流程与代码约定：[CONTRIBUTING.md](CONTRIBUTING.md)
- 漏洞报告方式：[SECURITY.md](SECURITY.md)

## License

本项目采用双许可：**MIT OR Apache-2.0**（服务端与 Rust 客户端），其余语言客户端为 **MIT**。

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)
