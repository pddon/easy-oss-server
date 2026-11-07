# OSS Server 部署指南（dist 部署包）

本目录是可分发的最小部署包：一条 `docker compose` 命令即可启动 OSS Server，
配置、数据目录、端口都可以通过 `.env` 与挂载定制。

## 1. 目录清单

```
dist/
├── docker-compose.yml     # 服务编排：端口、数据卷、env_file、健康检查
├── Dockerfile             # 多阶段镜像构建（默认 redb，可选 mysql 特性）
├── .env.example           # 全部环境变量说明与默认值
├── build.sh               # 构建镜像（Linux/macOS/Git Bash）
├── build.ps1              # 构建镜像（Windows PowerShell）
├── deploy.sh              # 拉取/构建 + 启动 + 健康检查
├── stop.sh                # 停止服务（保留数据）
├── restart.sh             # 重启 + 健康检查
├── logs.sh                # 查看日志（-f 跟随）
├── status.sh              # 容器/健康/数据目录/维护任务状态
├── backup.sh              # 备份数据目录 + redb/MySQL 元数据
├── restore.sh             # 从备份恢复
├── uninstall.sh           # 卸载（--purge 连数据与镜像一起删除）
├── lib/common.sh          # 脚本公共函数
└── DEPLOY.md              # 本文档
```

## 2. 前置要求

- Docker Engine 20.10+ / Docker Desktop，或 Docker Toolbox（见第 8 节）。
- Docker Compose：`docker compose` 插件或 `docker-compose` 1.20+。
- 宿主机至少 1 GB 内存；镜像构建建议 2 GB 以上。

检查：

```bash
docker version
docker compose version   # 或 docker-compose version
```

## 3. 首次部署

```bash
cd dist
cp .env.example .env
# 必须修改：OSS_ADMIN_TOKEN、OSS_MASTER_KEY
# 可选修改：OSS_PORT、OSS_DATA_HOST_DIR、OSS_PUBLIC_BASE_URL
vi .env

chmod +x *.sh            # 从压缩包/Windows 拷贝后可能需要补执行权限
./deploy.sh --build      # 首次建议本地构建（或 ./deploy.sh 使用 OSS_IMAGE 拉取）
```

脚本会：

1. 检查 `.env`、数据目录可写、宿主机端口是否被占用；
2. 校验 `docker-compose.yml`；
3. 构建或拉取镜像；
4. `docker compose up -d`；
5. 轮询 `/healthz` 直到通过，超时会打印最近日志并退出非 0。

访问：

- 健康检查：`http://<宿主机>:${OSS_PORT}/healthz`
- 管理后台：`http://<宿主机>:${OSS_PORT}/admin/`（首次进入输入 `OSS_ADMIN_TOKEN`）

> `OSS_MASTER_KEY` 用于加密 App Secret。**首次部署后不要再随意修改**，否则旧 Secret 无法解密。
>
> 容器进程以非 root 用户 `app`（uid/gid `10001`）运行。镜像入口会在启动时自动修正 `/data`
> 归属后再降权，因此普通 bind mount 无需手工 chown；Rootless Docker 或显式 `--user` 运行时，
> 需保证宿主机数据目录对该 uid 可写。

## 4. 配置、端口与数据目录

全部变量及默认值见 `.env.example`，常用项：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `OSS_PORT` | `8080` | 宿主机端口，容器内固定 8080 |
| `OSS_DATA_HOST_DIR` | `./data` | 宿主机数据目录，挂载到容器 `/data` |
| `OSS_ADMIN_TOKEN` | 无（必填） | 管理后台/管理 API 令牌 |
| `OSS_MASTER_KEY` | 无（必填） | Secret 加密主密钥 |
| `OSS_STORAGE_BACKEND` | `redb` | `redb` / `memory` / `mysql` |
| `OSS_DB_URL` | `redb:///data/oss.db` | 元数据库连接串 |
| `OSS_TRASH_RETENTION_DAYS` | `30` | 回收站保留天数 |
| `OSS_CLEANUP_INTERVAL_MINUTES` | `30` | 清理周期，0 关闭定时任务 |
| `OSS_CLEANUP_MAX_OBJECTS` | `1000` | 单次清理对象上限 |
| `OSS_CLEANUP_MAX_SECONDS` | `60` | 单次清理耗时上限 |
| `OSS_STARTUP_CHECK_MODE` | `report` | 启动自检 `report` / `fix` |
| `OSS_NONCE_RETENTION_SECONDS` | `3600` | nonce 保留时长 |

数据目录内容：

- `data/`：按 `app/bucket/key` 分层的对象文件；
- `data/../oss.db`（redb 后端默认 `redb:///data/oss.db`，即容器内 `/data/oss.db`）：元数据库。

修改端口后重新 `./deploy.sh --no-build` 即可；`deploy.sh` 会先检测端口占用。

## 5. 升级

```bash
cd dist
./backup.sh                       # 先备份
./build.sh -v 1.2.0               # 构建新镜像
sed -i 's#^OSS_IMAGE=.*#OSS_IMAGE=oss-server:1.2.0#' .env
./deploy.sh --no-build            # 滚动重启 + 健康检查
./status.sh
```

- 数据库 schema 会在启动时自动升级（redb `schema_v1 -> v2`；MySQL 执行 `migrations/002_upgrade_v2.sql`），无需手工执行 SQL。
- 升级前务必保留 `data/` 与备份；降级不保证兼容（新 schema 不会被旧版本识别）。

## 6. 备份与恢复

```bash
./backup.sh                          # 产物：backups/oss-backup-<时间戳>/
./backup.sh --out /mnt/backups       # 指定输出目录
./restore.sh backups/oss-backup-20261106-120000 --yes
```

备份内容：

- `data.tar.gz`：数据目录（对象文件 + redb 元数据库）；
- `mysql.sql`：MySQL 后端时额外导出的逻辑备份；
- `manifest.txt` / `SHA256SUMS`：版本与校验信息。

说明：

- `backup.sh` 会在备份期间短暂停止容器（保证 redb 一致），完成后自动恢复运行。
- MySQL 备份优先使用 compose 内的 `mysql` 容器（`docker exec mysqldump`），否则使用本机 `mysqldump`，再否则使用 `mysql:8.0` 临时容器。
- `restore.sh` 不会直接覆盖现有数据：旧目录会重命名为 `data.bak-<时间戳>`。

## 7. 切换后端（redb ↔ MySQL）

### 7.1 切到 MySQL

1. 准备 MySQL 8.0（可用 compose 注释段内置实例，或外部实例）。
2. 用带 mysql 特性的镜像重建：

   ```bash
   ./build.sh -v 1.2.0-mysql -f mysql
   ```

3. 修改 `.env`：

   ```dotenv
   OSS_IMAGE=oss-server:1.2.0-mysql
   OSS_STORAGE_BACKEND=mysql
   OSS_DB_URL=mysql://oss:password@mysql:3306/oss
   CARGO_FEATURES=mysql
   ```

   > DSN 中的密码若含 `@ : /` 等字符需百分号编码，例如 `p@ss` → `p%40ss`。

4. `./deploy.sh --no-build`。首次连接会自动建库建表并写入 schema 版本。

### 7.2 切回 redb

把 `OSS_IMAGE` 换回默认镜像、`OSS_STORAGE_BACKEND=redb`、`OSS_DB_URL=redb:///data/oss.db`，
再 `./deploy.sh --no-build`。注意：**两种后端的元数据不会自动互相迁移**，切换前请先
`./backup.sh`，并明确数据归属。

## 8. Docker Toolbox / 远程 daemon

- 脚本会自动执行 `docker-machine start default` 与 `docker-machine env`。
- Docker Toolbox 的 daemon 在虚拟机内，`deploy.sh` 的健康检查会自动使用
  `DOCKER_HOST` 的 IP（例如 `http://192.168.99.100:8080/healthz`）。
- Docker Toolbox 对 Windows 宿主机目录的挂载有限：建议把 `OSS_DATA_HOST_DIR`
  设为 `/c/Users/<用户>/oss-data`（VirtualBox 默认共享 `C:\Users`），不要使用 E: 盘路径。

## 9. 排错

| 现象 | 处理 |
| --- | --- |
| `未检测到 docker compose` | 安装 Compose 插件，或安装 `docker-compose` 1.20+ |
| `宿主机端口 8080 已被占用` | 修改 `.env` 的 `OSS_PORT`，或停止占用进程（`ss -ltnp` / `netstat -ano`） |
| `数据目录不可写` | 检查挂载路径；Rootless Docker / 显式 `--user` 下执行 `chown -R 10001:10001 <data>`；SELinux 加 `:z` |
| 健康检查超时 | `./logs.sh --tail 200` 查看启动日志；确认 `OSS_ADMIN_TOKEN` 已设置；确认 8080 未被容器内其他进程占用 |
| 启动日志出现自检报告 | 属正常行为；若 `OSS_STARTUP_CHECK_MODE=fix` 会自动修复失效记录/孤儿文件 |
| 磁盘未释放 | 普通删除只进回收站；在后台“回收站”彻底删除，或等待保留期到期由清理任务释放 |
| 管理后台 401 | 令牌错误；重新输入 `.env` 中的 `OSS_ADMIN_TOKEN` |

常用命令：

```bash
./status.sh
./logs.sh -f
docker compose exec oss-server sh
docker compose ps
```

## 10. 运维说明

- **回收站**：普通删除/桶删除/App 删除只把对象移入回收站并保留磁盘文件；
  只有“彻底删除”和保留期到期后的定时清理会删除磁盘文件。
- **定时清理**：清理过期回收站对象、失效记录、孤儿文件、过期 nonce；
  单次运行受 `OSS_CLEANUP_MAX_OBJECTS` / `OSS_CLEANUP_MAX_SECONDS` 限制，
  失败条目保留元数据待下一轮重试。
- **启动自检**：启动时比对元数据与磁盘文件，输出结构化日志；
  结果可在后台“清理与自检”页面查看，也可手动触发。
- 管理后台提供回收站（列表/还原/彻底删除/批量）与清理/自检（状态、手动触发、配置展示）。
