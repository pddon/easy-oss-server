<#
.SYNOPSIS
  开发环境公共库：定位 Rust GNU 工具链、注入 PATH、加载 .env.dev、补齐开发默认值。

.DESCRIPTION
  被 scripts/dev 下的其它脚本 dot-source 使用，也可以直接执行查看当前开发环境。

  这台机器没有 Visual Studio（MSVC 缺 link.exe），cmake/httparse 之类的构建脚本
  依赖 dlltool/as/ld，必须先安装 stable-x86_64-pc-windows-gnu 工具链，
  并把 toolchain\bin 与 toolchain\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained
  注入 PATH。本脚本自动完成这几件事，避免每次手工设置。

.EXAMPLE
  . .\scripts\dev\env.ps1; Initialize-DevEnvironment
.EXAMPLE
  powershell -File .\scripts\dev\env.ps1
#>
[CmdletBinding()]
param(
    # auto：默认走 GNU，只有默认 toolchain 是 MSVC 且能找到 link.exe 时才保留 MSVC；
    # gnu：强制 GNU 工具链；msvc：强制 MSVC（不可用时回退 GNU）。
    [ValidateSet('auto', 'gnu', 'msvc')]
    [string]$Toolchain = 'auto',
    [switch]$Quiet
)

$script:Quiet = $Quiet.IsPresent

# 项目根目录：scripts\dev\env.ps1 -> 上两级
$script:ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$script:DevEnvFile = Join-Path $script:ProjectRoot '.env.dev'
$script:DevDataDirName = 'dev-data'
$script:DevState = $null

function Write-DevInfo {
    param([string]$Message)
    if (-not $script:Quiet) { Write-Host "  $Message" -ForegroundColor DarkGray }
}

function Write-DevWarn {
    param([string]$Message)
    Write-Host "  [WARN] $Message" -ForegroundColor Yellow
}

# 查找已安装的 GNU 工具链目录（优先 stable，其次最新安装的）。
function Find-GnuToolchain {
    $toolchainsDir = Join-Path $env:USERPROFILE '.rustup\toolchains'
    if (-not (Test-Path $toolchainsDir)) { return $null }
    $candidates = Get-ChildItem $toolchainsDir -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '*x86_64-pc-windows-gnu*' }
    if (-not $candidates) { return $null }
    $stable = $candidates | Where-Object { $_.Name -like 'stable-*' } | Select-Object -First 1
    $picked = if ($stable) { $stable } else { $candidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1 }
    if (Test-Path (Join-Path $picked.FullName 'bin\cargo.exe')) { return $picked.FullName }
    return $null
}

function Test-MsvcReady {
    if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) { return $false }
    return $true
}

# 确保 rustup/cargo shim 目录在 PATH 中（部分终端不会自动加载 ~\.cargo\bin）。
function Enable-CargoBinPath {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (-not (Test-Path $cargoBin)) { return }
    $entries = $env:PATH -split ';'
    if ($entries -notcontains $cargoBin) { $env:PATH = $env:PATH + ';' + $cargoBin }
}

function Get-RustupPath {
    $cmd = Get-Command rustup -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $fallback = Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'
    if (Test-Path $fallback) { return $fallback }
    return $null
}

function Get-ActiveToolchain {
    $rustup = Get-RustupPath
    if (-not $rustup) { return '' }
    $out = & $rustup show active-toolchain 2>$null
    if (-not $out) { return '' }
    return ($out | Select-Object -First 1).ToString()
}

# 把 GNU 工具链的可执行目录注入当前进程 PATH（bin 在前，self-contained 在后）。
function Enable-GnuToolchain {
    param([string]$GnuPath)
    $bin = Join-Path $GnuPath 'bin'
    $selfContained = Join-Path $GnuPath 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
    $entries = @($bin)
    if (Test-Path $selfContained) { $entries += $selfContained }
    $env:PATH = ($entries -join ';') + ';' + $env:PATH
    # rustup shim（以及 cargo 的子进程）会读取该变量，确保它们不落回默认的 MSVC。
    $env:RUSTUP_TOOLCHAIN = Split-Path $GnuPath -Leaf
    return $entries
}

function Initialize-DevToolchain {
    param([string]$Mode = 'auto')
    Enable-CargoBinPath
    $gnu = Find-GnuToolchain
    $active = Get-ActiveToolchain

    switch ($Mode) {
        'gnu' {
            if (-not $gnu) {
                throw "未找到 GNU 工具链。请先执行：rustup toolchain install stable-x86_64-pc-windows-gnu"
            }
            $entries = Enable-GnuToolchain $gnu
            Write-DevInfo "使用 GNU 工具链：$gnu"
            return @{ Mode = 'gnu'; GnuPath = $gnu; PathEntries = $entries }
        }
        'msvc' {
            if (Test-MsvcReady) {
                Write-DevInfo "使用 MSVC 工具链（override），当前：$active"
                return @{ Mode = 'msvc'; GnuPath = $null; PathEntries = @() }
            }
            Write-DevWarn "指定了 msvc，但找不到 link.exe；回退到 GNU。"
            if (-not $gnu) { throw "MSVC 与 GNU 工具链都不可用，请安装 stable-x86_64-pc-windows-gnu。" }
            $entries = Enable-GnuToolchain $gnu
            return @{ Mode = 'gnu'; GnuPath = $gnu; PathEntries = $entries }
        }
        default {
            if (Test-MsvcReady -and $active -like '*msvc*') {
                Write-DevInfo "检测到 MSVC + link.exe，沿用默认工具链（$active）"
                return @{ Mode = 'msvc'; GnuPath = $null; PathEntries = @() }
            }
            if ($gnu) {
                $entries = Enable-GnuToolchain $gnu
                Write-DevInfo "已注入 GNU 工具链 PATH：$gnu"
                return @{ Mode = 'gnu'; GnuPath = $gnu; PathEntries = $entries }
            }
            Write-DevWarn "未找到 GNU 工具链，也没有可用的 MSVC；cargo 可能无法链接。"
            return @{ Mode = 'none'; GnuPath = $null; PathEntries = @() }
        }
    }
}

# 读取 KEY=VALUE 形式的 .env.dev；已存在的环境变量优先（便于命令行覆盖）。
function Import-DevEnv {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return $false }
    foreach ($line in Get-Content -LiteralPath $Path) {
        $text = $line.Trim()
        if ($text.Length -eq 0 -or $text.StartsWith('#')) { continue }
        if ($text -notmatch '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$') { continue }
        $key = $matches[1]
        $value = $matches[2].Trim()
        if ($value.Length -ge 2) {
            $first = $value.Substring(0, 1)
            $last = $value.Substring($value.Length - 1, 1)
            if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
                $value = $value.Substring(1, $value.Length - 2)
            }
        }
        if (-not (Test-Path "Env:$key")) { Set-Item "Env:$key" $value }
    }
    return $true
}

# 补齐 .env.dev 未覆盖的开发默认值（不覆盖已设置的值）。
function Set-DevDefaults {
    $root = $script:ProjectRoot
    $defaultDataDir = Join-Path $root $script:DevDataDirName

    if (-not $env:OSS_DATA_DIR) { $env:OSS_DATA_DIR = $defaultDataDir }
    $dataDir = if ([System.IO.Path]::IsPathRooted($env:OSS_DATA_DIR)) {
        [System.IO.Path]::GetFullPath($env:OSS_DATA_DIR)
    } else {
        [System.IO.Path]::GetFullPath((Join-Path $root $env:OSS_DATA_DIR))
    }
    if (-not (Test-Path $dataDir)) { New-Item -ItemType Directory -Path $dataDir -Force | Out-Null }

    if (-not $env:OSS_STORAGE_BACKEND) { $env:OSS_STORAGE_BACKEND = 'redb' }
    if (-not $env:OSS_DB_URL) { $env:OSS_DB_URL = 'redb://' + (Join-Path $dataDir 'oss.db') }
    if (-not $env:OSS_HOST) { $env:OSS_HOST = '127.0.0.1:18080' }
    if (-not $env:OSS_ADMIN_TOKEN) { $env:OSS_ADMIN_TOKEN = 'dev-admin-token' }
    if (-not $env:OSS_MASTER_KEY) { $env:OSS_MASTER_KEY = 'dev-master-key-0123456789abcdef' }
    if (-not $env:OSS_PUBLIC_BASE_URL) { $env:OSS_PUBLIC_BASE_URL = 'http://' + $env:OSS_HOST }
    if (-not $env:RUST_LOG) { $env:RUST_LOG = 'oss_server=info,tower_http=info' }

    return @{
        ProjectRoot = $root
        DataDir     = $dataDir
        DbUrl       = $env:OSS_DB_URL
        Backend     = $env:OSS_STORAGE_BACKEND
        Host        = $env:OSS_HOST
        AdminToken  = $env:OSS_ADMIN_TOKEN
        PublicUrl   = $env:OSS_PUBLIC_BASE_URL
    }
}

# 一次性初始化：工具链 + .env.dev + 默认值。
function Initialize-DevEnvironment {
    param([string]$ToolchainMode = 'auto')
    $toolchain = Initialize-DevToolchain -Mode $ToolchainMode
    $envLoaded = Import-DevEnv $script:DevEnvFile
    $state = Set-DevDefaults
    $state.Toolchain = $toolchain.Mode
    $state.GnuPath = $toolchain.GnuPath
    $state.PathEntries = $toolchain.PathEntries
    $state.EnvFileLoaded = $envLoaded
    $script:DevState = $state
    return $state
}

# 用初始化后的工具链调用 cargo（显式路径，避免 PowerShell 命令缓存命中 rustup shim）。
# 输出实时写到控制台，退出码放在 $script:LastCargoExit（函数本身不返回其它对象）。
function Invoke-Cargo {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArgs)
    if ($script:DevState -and $script:DevState.GnuPath) {
        $cargoExe = Join-Path $script:DevState.GnuPath 'bin\cargo.exe'
    } else {
        $cargoExe = (Get-Command cargo -ErrorAction Stop).Source
    }
    & $cargoExe @CargoArgs | Out-Host
    $script:LastCargoExit = $LASTEXITCODE
}

function Get-DevPort {
    param([string]$Host2 = $env:OSS_HOST)
    if (-not $Host2) { return 18080 }
    $idx = $Host2.LastIndexOf(':')
    if ($idx -lt 0) { return 18080 }
    $portText = $Host2.Substring($idx + 1)
    $port = 0
    if ([int]::TryParse($portText, [ref]$port)) { return $port }
    return 18080
}

# 直接执行时打印摘要；被 dot-source 时只定义函数。
if ($MyInvocation.InvocationName -ne '.') {
    $state = Initialize-DevEnvironment -ToolchainMode $Toolchain
    Write-Host ""
    Write-Host "OSS Server 开发环境" -ForegroundColor Cyan
    Write-Host ("  项目根目录 : {0}" -f $state.ProjectRoot)
    Write-Host ("  工具链     : {0}{1}" -f $state.Toolchain, $(if ($state.GnuPath) { " ($($state.GnuPath))" } else { '' }))
    Write-Host ("  .env.dev   : {0}" -f $(if ($state.EnvFileLoaded) { $script:DevEnvFile } else { '未找到（使用默认值）' }))
    Write-Host ("  数据目录   : {0}" -f $state.DataDir)
    Write-Host ("  元数据库   : {0}" -f $state.DbUrl)
    Write-Host ("  监听地址   : {0}" -f $state.Host)
    Write-Host ("  管理 Token : {0}" -f $state.AdminToken)
    Write-Host ("  后台地址   : {0}/admin/" -f $state.PublicUrl)
    Write-Host ""
}
