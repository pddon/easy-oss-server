# Start a real OSS server instance and run the Java SDK integration tests.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-integration.ps1
# If the server is already running: add -SkipStart
param(
    [int]$Port = 18101,
    [string]$AdminToken = "java-sdk-it-admin-token",
    [switch]$SkipStart,
    [switch]$SkipUnitTests
)

$ErrorActionPreference = 'Stop'
$ServerRoot = 'E:\ai_code_proj\oss-server'
$ClientDir = Join-Path $ServerRoot 'clients\java'
$DataDir = Join-Path $env:TEMP 'oss-server-java-data'
$LogDir = Join-Path $env:TEMP 'oss-server-java-logs'
$BuildDir = Join-Path $env:TEMP 'oss-server-build-java'
$Endpoint = "http://127.0.0.1:$Port"
$serverProcess = $null
$CargoExe = Join-Path $env:USERPROFILE '.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe'

function Test-S3Binary([string]$exe) {
    # The frozen target/release binary may predate the S3 implementation, so require the
    # AWS SigV4 marker to be present in the image.
    if (-not (Test-Path $exe)) { return $false }
    findstr /C:"AWS4-HMAC-SHA256" $exe > $null 2>&1
    return ($LASTEXITCODE -eq 0)
}

function Resolve-ServerBinary {
    $candidates = @(
        (Join-Path $ServerRoot 'target\release\oss-server.exe'),
        (Join-Path $BuildDir 'release\oss-server.exe'),
        (Join-Path $env:TEMP 'oss-server-build-py\release\oss-server.exe'),
        (Join-Path $ServerRoot 'target\debug\oss-server.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-S3Binary $candidate) { return $candidate }
    }
    return $null
}

function Test-PortOpen([int]$p) {
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $client.Connect('127.0.0.1', $p)
        $client.Close()
        return $true
    } catch {
        return $false
    }
}

function Wait-Health([int]$timeoutSeconds = 60) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $response = Invoke-WebRequest -Uri "$Endpoint/healthz" -UseBasicParsing -TimeoutSec 3
            if ($response.StatusCode -eq 200) { return $true }
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }
    return $false
}

function Stop-Server {
    if ($null -ne $serverProcess) {
        try {
            if (-not $serverProcess.HasExited) {
                Stop-Process -Id $serverProcess.Id -Force
                $serverProcess.WaitForExit(5000) | Out-Null
            }
        } catch {
            Write-Warning "Failed to stop server process: $($_.Exception.Message)"
        }
        $serverProcess = $null
    }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline -and (Test-PortOpen $Port)) {
        Start-Sleep -Milliseconds 300
    }
    if (Test-PortOpen $Port) {
        Write-Warning "Port $Port is still in use, please check manually"
    }
}

try {
    if (-not $SkipStart) {
        if (Test-PortOpen $Port) {
            throw "Port $Port is already in use. Release it first, or use -SkipStart."
        }

        $ReleaseExe = Resolve-ServerBinary
        if ($null -eq $ReleaseExe) {
            Write-Host "No S3-capable binary found, building with CARGO_TARGET_DIR=$BuildDir ..."
            $env:CARGO_TARGET_DIR = $BuildDir
            Push-Location $ServerRoot
            try {
                if (Test-Path $CargoExe) { & $CargoExe build --release } else { cargo build --release }
                if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
            } finally {
                Pop-Location
            }
            $ReleaseExe = Join-Path $BuildDir 'release\oss-server.exe'
            if (-not (Test-Path $ReleaseExe)) {
                throw "oss-server.exe not found after build: $ReleaseExe"
            }
        }
        Write-Host "Using server binary: $ReleaseExe"

        New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
        New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
        $env:OSS_HOST = "127.0.0.1:$Port"
        $env:OSS_DATA_DIR = $DataDir
        $env:OSS_STORAGE_BACKEND = 'redb'
        $env:OSS_DB_URL = "redb://$DataDir\oss.db"
        $env:OSS_ADMIN_TOKEN = $AdminToken
        $env:OSS_CLEANUP_INTERVAL_MINUTES = '0'
        $env:OSS_TRASH_RETENTION_DAYS = '30'

        Write-Host "Starting server: $ReleaseExe (host=$env:OSS_HOST, data=$DataDir)"
        $serverProcess = Start-Process -FilePath $ReleaseExe -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput (Join-Path $LogDir 'server.out.log') `
            -RedirectStandardError (Join-Path $LogDir 'server.err.log')

        if (-not (Wait-Health 60)) {
            throw "Server not healthy within 60s, see $LogDir\server.err.log"
        }
    }

    $env:OSS_IT_ENDPOINT = $Endpoint
    $env:OSS_IT_ADMIN_TOKEN = $AdminToken

    Push-Location $ClientDir
    try {
        if ($SkipUnitTests) {
            mvn -B test "-Dtest=S3IntegrationTest" "-DfailIfNoTests=false"
        } else {
            mvn -B test
        }
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }

    if ($exitCode -ne 0) {
        throw "Maven tests failed (exit $exitCode)"
    }
    Write-Host "All integration tests passed"
    exit 0
} catch {
    Write-Error $_
    exit 1
} finally {
    Stop-Server
}
