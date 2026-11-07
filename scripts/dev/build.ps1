<#
.SYNOPSIS
  Release 构建，可选启用 mysql feature，并输出产物路径与体积。

.EXAMPLE
  .\build.ps1
.EXAMPLE
  .\build.ps1 -Mysql
.EXAMPLE
  .\build.ps1 -Features mysql
#>
[CmdletBinding()]
param(
    [ValidateSet('', 'mysql')]
    [string]$Features = '',
    [switch]$Mysql
)

. (Join-Path $PSScriptRoot 'env.ps1')
$state = Initialize-DevEnvironment -ToolchainMode 'auto'

if ($Mysql) { $Features = 'mysql' }

$cargoArgs = @('build', '--release')
if ($Features) { $cargoArgs += '--features'; $cargoArgs += $Features }

Write-Host ""
Write-Host "OSS Server Release 构建" -ForegroundColor Cyan
Write-Host ("  工具链: {0}" -f $state.Toolchain)
Write-Host ("  feature: {0}" -f $(if ($Features) { $Features } else { '默认（无 mysql）' }))
Write-Host ("  命令  : cargo {0}" -f ($cargoArgs -join ' '))
Write-Host ""

Invoke-Cargo -CargoArgs $cargoArgs
$code = $script:LastCargoExit
if ($code -ne 0) {
    Write-Host ("构建失败（exit {0}）" -f $code) -ForegroundColor Red
    exit $code
}

$exe = Join-Path $state.ProjectRoot 'target\release\oss-server.exe'
if (Test-Path $exe) {
    $size = (Get-Item $exe).Length
    Write-Host ""
    Write-Host "构建成功" -ForegroundColor Green
    Write-Host ("  产物: {0}" -f $exe)
    Write-Host ("  体积: {0:N2} MB" -f ($size / 1MB))
} else {
    Write-Host ("构建命令成功但未找到产物: {0}" -f $exe) -ForegroundColor Yellow
    exit 1
}
exit 0
