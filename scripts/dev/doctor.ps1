<#
.SYNOPSIS
  环境体检：工具链、PATH、端口、数据目录、Docker 等逐项检查并给出修复建议。

.EXAMPLE
  powershell -File .\scripts\dev\doctor.ps1
#>
[CmdletBinding()]
param()

. (Join-Path $PSScriptRoot 'env.ps1')
$state = Initialize-DevEnvironment -ToolchainMode 'auto'

$script:FailCount = 0
$script:WarnCount = 0

function Report-Item {
    param(
        [string]$Name,
        [ValidateSet('PASS', 'FAIL', 'WARN', 'INFO')]
        [string]$Status,
        [string]$Detail = '',
        [string]$Fix = ''
    )
    $color = switch ($Status) {
        'PASS' { 'Green' }
        'FAIL' { 'Red' }
        'WARN' { 'Yellow' }
        default { 'Gray' }
    }
    if ($Status -eq 'FAIL') { $script:FailCount++ }
    if ($Status -eq 'WARN') { $script:WarnCount++ }
    $line = "[{0}] {1}" -f $Status.PadRight(4), $Name
    if ($Detail) { $line += " - $Detail" }
    Write-Host $line -ForegroundColor $color
    if ($Fix -and $Status -ne 'PASS' -and $Status -ne 'INFO') {
        Write-Host ("       修复建议: {0}" -f $Fix) -ForegroundColor DarkGray
    }
}

Write-Host ""
Write-Host "OSS Server 开发环境体检" -ForegroundColor Cyan
Write-Host ("项目根目录: {0}" -f $state.ProjectRoot)
Write-Host ("工具链模式: {0}" -f $state.Toolchain)
Write-Host ""

# 1. rustup
$rustupExe = Get-RustupPath
if ($rustupExe) {
    $rustupVersion = (& $rustupExe --version 2>$null | Select-Object -First 1)
    Report-Item 'rustup' 'PASS' $rustupVersion
} else {
    Report-Item 'rustup' 'FAIL' '未找到 rustup' '安装 Rust：https://rustup.rs/，然后执行 rustup toolchain install stable-x86_64-pc-windows-gnu'
}

# 2. cargo / rustc（当前进程实际会用到的那一套）
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargo) {
    $cargoVersion = (& $cargo.Source --version 2>$null)
    Report-Item 'cargo' 'PASS' $cargoVersion
} else {
    Report-Item 'cargo' 'FAIL' 'PATH 中没有 cargo' '确认 $env:USERPROFILE\.cargo\bin 已加入 PATH，或重新打开终端'
}
$rustc = Get-Command rustc -ErrorAction SilentlyContinue
if ($rustc) {
    $rustcVersion = (& $rustc.Source --version 2>$null)
    Report-Item 'rustc' 'PASS' $rustcVersion
} else {
    Report-Item 'rustc' 'FAIL' 'PATH 中没有 rustc' '与 cargo 相同，检查 Rust 安装与 PATH'
}

# 3. 默认 / 活动 toolchain
$active = Get-ActiveToolchain
if ($active) {
    Report-Item '默认 toolchain' 'PASS' $active
} else {
    Report-Item '默认 toolchain' 'WARN' '未配置默认 toolchain' 'rustup default stable-x86_64-pc-windows-gnu'
}

# 4. GNU 工具链
$gnu = Find-GnuToolchain
if ($gnu) {
    $gnuRustc = Join-Path $gnu 'bin\rustc.exe'
    if (Test-Path $gnuRustc) {
        $gnuVersion = (& $gnuRustc --version 2>$null)
        Report-Item 'GNU 工具链' 'PASS' ("{0} ({1})" -f $gnuVersion, $gnu)
    } else {
        Report-Item 'GNU 工具链' 'FAIL' "目录存在但缺少 rustc.exe: $gnu" 'rustup toolchain install stable-x86_64-pc-windows-gnu'
    }
} else {
    Report-Item 'GNU 工具链' 'FAIL' '未安装 x86_64-pc-windows-gnu' 'rustup toolchain install stable-x86_64-pc-windows-gnu（本机没有 MSVC，必须用 GNU）'
}

# 5. PATH 注入
if ($state.GnuPath) {
    $binDir = Join-Path $state.GnuPath 'bin'
    $selfContained = Join-Path $state.GnuPath 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
    $pathEntries = $env:PATH -split ';'
    $hasBin = $pathEntries -contains $binDir
    $hasSelf = $pathEntries -contains $selfContained
    if ($hasBin -and $hasSelf) {
        Report-Item 'PATH 注入' 'PASS' 'toolchain\bin 与 self-contained 均已注入'
    } elseif ($hasBin) {
        Report-Item 'PATH 注入' 'WARN' '已注入 bin，但缺少 self-contained' '确认 self-contained 目录存在；脚本会自动注入，若缺失请检查工具链完整性'
    } else {
        Report-Item 'PATH 注入' 'FAIL' '当前进程 PATH 中没有 GNU toolchain\bin' '先 dot-source scripts\dev\env.ps1 再执行命令，或直接使用本目录脚本'
    }
} else {
    Report-Item 'PATH 注入' 'WARN' '未启用 GNU 工具链，跳过 PATH 检查' '安装 GNU 工具链后重试'
}

# 6. 链接/汇编工具（dlltool、as、ld）
$requiredTools = @('dlltool.exe', 'as.exe', 'ld.exe')
$missingTools = @()
foreach ($tool in $requiredTools) {
    $found = $false
    if ($state.GnuPath) {
        $selfContained = Join-Path $state.GnuPath 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
        $candidates = @(
            (Join-Path $state.GnuPath ("bin\$tool")),
            (Join-Path $selfContained $tool)
        )
        foreach ($c in $candidates) {
            if (Test-Path $c) { $found = $true; break }
        }
    }
    if (-not $found -and (Get-Command $tool -ErrorAction SilentlyContinue)) { $found = $true }
    if (-not $found) { $missingTools += $tool }
}
if ($missingTools.Count -eq 0) {
    Report-Item 'GNU 链接工具' 'PASS' 'dlltool / as / ld 均可用'
} else {
    Report-Item 'GNU 链接工具' 'FAIL' ("缺少: {0}" -f ($missingTools -join ', ')) '重新安装 GNU 工具链：rustup toolchain uninstall stable-x86_64-pc-windows-gnu; rustup toolchain install stable-x86_64-pc-windows-gnu'
}

# 7. 开发端口
$port = Get-DevPort
$portInUse = $null
try {
    $portInUse = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
} catch {
    $portInUse = $null
}
if (-not $portInUse) {
    Report-Item '开发端口' 'PASS' ("{0} 未被占用" -f $port)
} else {
    $owners = ($portInUse | Select-Object -ExpandProperty OwningProcess -Unique) -join ','
    Report-Item '开发端口' 'FAIL' ("{0} 已被进程 {1} 占用" -f $port, $owners) ("换端口：.\run.ps1 -Port 18081；或结束占用进程：Stop-Process -Id {0}" -f $owners)
}

# 8. 数据目录可写
try {
    if (-not (Test-Path $state.DataDir)) { New-Item -ItemType Directory -Path $state.DataDir -Force | Out-Null }
    $probe = Join-Path $state.DataDir ('.doctor-probe-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    Set-Content -LiteralPath $probe -Value 'ok' -Encoding ASCII -ErrorAction Stop
    Remove-Item -LiteralPath $probe -Force -ErrorAction Stop
    Report-Item '数据目录' 'PASS' ("可写: {0}" -f $state.DataDir)
} catch {
    Report-Item '数据目录' 'FAIL' ("不可写: {0} ({1})" -f $state.DataDir, $_.Exception.Message) '检查目录权限；或在 .env.dev 里把 OSS_DATA_DIR 指向可写目录'
}

# 9. .env.dev
if ($state.EnvFileLoaded) {
    Report-Item '.env.dev' 'PASS' $script:DevEnvFile
} else {
    Report-Item '.env.dev' 'WARN' '未找到 .env.dev，使用脚本内置默认值' '复制 .env.dev.example 为 .env.dev 后按需修改'
}

# 10. cargo-watch（可选）
$hasWatch = Get-Command cargo-watch -ErrorAction SilentlyContinue
if ($hasWatch) {
    Report-Item 'cargo-watch' 'INFO' '已安装，run.ps1 -Watch 可用'
} else {
    Report-Item 'cargo-watch' 'INFO' '未安装（可选）' 'cargo install cargo-watch'
}

# 11. Docker（可选）
$docker = Get-Command docker -ErrorAction SilentlyContinue
if ($docker) {
    $dockerVersion = (& $docker.Source --version 2>$null)
    $daemon = ''
    try {
        $daemon = (& $docker.Source version --format '{{.Server.Version}}' 2>$null)
    } catch {
        $daemon = ''
    }
    if ($daemon) {
        Report-Item 'Docker' 'INFO' ("{0}，daemon {1}" -f $dockerVersion, $daemon)
    } elseif (Get-Command docker-machine -ErrorAction SilentlyContinue) {
        Report-Item 'Docker' 'INFO' ("{0}，Docker Toolbox 未激活" -f $dockerVersion, $daemon) 'docker-machine env --shell powershell | Invoke-Expression 后再试'
    } else {
        Report-Item 'Docker' 'INFO' ("{0}，daemon 未连接（开发非必需）" -f $dockerVersion)
    }
} else {
    Report-Item 'Docker' 'INFO' '未安装（开发非必需，仅部署需要）'
}

Write-Host ""
if ($script:FailCount -gt 0) {
    Write-Host ("体检结果: {0} 项失败, {1} 项警告" -f $script:FailCount, $script:WarnCount) -ForegroundColor Red
    exit 1
} elseif ($script:WarnCount -gt 0) {
    Write-Host ("体检结果: 全部必需项通过, {0} 项警告" -f $script:WarnCount) -ForegroundColor Yellow
    exit 0
} else {
    Write-Host "体检结果: 全部通过" -ForegroundColor Green
    exit 0
}
