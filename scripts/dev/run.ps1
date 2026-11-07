<#
.SYNOPSIS
  本地启动 OSS Server（cargo run），自动注入 GNU 工具链并读取 .env.dev。

.EXAMPLE
  .\run.ps1
.EXAMPLE
  .\run.ps1 -Release -Port 18081
.EXAMPLE
  .\run.ps1 -Watch
.EXAMPLE
  .\run.ps1 -Backend mysql
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [ValidateSet('default', 'redb', 'memory', 'mysql')]
    [string]$Backend = 'default',
    [int]$Port = 0,
    [switch]$Watch
)

. (Join-Path $PSScriptRoot 'env.ps1')
$state = Initialize-DevEnvironment -ToolchainMode 'auto'

# 后端选择：命令行 > .env.dev > 默认 redb
$effectiveBackend = $env:OSS_STORAGE_BACKEND
if ($Backend -ne 'default') {
    $env:OSS_STORAGE_BACKEND = $Backend
    $effectiveBackend = $Backend
} elseif (-not $effectiveBackend) {
    $effectiveBackend = 'redb'
    $env:OSS_STORAGE_BACKEND = 'redb'
}

if ($effectiveBackend -eq 'mysql') {
    if (-not $env:OSS_DB_URL -or $env:OSS_DB_URL -notlike 'mysql://*') {
        throw "mysql 后端需要 OSS_DB_URL=mysql://user:pass@host:port/db。请在 .env.dev 中配置，或先设置环境变量。"
    }
} elseif ($effectiveBackend -eq 'redb') {
    if (-not $env:OSS_DB_URL -or $env:OSS_DB_URL -notlike 'redb://*') {
        $env:OSS_DB_URL = 'redb://' + (Join-Path $state.DataDir 'oss.db')
    }
}

# 端口覆盖
if ($Port -gt 0) {
    $env:OSS_HOST = '127.0.0.1:' + $Port
    $env:OSS_PUBLIC_BASE_URL = 'http://127.0.0.1:' + $Port
}

$port = Get-DevPort
$bindHost = $env:OSS_HOST.Substring(0, $env:OSS_HOST.LastIndexOf(':'))
$urlHost = if ($bindHost -in @('0.0.0.0', '::', '[::]')) { '127.0.0.1' } else { $bindHost }

$occupied = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
if ($occupied) {
    $owners = ($occupied | Select-Object -ExpandProperty OwningProcess -Unique) -join ','
    throw "端口 $port 已被进程 $owners 占用。换端口：.\run.ps1 -Port 18081；或结束进程：Stop-Process -Id $owners"
}

$cargoArgs = @('run')
if ($Release) { $cargoArgs += '--release' }
if ($effectiveBackend -eq 'mysql') { $cargoArgs += '--features'; $cargoArgs += 'mysql' }

Write-Host ""
Write-Host "OSS Server 本地开发启动" -ForegroundColor Cyan
Write-Host ("  工具链     : {0}" -f $state.Toolchain)
Write-Host ("  后端       : {0}" -f $effectiveBackend)
Write-Host ("  数据目录   : {0}" -f $state.DataDir)
if ($effectiveBackend -ne 'memory') { Write-Host ("  元数据库   : {0}" -f $env:OSS_DB_URL) }
Write-Host ("  服务地址   : http://{0}:{1}/" -f $urlHost, $port)
Write-Host ("  管理后台   : http://{0}:{1}/admin/" -f $urlHost, $port)
Write-Host ("  健康检查   : http://{0}:{1}/healthz" -f $urlHost, $port)
Write-Host ("  管理 Token : {0}" -f $env:OSS_ADMIN_TOKEN)
Write-Host ("  启动命令   : cargo {0}" -f ($cargoArgs -join ' '))
Write-Host ""

if ($Watch) {
    $watchCmd = Get-Command cargo-watch -ErrorAction SilentlyContinue
    if ($watchCmd) {
        $watchRun = ($cargoArgs -join ' ')
        Write-Host "cargo-watch 已启用，修改源码后自动重启。" -ForegroundColor Green
        & $watchCmd.Source -x $watchRun
        exit $LASTEXITCODE
    }
    Write-Warning "未安装 cargo-watch，退化为普通运行。安装：cargo install cargo-watch"
}

Invoke-Cargo -CargoArgs $cargoArgs
exit $script:LastCargoExit
