<#
.SYNOPSIS
  跑测试（可选 clippy），支持默认后端与 MySQL feature 两种组合。

.EXAMPLE
  .\test.ps1
.EXAMPLE
  .\test.ps1 -Clippy
.EXAMPLE
  .\test.ps1 -Mode mysql
.EXAMPLE
  .\test.ps1 -Mode all -Clippy
#>
[CmdletBinding()]
param(
    [ValidateSet('default', 'mysql', 'all')]
    [string]$Mode = 'default',
    [switch]$Clippy
)

. (Join-Path $PSScriptRoot 'env.ps1')
$state = Initialize-DevEnvironment -ToolchainMode 'auto'

$script:FailedSteps = @()

function Invoke-Step {
    param([string]$Name, [string[]]$Arguments)
    Write-Host ""
    Write-Host ("==> {0}" -f $Name) -ForegroundColor Cyan
    Write-Host ("    cargo {0}" -f ($Arguments -join ' ')) -ForegroundColor DarkGray
    Invoke-Cargo -CargoArgs $Arguments
    $code = $script:LastCargoExit
    if ($code -eq 0) {
        Write-Host ("    [PASS] {0}" -f $Name) -ForegroundColor Green
    } else {
        Write-Host ("    [FAIL] {0} (exit {1})" -f $Name, $code) -ForegroundColor Red
        $script:FailedSteps += $Name
    }
    return $code
}

Write-Host ""
Write-Host "OSS Server 测试" -ForegroundColor Cyan
Write-Host ("  工具链: {0}" -f $state.Toolchain)

if ($Mode -ne 'default') {
    if ($env:OSS_MYSQL_TEST_URL) {
        Write-Host ("  MySQL 测试库: {0}" -f ($env:OSS_MYSQL_TEST_URL -replace '://[^@]*@', '://***@'))
    } else {
        Write-Host "  MySQL 测试库: 未设置 OSS_MYSQL_TEST_URL，将使用测试内置默认 DSN（需本地/内网 MySQL 可达）" -ForegroundColor Yellow
    }
}

if ($Mode -eq 'default' -or $Mode -eq 'all') {
    [void](Invoke-Step '测试（默认后端）' @('test', '--all-targets'))
    if ($Clippy) {
        [void](Invoke-Step 'Clippy（默认后端）' @('clippy', '--all-targets', '--', '-D', 'warnings'))
    }
}

if ($Mode -eq 'mysql' -or $Mode -eq 'all') {
    [void](Invoke-Step '测试（mysql feature）' @('test', '--features', 'mysql', '--all-targets'))
    if ($Clippy) {
        [void](Invoke-Step 'Clippy（mysql feature）' @('clippy', '--features', 'mysql', '--all-targets', '--', '-D', 'warnings'))
    }
}

Write-Host ""
if ($script:FailedSteps.Count -gt 0) {
    Write-Host ("测试结束: {0} 个步骤失败 -> {1}" -f $script:FailedSteps.Count, ($script:FailedSteps -join ', ')) -ForegroundColor Red
    exit 1
}
Write-Host "测试结束: 全部通过" -ForegroundColor Green
exit 0
