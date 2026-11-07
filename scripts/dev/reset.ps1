<#
.SYNOPSIS
  清空本地开发数据目录（默认交互确认，-Force 跳过）。

.DESCRIPTION
  只允许删除项目根目录下的 dev-data 目录。若 OSS_DATA_DIR 指向其它路径，
  脚本会直接拒绝，避免误删用户数据或生产目录。

.EXAMPLE
  .\reset.ps1
.EXAMPLE
  .\reset.ps1 -Force
#>
[CmdletBinding()]
param(
    [switch]$Force
)

. (Join-Path $PSScriptRoot 'env.ps1')
$state = Initialize-DevEnvironment -ToolchainMode 'auto'

$root = $state.ProjectRoot
$dataDir = $state.DataDir
$leaf = Split-Path -Leaf $dataDir

# 安全闸门：必须是项目根目录下的 dev-data，且不能是根目录本身。
$insideRoot = $dataDir.StartsWith($root + [System.IO.Path]::DirectorySeparatorChar)
if (-not $insideRoot -or $leaf -ne 'dev-data') {
    Write-Host ""
    Write-Host "拒绝删除：目标不是项目内的开发数据目录" -ForegroundColor Red
    Write-Host ("  目标路径 : {0}" -f $dataDir)
    Write-Host ("  允许范围 : {0}\dev-data" -f $root)
    Write-Host "  OSS_DATA_DIR 指向了自定义路径，脚本不会碰它。如需清理请手动处理。" -ForegroundColor Yellow
    exit 1
}

Write-Host ""
Write-Host "开发数据目录清理" -ForegroundColor Cyan
Write-Host ("  项目根目录 : {0}" -f $root)
Write-Host ("  将删除     : {0}" -f $dataDir)

if (Test-Path $dataDir) {
    $files = Get-ChildItem -LiteralPath $dataDir -Recurse -File -ErrorAction SilentlyContinue
    $total = ($files | Measure-Object -Property Length -Sum).Sum
    if (-not $total) { $total = 0 }
    Write-Host ("  文件数量   : {0}" -f $files.Count)
    Write-Host ("  占用空间   : {0:N2} MB" -f ($total / 1MB))
} else {
    Write-Host "  目录当前不存在，将直接重建空目录" -ForegroundColor DarkGray
}

$port = Get-DevPort
$occupied = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
if ($occupied) {
    Write-Host ("  注意：端口 {0} 仍被占用，服务可能正在运行，删除数据库文件可能导致运行中的服务报错。" -f $port) -ForegroundColor Yellow
}

Write-Host ""
if (-not $Force) {
    $answer = Read-Host "确认删除以上目录？输入 yes 继续"
    if ($answer -ne 'yes') {
        Write-Host "已取消" -ForegroundColor Yellow
        exit 1
    }
}

try {
    if (Test-Path $dataDir) { Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction Stop }
    New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
    Write-Host ("已清理并重建: {0}" -f $dataDir) -ForegroundColor Green
    exit 0
} catch {
    Write-Host ("清理失败: {0}" -f $_.Exception.Message) -ForegroundColor Red
    exit 1
}
