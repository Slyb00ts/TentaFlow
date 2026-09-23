# =============================================================================
# File: scripts/release/smoke-windows.ps1
# Purpose: Proves a staged Windows archive starts on its own: extracts the ZIP
#          into an empty directory, writes a config with `init-config`, starts
#          the server with a PATH of System32 plus the external runtimes the
#          archive declares (GStreamer for `full`), and waits for /health.
#          Nothing from the build tree or the toolchains is reachable, so a DLL
#          the archive forgot is a start failure here, not on a user's machine.
#
# Usage:
#   scripts\release\smoke-windows.ps1 -Archive dist\tentaflow-...-full-vulkan.zip `
#       [-GstreamerBin <GStreamer>\bin]
# =============================================================================

param(
    [Parameter(Mandatory)][string]$Archive,
    [string]$GstreamerBin = '',
    [int]$Port = 18099,
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = 'Stop'

$work = Join-Path ([IO.Path]::GetTempPath()) "tentaflow-smoke-$PID"
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Force -Path $work | Out-Null
Expand-Archive -Path $Archive -DestinationPath $work
$root = Get-ChildItem $work -Directory | Select-Object -First 1
if (-not $root) { throw "The archive $Archive has no top-level folder." }
$exe = Join-Path $root.FullName 'tentaflow.exe'
if (-not (Test-Path $exe)) { throw "tentaflow.exe is not in $($root.FullName)." }

$requirements = Join-Path $root.FullName 'REQUIREMENTS.txt'
$pathEntries = @("$env:SystemRoot\System32", $env:SystemRoot, "$env:SystemRoot\System32\WindowsPowerShell\v1.0")
if (Test-Path $requirements) {
    if (-not $GstreamerBin -or -not (Test-Path (Join-Path $GstreamerBin 'gstreamer-1.0-0.dll'))) {
        throw "The archive declares an external GStreamer runtime; pass -GstreamerBin."
    }
    $pathEntries += $GstreamerBin
}

$nodeHome = Join-Path $work 'home'
$config = Join-Path $work 'config.toml'
New-Item -ItemType Directory -Force -Path $nodeHome | Out-Null

$savedPath = $env:Path
$env:Path = $pathEntries -join ';'
$proc = $null
try {
    & $exe init-config --output $config --bind "127.0.0.1:$Port"
    if ($LASTEXITCODE -ne 0) {
        # 0xC0000135 = STATUS_DLL_NOT_FOUND: an import the archive does not carry.
        throw ("init-config failed with exit code {0} (0x{0:X8})." -f $LASTEXITCODE)
    }
    $log = Join-Path $work 'server.log'
    $err = Join-Path $work 'server.err.log'
    $proc = Start-Process -FilePath $exe -ArgumentList @('--config', $config, '--home', $nodeHome) `
        -RedirectStandardOutput $log -RedirectStandardError $err -PassThru -WindowStyle Hidden

    # The node generates its TLS identity and database before the socket opens.
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $healthy = $false
    while ((Get-Date) -lt $deadline) {
        if ($proc.HasExited) {
            throw ("tentaflow.exe exited with code {0} (0x{0:X8}) before answering /health." -f $proc.ExitCode)
        }
        # curl.exe ships with Windows and, unlike Invoke-WebRequest on PS 5.1,
        # takes the self-signed per-installation certificate with -k.
        & curl.exe -fsk "https://127.0.0.1:$Port/health" *> $null
        if ($LASTEXITCODE -eq 0) { $healthy = $true; break }
        Start-Sleep -Seconds 2
    }
    if (-not $healthy) { throw "No answer on https://127.0.0.1:$Port/health within $TimeoutSeconds s." }
    Write-Host "OK: $($root.Name) answers /health"
} catch {
    foreach ($file in @((Join-Path $work 'server.log'), (Join-Path $work 'server.err.log'))) {
        if (Test-Path $file) {
            Write-Host "----- $file"
            Get-Content $file -Tail 60 | Write-Host
        }
    }
    Write-Host "::error::$($_.Exception.Message)"
    throw
} finally {
    $env:Path = $savedPath
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
}
