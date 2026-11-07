# Windows 本地开发脚本

本目录是**仓库内开发工具**，不随 `dist/` 部署包分发。目标：在没装 Visual Studio、MSVC 缺 `link.exe` 的 Windows 机器上，用 `stable-x86_64-pc-windows-gnu` 直接调试本项目，无需每次手工配置 PATH。

## 前置要求

- Windows PowerShell 5.1（系统自带）
- Rust（rustup）：`rustup toolchain install stable-x86_64-pc-windows-gnu`
- 可选：`cargo-watch`（`run.ps1 -Watch`）、Docker（看 `doctor.ps1` 信息项）、MySQL（仅 `test.ps1 -Mode mysql`）

首次使用建议先跑一次 `doctor.ps1`，并按提示修复 FAIL 项。

## 脚本清单

| 文件 | 作用 |
| --- | --- |
| `env.ps1` | 公共库：定位 GNU 工具链并注入 PATH（`bin` + `lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained`）、加载 `.env.dev`、补齐开发默认值。直接执行可查看当前环境摘要 |
| `doctor.ps1` | 环境体检：rustup/cargo/rustc、默认 toolchain、GNU 工具链、PATH 注入、dlltool/as/ld、端口占用、数据目录可写、`.env.dev`、cargo-watch、Docker，逐项 PASS/FAIL/WARN 并给修复建议；有 FAIL 时退出码为 1 |
| `run.ps1` | `cargo run` 起服务；`-Release`、`-Backend redb\|memory\|mysql`、`-Port 18081`、`-Watch`（装了 cargo-watch 才生效，否则提示并退化为普通运行） |
| `test.ps1` | 跑测试：`-Mode default\|mysql\|all`，`-Clippy` 同时跑 clippy（`-D warnings`）；mysql 模式读取 `OSS_MYSQL_TEST_URL` |
| `build.ps1` | Release 构建，`-Mysql` 或 `-Features mysql` 启用 mysql feature，输出产物路径与体积 |
| `reset.ps1` | 清空**项目根部 `dev-data`** 开发数据目录；默认交互确认，`-Force` 跳过；删除前打印绝对路径与占用 |
| `*.cmd` | 各脚本的双击包装（`env.cmd`/`doctor.cmd`/`run.cmd`/`test.cmd`/`build.cmd`/`reset.cmd`），资源管理器双击即可运行 |
| `.env.dev.example` | 开发变量模板，变量名与 `dist/.env.example` 一致 |

## 典型用法

```powershell
# 0) 首次准备：复制配置模板（可选，不复制则用脚本内置默认值）
Copy-Item scripts\dev\.env.dev.example .env.dev

# 1) 体检
powershell -ExecutionPolicy Bypass -File scripts\dev\doctor.ps1

# 2) 起服务（默认 127.0.0.1:18080，redb，数据在 <项目根>\dev-data）
powershell -ExecutionPolicy Bypass -File scripts\dev\run.ps1

# 3) 换端口 / 换后端 / 热重载
powershell -ExecutionPolicy Bypass -File scripts\dev\run.ps1 -Port 18081
powershell -ExecutionPolicy Bypass -File scripts\dev\run.ps1 -Backend memory
powershell -ExecutionPolicy Bypass -File scripts\dev\run.ps1 -Watch

# 4) 测试
powershell -ExecutionPolicy Bypass -File scripts\dev\test.ps1
powershell -ExecutionPolicy Bypass -File scripts\dev\test.ps1 -Mode all -Clippy

# 5) Release 构建
powershell -ExecutionPolicy Bypass -File scripts\dev\build.ps1 -Mysql

# 6) 清空开发数据（交互确认；CI/脚本里用 -Force）
powershell -ExecutionPolicy Bypass -File scripts\dev\reset.ps1 -Force
```

启动后访问：

- 服务根：`http://127.0.0.1:18080/`
- 管理后台：`http://127.0.0.1:18080/admin/`（默认 token `dev-admin-token`，可在 `.env.dev` 改）
- 健康检查：`http://127.0.0.1:18080/healthz`

## 默认值（env.ps1 补齐，均不覆盖已有环境变量）

| 变量 | 默认值 |
| --- | --- |
| `OSS_HOST` | `127.0.0.1:18080` |
| `OSS_DATA_DIR` | `<项目根>\dev-data` |
| `OSS_STORAGE_BACKEND` | `redb` |
| `OSS_DB_URL` | `redb://<项目根>\dev-data\oss.db` |
| `OSS_ADMIN_TOKEN` | `dev-admin-token` |
| `OSS_MASTER_KEY` | `dev-master-key-0123456789abcdef` |
| `OSS_PUBLIC_BASE_URL` | `http://<OSS_HOST>` |
| `RUST_LOG` | `oss_server=info,tower_http=info` |

`.env.dev` 只在变量尚未存在时生效，因此命令行临时覆盖优先，例如：

```powershell
$env:OSS_STORAGE_BACKEND='memory'; .\run.ps1
```

## 常见报错与处理

**找不到 dlltool.exe / as.exe / ld.exe，或 httparse 的 rustc 探测失败**

原因：没有把 GNU 工具链的 `bin` 与 `self-contained` 注入 PATH，cargo 落到了 MSVC 或找不到链接器。

处理：

1. 确认工具链已安装：`rustup toolchain list` 应包含 `stable-x86_64-pc-windows-gnu`；
2. 用本目录脚本启动（`env.ps1` 会自动注入 PATH），或直接执行 `powershell -File scripts\dev\env.ps1` 看摘要；
3. `doctor.ps1` 的「GNU 工具链 / PATH 注入 / GNU 链接工具」三项应为 PASS；
4. 工具链损坏时重装：`rustup toolchain uninstall stable-x86_64-pc-windows-gnu` 后重新 install。

**端口被占用（`run.ps1` 直接报错退出）**

处理：`Get-NetTCPConnection -LocalPort 18080 -State Listen` 看占用进程 → `Stop-Process -Id <pid>`，或换端口 `-Port 18081` 并同步改 `.env.dev` 的 `OSS_PUBLIC_BASE_URL`。

**MySQL 连不上（`test.ps1 -Mode mysql` 失败）**

处理：检查 `.env.dev` 或环境变量 `OSS_MYSQL_TEST_URL`（形如 `mysql://user:pass@host:3306/oss_server_test`），确认账号密码中的特殊字符已 URL 编码、网络可达、目标库存在。不设置时测试会用内置默认 DSN（`192.168.56.100:3306/oss_server_test`），该库不可达则测试失败。

**担心误删数据（`reset.ps1`）**

`reset.ps1` 只允许删除「项目根目录下、目录名为 `dev-data`」的路径；若 `OSS_DATA_DIR` 指向别处（例如生产数据目录或自定义路径），脚本会拒绝执行并打印允许范围。删除前会显示绝对路径、文件数与占用空间，默认需要输入 `yes` 确认。

**PowerShell 执行策略报错（无法加载脚本）**

处理：使用本文档中的 `powershell -ExecutionPolicy Bypass -File ...` 形式，或双击对应 `.cmd`（已内置该参数）。组织策略强制限制时请让管理员放行。

**服务起不来但没有日志**

检查 `doctor.ps1` 的「数据目录」是否 PASS；若 redb 数据库文件被其它进程占用（例如上一次服务未退出），结束进程后重试。注意不要手工删除正在使用的 `dev-data\oss.db`。
