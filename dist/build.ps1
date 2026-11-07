<#
.SYNOPSIS
    构建 OSS Server 镜像（Windows PowerShell 版）。

.EXAMPLE
    .\build.ps1 -Version 1.2.0
    .\build.ps1 -Version 1.2.0 -Features mysql -Push
    .\build.ps1 -Image registry.example.com/team/oss-server -Version 1.2.0
#>
param(
    [Alias("v")]
    [string]$Version = "",
    [Alias("i")]
    [string]$Image = "oss-server",
    [Alias("f")]
    [string]$Features = "",
    [switch]$NoCache,
    [switch]$Push
)

$ErrorActionPreference = "Stop"
$DistDir = $PSScriptRoot
$RepoRoot = Split-Path -Parent $DistDir
$Dockerfile = Join-Path $DistDir "Dockerfile"

function Write-Info($Message) { Write-Host "[INFO] $Message" -ForegroundColor Cyan }
function Write-Warn($Message) { Write-Host "[WARN] $Message" -ForegroundColor Yellow }
function Fail($Message) { Write-Host "[ERROR] $Message" -ForegroundColor Red; exit 1 }

# --- Docker 可用性（兼容 Docker Toolbox） ---
if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Fail "未找到 docker 命令，请先安装 Docker。"
}

docker info *> $null
if ($LASTEXITCODE -ne 0 -and (Get-Command docker-machine -ErrorAction SilentlyContinue)) {
    Write-Warn "Docker daemon 未连接，尝试启动 docker-machine 'default' ..."
    docker-machine start default *> $null
    docker-machine env --shell powershell default | Invoke-Expression
    docker info *> $null
}
if ($LASTEXITCODE -ne 0) {
    Fail "无法连接 Docker daemon，请确认 Docker Desktop 或 docker-machine 已启动。"
}

# --- 默认版本号取自 Cargo.toml ---
if ([string]::IsNullOrWhiteSpace($Version)) {
    $cargoToml = Join-Path $RepoRoot "Cargo.toml"
    $line = Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if ($line -and $line.Matches.Count -gt 0) {
        $Version = $line.Matches[0].Groups[1].Value
    } else {
        $Version = "0.0.0"
    }
}

$Tag = "${Image}:${Version}"
$Latest = "${Image}:latest"

Write-Info "构建镜像：$Tag（同时打标 $Latest）"
Write-Info "构建特性：$(if ([string]::IsNullOrWhiteSpace($Features)) { '<默认 redb>' } else { $Features })"

$dockerArgs = @(
    "build",
    "-f", $Dockerfile,
    "--build-arg", "CARGO_FEATURES=$Features",
    "-t", $Tag,
    "-t", $Latest
)
if ($NoCache) { $dockerArgs += "--no-cache" }
$dockerArgs += $RepoRoot

& docker @dockerArgs
if ($LASTEXITCODE -ne 0) { Fail "镜像构建失败。" }

Write-Info "构建完成：$Tag"

if ($Push) {
    Write-Info "推送镜像 ..."
    docker push $Tag
    if ($LASTEXITCODE -ne 0) { Fail "推送 $Tag 失败。" }
    docker push $Latest
    if ($LASTEXITCODE -ne 0) { Fail "推送 $Latest 失败。" }
    Write-Info "推送完成。"
}

Write-Host ""
Write-Info "下一步：在 .env 中设置 OSS_IMAGE=$Tag，然后执行 docker-compose up -d"
