# =============================================================================
# File:        scripts/install/uninstall.ps1
# Description: Removes a TentaFlow installation made by install.ps1 - the
#              counterpart of uninstall.sh.
#
# Data is kept by default: %ProgramData%\TentaFlow holds the database, the
# vector store and the per-installation TLS identity, and an uninstall is not a
# request to destroy them. -Purge removes them, and says what it deletes.
#
# The GStreamer runtime stays: it is a system-wide component other software may
# use, and it has its own entry in Settings > Apps.
#
# Usage (PowerShell started as Administrator):
#   uninstall.ps1 [-Purge]
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Slyb00ts/TentaFlow/main/scripts/install/uninstall.ps1))) -Purge
# =============================================================================

param([switch]$Purge)

$ErrorActionPreference = 'Stop'
$ServiceName = 'TentaFlow'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Removing a service and Program Files needs PowerShell started with "Run as administrator".'
}

# The receipt says what was installed and where, so an uninstall does not guess.
$receiptPath = Join-Path $env:ProgramData 'TentaFlow\install-receipt.json'
if (Test-Path -LiteralPath $receiptPath) {
    $receipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
    $prefix = $receipt.prefix
    $configDir = Split-Path -Parent $receipt.config
    $dataDir = $receipt.home
} else {
    Write-Warning 'No receipt - assuming the default layout.'
    $prefix = Join-Path $env:ProgramFiles 'TentaFlow'
    $configDir = Join-Path $env:ProgramData 'TentaFlow'
    $dataDir = Join-Path $configDir 'data'
}

Write-Host 'Removing TentaFlow:'
Write-Host "  prefix: $prefix"
Write-Host "  config: $configDir"
$fate = '(kept)'
if ($Purge) { $fate = '(WILL BE DELETED)' }
Write-Host "  data:   $dataDir $fate"

$service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($service) {
    # Stopped before it is deleted: a deleted service that still runs is only
    # marked for deletion and keeps its files open until it exits.
    if ($service.Status -ne 'Stopped') {
        Stop-Service -Name $ServiceName -Force
        $service.WaitForStatus('Stopped', [TimeSpan]::FromMinutes(3))
    }
    & sc.exe delete $ServiceName | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "sc.exe delete $ServiceName failed (exit $LASTEXITCODE)." }
    Write-Host "  service $ServiceName removed"
}

Get-NetFirewallRule -Group 'TentaFlow' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
Write-Host '  firewall rules removed'

$current = Join-Path $prefix 'current'
$path = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$kept = @($path -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ine $current.TrimEnd('\')) })
[Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'Machine')

if (Test-Path -LiteralPath $prefix) {
    # The junction first: removing the tree through it would reach the version
    # directory twice.
    if (Test-Path -LiteralPath $current) { (Get-Item -LiteralPath $current -Force).Delete() }
    Remove-Item -LiteralPath $prefix -Recurse -Force
}

if ($Purge) {
    Remove-Item -LiteralPath $dataDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $configDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'Data and configuration were removed as well.'
} else {
    Write-Host "Data and configuration kept: $dataDir, $configDir"
    Write-Host 'To remove everything: uninstall.ps1 -Purge'
}
