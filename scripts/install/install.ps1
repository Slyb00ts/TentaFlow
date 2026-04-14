# =============================================================================
# File:        install.ps1
# Description: One-liner installer for TentaFlow on Windows.
#
# Usage:
#   irm https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.ps1 | iex
#
# Environment overrides:
#   $env:TENTAFLOW_VERSION      = "v0.1.0"
#   $env:TENTAFLOW_PREFIX       = "C:\Program Files\TentaFlow"
#   $env:TENTAFLOW_USER_INSTALL = "1"   # no admin, install under %LOCALAPPDATA%\TentaFlow
#   $env:TENTAFLOW_NO_AUTOSTART = "1"   # skip Scheduled Task registration
# =============================================================================

$ErrorActionPreference = 'Stop'
$Repo = 'Slyb00ts/TentaFlow'

$Version     = if ($env:TENTAFLOW_VERSION) { $env:TENTAFLOW_VERSION } else { 'latest' }
$UserInstall = $env:TENTAFLOW_USER_INSTALL -eq '1'
$NoAutostart = $env:TENTAFLOW_NO_AUTOSTART -eq '1'

$Target = 'x86_64-pc-windows-msvc'

if ($UserInstall) {
    $Prefix  = if ($env:TENTAFLOW_PREFIX) { $env:TENTAFLOW_PREFIX } else { Join-Path $env:LOCALAPPDATA 'TentaFlow' }
    $BinLink = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\tentaflow.exe'
} else {
    $Prefix  = if ($env:TENTAFLOW_PREFIX) { $env:TENTAFLOW_PREFIX } else { Join-Path $env:ProgramFiles 'TentaFlow' }
    $BinLink = $null
}

Write-Host "==> TentaFlow installer"
Write-Host "    target:  $Target"
Write-Host "    version: $Version"
Write-Host "    prefix:  $Prefix"

if (-not (Test-Path $Prefix)) { New-Item -ItemType Directory -Path $Prefix -Force | Out-Null }

if ($Version -eq 'latest') {
    $apiResp = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Version = $apiResp.tag_name
}

$AssetUrl = "https://github.com/$Repo/releases/download/$Version/tentaflow-$Version-$Target.zip"
$ShaUrl   = "$AssetUrl.sha256"

$tmp = Join-Path $env:TEMP "tentaflow-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
$zipPath = Join-Path $tmp 'tentaflow.zip'

Write-Host "==> Downloading $AssetUrl"
Invoke-WebRequest $AssetUrl -OutFile $zipPath -UseBasicParsing
try { Invoke-WebRequest $ShaUrl -OutFile "$zipPath.sha256" -UseBasicParsing } catch {}

if (Test-Path "$zipPath.sha256") {
    Write-Host "==> Verifying SHA-256"
    $expected = (Get-Content "$zipPath.sha256" | Where-Object { $_ -match '[0-9a-fA-F]{64}' } | Select-Object -First 1) -replace '.*([0-9a-fA-F]{64}).*','$1'
    $actual   = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
    if ($expected.ToLower() -ne $actual) {
        throw "SHA-256 mismatch: expected $expected, got $actual"
    }
}

Write-Host "==> Extracting to $Prefix"
Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
$inner = Get-ChildItem $tmp -Directory | Where-Object { $_.Name -like "tentaflow-$Version-*" } | Select-Object -First 1
Copy-Item -Path "$($inner.FullName)\*" -Destination $Prefix -Recurse -Force

if (-not (Test-Path "$Prefix\config.toml") -and (Test-Path "$Prefix\config.example.toml")) {
    Copy-Item "$Prefix\config.example.toml" "$Prefix\config.toml"
}

if ($UserInstall) {
    if (-not (Test-Path (Split-Path $BinLink))) { New-Item -ItemType Directory -Path (Split-Path $BinLink) -Force | Out-Null }
    Copy-Item -Force "$Prefix\tentaflow.exe" $BinLink
    Write-Host "==> Shortcut: $BinLink"
} else {
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    if ($machinePath -notlike "*$Prefix*") {
        [Environment]::SetEnvironmentVariable('Path', "$machinePath;$Prefix", 'Machine')
        Write-Host "==> Added $Prefix to PATH (Machine scope). Open a new terminal to pick it up."
    }
}

if (-not $NoAutostart) {
    $taskName = 'TentaFlow'
    Write-Host "==> Registering Scheduled Task '$taskName' (runs at logon)"
    $action   = New-ScheduledTaskAction -Execute "$Prefix\tentaflow.exe" -Argument "--config `"$Prefix\config.toml`"" -WorkingDirectory $Prefix
    $trigger  = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Write-Host "==> Running. Status: Get-ScheduledTask $taskName"
} else {
    Write-Host "==> Skipping auto-start. Run manually: $Prefix\tentaflow.exe"
}

Remove-Item -Recurse -Force $tmp
Write-Host ""
Write-Host "==> Installation complete. Version: $Version"
Write-Host "    prefix: $Prefix"
Write-Host "    update: tentaflow update"
