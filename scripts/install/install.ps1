# =============================================================================
# File:        install.ps1
# Description: Zero-touch installer for TentaFlow on Windows. Installs Docker
#              Desktop (via winget) and Python 3.11+ if missing.
#
# Usage:
#   irm https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.ps1 | iex
#
# Environment overrides:
#   $env:TENTAFLOW_VERSION      = "v0.1.0"
#   $env:TENTAFLOW_PREFIX       = "C:\Program Files\TentaFlow"
#   $env:TENTAFLOW_USER_INSTALL = "1"   # no admin, install under %LOCALAPPDATA%\TentaFlow
#   $env:TENTAFLOW_NO_AUTOSTART = "1"   # skip Scheduled Task registration
#   $env:TENTAFLOW_SKIP_DEPS    = "1"   # skip all dep installation
#   $env:TENTAFLOW_SKIP_DOCKER  = "1"   # skip Docker Desktop install
#   $env:TENTAFLOW_SKIP_PYTHON  = "1"   # skip Python install
# =============================================================================

$ErrorActionPreference = 'Stop'
$Repo = 'Slyb00ts/TentaFlow'

$Version     = if ($env:TENTAFLOW_VERSION)    { $env:TENTAFLOW_VERSION } else { 'latest' }
$UserInstall = $env:TENTAFLOW_USER_INSTALL -eq '1'
$NoAutostart = $env:TENTAFLOW_NO_AUTOSTART -eq '1'
$SkipDeps    = $env:TENTAFLOW_SKIP_DEPS   -eq '1'
$SkipDocker  = $env:TENTAFLOW_SKIP_DOCKER -eq '1'
$SkipPython  = $env:TENTAFLOW_SKIP_PYTHON -eq '1'

$Target = 'x86_64-pc-windows-msvc'

function Log($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }
function OK($msg)   { Write-Host " ✓ $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host " ⚠ $msg" -ForegroundColor Yellow }
function Fail($msg) { Write-Host " ✗ $msg" -ForegroundColor Red; throw $msg }

# =============================================================================
# Dependency checks
# =============================================================================

function Has-Command($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

function Check-Winget {
    if (-not (Has-Command 'winget')) {
        Warn "winget nie jest dostepny — instalacja zaleznosci nie zadziala automatycznie."
        Warn "Zainstaluj App Installer z Microsoft Store: https://aka.ms/getwinget"
        return $false
    }
    return $true
}

function Check-Docker {
    if ($SkipDocker) {
        Warn "Docker check pominiety (TENTAFLOW_SKIP_DOCKER=1). Deploy silnikow bedzie padal."
        return
    }

    if (Has-Command 'docker') {
        $ver = (docker --version 2>&1) -join ''
        OK "Docker present: $ver"
    } else {
        Log "Docker Desktop not found — installing via winget"
        if (-not (Check-Winget)) {
            Warn "Zainstaluj Docker Desktop recznie: https://docs.docker.com/desktop/install/windows-install/"
            return
        }
        winget install --id Docker.DockerDesktop --silent --accept-package-agreements --accept-source-agreements
        OK "Docker Desktop zainstalowany — uruchom go z Start Menu zanim zrobisz pierwszy deploy"
    }

    # BuildKit — w Docker Desktop buildx jest wbudowany, tylko sprawdzmy.
    try {
        $bx = (docker buildx version 2>&1) -join ''
        OK "Docker buildx (BuildKit) present: $bx"
    } catch {
        Warn "buildx nie dziala — Docker Desktop moze nie byc uruchomiony."
    }

    try {
        docker info | Out-Null
        OK "Docker daemon runs"
    } catch {
        Warn "Docker daemon nie odpowiada. Uruchom Docker Desktop z Start Menu i zaczekaj 30s."
    }
}

function Check-Python {
    if ($SkipPython) {
        Warn "Python check pominiety. Python-bundle silniki (vLLM, xtts, parakeet) nie zadzialaja."
        return
    }

    $pyCmd = $null
    foreach ($c in @('python3.12','python3.11','python','python3')) {
        if (Has-Command $c) {
            $ok = & $c -c 'import sys; sys.exit(0 if sys.version_info >= (3,10) else 1)' 2>&1
            if ($LASTEXITCODE -eq 0) { $pyCmd = $c; break }
        }
    }

    if (-not $pyCmd) {
        Log "Python 3.10+ not found — installing via winget"
        if (-not (Check-Winget)) {
            Warn "Zainstaluj Python 3.11+ recznie: https://www.python.org/downloads/"
            return
        }
        winget install --id Python.Python.3.12 --silent --accept-package-agreements --accept-source-agreements
        # Odswiez PATH w biezacej sesji
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
        $pyCmd = 'python'
    }

    if ($pyCmd) {
        $ver = (& $pyCmd --version 2>&1) -join ''
        OK "Python present: $pyCmd ($ver)"
        try {
            & $pyCmd -m pip --version | Out-Null
            OK "pip present"
        } catch {
            Warn "pip nie dziala — Python-bundle deploy moze padac"
        }
    }
}

# =============================================================================
# Path setup
# =============================================================================

if ($UserInstall) {
    $Prefix  = if ($env:TENTAFLOW_PREFIX) { $env:TENTAFLOW_PREFIX } else { Join-Path $env:LOCALAPPDATA 'TentaFlow' }
    $BinLink = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\tentaflow.exe'
} else {
    $Prefix  = if ($env:TENTAFLOW_PREFIX) { $env:TENTAFLOW_PREFIX } else { Join-Path $env:ProgramFiles 'TentaFlow' }
    $BinLink = $null
}

Write-Host ""
Write-Host "TentaFlow installer" -ForegroundColor White
Write-Host "  target:  $Target"
Write-Host "  version: $Version"
Write-Host "  prefix:  $Prefix"
Write-Host ""

# =============================================================================
# Run dependency install
# =============================================================================

if (-not $SkipDeps) {
    Log "Checking dependencies"
    Check-Docker
    Check-Python
    Write-Host ""
}

# =============================================================================
# Download + extract
# =============================================================================

if (-not (Test-Path $Prefix)) { New-Item -ItemType Directory -Path $Prefix -Force | Out-Null }

if ($Version -eq 'latest') {
    Log "Resolving latest release tag"
    $apiResp = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Version = $apiResp.tag_name
    OK "Latest: $Version"
}

$AssetUrl = "https://github.com/$Repo/releases/download/$Version/tentaflow-$Version-$Target.zip"
$ShaUrl   = "$AssetUrl.sha256"

$tmp = Join-Path $env:TEMP "tentaflow-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
$zipPath = Join-Path $tmp 'tentaflow.zip'

Log "Downloading $AssetUrl"
Invoke-WebRequest $AssetUrl -OutFile $zipPath -UseBasicParsing
try { Invoke-WebRequest $ShaUrl -OutFile "$zipPath.sha256" -UseBasicParsing } catch {}

if (Test-Path "$zipPath.sha256") {
    Log "Verifying SHA-256"
    $expected = (Get-Content "$zipPath.sha256" | Where-Object { $_ -match '[0-9a-fA-F]{64}' } | Select-Object -First 1) -replace '.*([0-9a-fA-F]{64}).*','$1'
    $actual   = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
    if ($expected.ToLower() -ne $actual) {
        Fail "SHA-256 mismatch: expected $expected, got $actual"
    }
    OK "Checksum OK"
}

Log "Extracting to $Prefix"
Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
$inner = Get-ChildItem $tmp -Directory | Where-Object { $_.Name -like "tentaflow-$Version-*" } | Select-Object -First 1
Copy-Item -Path "$($inner.FullName)\*" -Destination $Prefix -Recurse -Force

if (-not (Test-Path "$Prefix\config.toml") -and (Test-Path "$Prefix\config.example.toml")) {
    Copy-Item "$Prefix\config.example.toml" "$Prefix\config.toml"
}
OK "Installed to $Prefix"

if ($UserInstall) {
    if (-not (Test-Path (Split-Path $BinLink))) { New-Item -ItemType Directory -Path (Split-Path $BinLink) -Force | Out-Null }
    Copy-Item -Force "$Prefix\tentaflow.exe" $BinLink
    OK "Shortcut: $BinLink"
} else {
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    if ($machinePath -notlike "*$Prefix*") {
        [Environment]::SetEnvironmentVariable('Path', "$machinePath;$Prefix", 'Machine')
        OK "Added $Prefix to PATH (Machine scope). Open a new terminal to pick it up."
    }
}

if (-not $NoAutostart) {
    $taskName = 'TentaFlow'
    Log "Registering Scheduled Task '$taskName' (runs at logon)"
    $action   = New-ScheduledTaskAction -Execute "$Prefix\tentaflow.exe" -Argument "--config `"$Prefix\config.toml`"" -WorkingDirectory $Prefix
    $trigger  = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    OK "Running. Status: Get-ScheduledTask $taskName"
} else {
    Log "Skipping auto-start. Run manually: $Prefix\tentaflow.exe"
}

Remove-Item -Recurse -Force $tmp

# =============================================================================
# Final summary
# =============================================================================

Write-Host ""
Write-Host "Installation complete" -ForegroundColor Green
Write-Host "  binary:     $Prefix\tentaflow.exe"
Write-Host "  prefix:     $Prefix"
Write-Host "  version:    $Version"
Write-Host "  dashboard:  https://localhost:8090"
Write-Host ""

try {
    docker info | Out-Null
} catch {
    Warn "Uruchom Docker Desktop z Start Menu zanim zrobisz pierwszy deploy silnika."
}

Write-Host ""
Write-Host "Next steps:"
Write-Host "  1. Open dashboard:       https://localhost:8090"
Write-Host "  2. Deploy silnik:        Services -> Nowy serwis -> wybierz silnik"
Write-Host "  3. Scheduled Task:       Get-ScheduledTask TentaFlow"
Write-Host "  4. Logi:                 $Prefix\tentaflow.log"
Write-Host ""
