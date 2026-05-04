# =============================================================================
# Plik: scripts/build.ps1
# Opis: Wrapper buildu TentaFlow na Windows. Odpala cargo build z poprawnie
#       zainicjalizowanym srodowiskiem MSVC (VCINSTALLDIR, INCLUDE, LIB itd.)
#       i wymuszonym Ninja jako generatorem CMake.
#
# Po co to istnieje: zwykly PowerShell nie ma srodowiska MSVC ustawionego.
# Wielu cratow uzywajacych cmake-rs / cc-rs to nie boli (auto-detekcja przez
# rejestr), ale jak tylko jakikolwiek nested CMake / ExternalProject_Add
# uzywa MSBuild — pada bo VCINSTALLDIR jest puste. Wrapper rozwiazuje to
# raz dla calej sesji cargo.
#
# Uzycie:
#   .\scripts\build.ps1                     # cargo build (debug, default features)
#   .\scripts\build.ps1 --release           # cargo build --release
#   .\scripts\build.ps1 --features gpu-vulkan
#   .\scripts\build.ps1 -Cmd test           # cargo test zamiast build
#   .\scripts\build.ps1 -Cmd "clean -p whisper-rs-sys"   # dowolne cargo subcmd
# =============================================================================

[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$Cmd = 'build',
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)

$ErrorActionPreference = 'Stop'

function Log-Info  { param($Msg) Write-Host "[INFO] $Msg"  -ForegroundColor Blue }
function Log-Ok    { param($Msg) Write-Host "[OK] $Msg"    -ForegroundColor Green }
function Log-Warn  { param($Msg) Write-Host "[WARN] $Msg"  -ForegroundColor Yellow }
function Log-Error { param($Msg) Write-Host "[ERROR] $Msg" -ForegroundColor Red }

# --- PATH refresh ------------------------------------------------------------

function Refresh-Env {
    # Skrypt moze byc uruchomiony z basha / WSL / IDE ktore propaguja okrojony
    # zestaw env vars (czesto tylko PATH i to bez wpisow z User scope).
    # Setup.ps1 zapisuje rzeczy jak VULKAN_SDK, LIBCLANG_PATH, PROTOC, HIP_PATH
    # CMAKE_GENERATOR do User scope rejestru — musimy je tu zaciagnac, inaczej
    # cargo widzi None i build.rs cratow pada.
    foreach ($scope in @('Machine', 'User')) {
        $vars = [Environment]::GetEnvironmentVariables($scope)
        foreach ($name in $vars.Keys) {
            if ($name -ieq 'Path') { continue }   # PATH lacze osobno
            $val = $vars[$name]
            if ($val -and -not (Get-Item "Env:$name" -ErrorAction SilentlyContinue)) {
                Set-Item -Path "Env:$name" -Value $val
            }
        }
    }

    # PATH lacze Machine + User (User wins na koncu, jak Windows). Dorzucamy
    # tez katalog VS Installer (vswhere.exe) bo Launch-VsDevShell.ps1 go
    # potrzebuje a Microsoft go nie dodaje do PATH.
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $userPath    = [Environment]::GetEnvironmentVariable('Path', 'User')
    $vsInstaller = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer'
    $combined = "$machinePath;$userPath"
    if ((Test-Path $vsInstaller) -and ($combined -notmatch [regex]::Escape($vsInstaller))) {
        $combined = "$combined;$vsInstaller"
    }
    $env:Path = $combined
}

# --- VS environment ----------------------------------------------------------

function Initialize-VsEnv {
    if ($env:VCINSTALLDIR) {
        Log-Ok "VS env juz aktywne: $env:VCINSTALLDIR"
        return
    }

    # Znajdz Launch-VsDevShell.ps1 — jest w kazdej instalacji VS 2022
    # (BuildTools / Community / Pro / Enterprise).
    $candidates = @(
        'C:\BuildTools\Common7\Tools\Launch-VsDevShell.ps1',
        'C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\Launch-VsDevShell.ps1',
        'C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\Launch-VsDevShell.ps1',
        'C:\Program Files\Microsoft Visual Studio\2022\Professional\Common7\Tools\Launch-VsDevShell.ps1',
        'C:\Program Files\Microsoft Visual Studio\2022\Enterprise\Common7\Tools\Launch-VsDevShell.ps1',
        'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\Launch-VsDevShell.ps1'
    )
    $launcher = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1

    if (-not $launcher) {
        Log-Error "Nie znaleziono Launch-VsDevShell.ps1. Zainstaluj VS 2022 Build Tools:"
        Log-Error "  scripts\setup.ps1"
        exit 1
    }

    Log-Info "Inicjalizuje VS env: $launcher"
    # -SkipAutomaticLocation zeby Launch-VsDevShell nie zmienil naszego CWD.
    & $launcher -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
    if (-not $env:VCINSTALLDIR) {
        Log-Error "Launch-VsDevShell zakonczony, ale VCINSTALLDIR nadal puste."
        exit 1
    }
    Log-Ok "VS env aktywne: $env:VCINSTALLDIR"
}

# --- CMake generator ---------------------------------------------------------

function Ensure-Ninja {
    # CMAKE_GENERATOR=Ninja powinno byc ustawione persistent przez setup.ps1,
    # ale na wszelki wypadek wymuszamy w sesji jak puste.
    if (-not $env:CMAKE_GENERATOR) {
        $env:CMAKE_GENERATOR = 'Ninja'
        Log-Info "Ustawiono CMAKE_GENERATOR=Ninja w tej sesji"
    } else {
        Log-Ok "CMAKE_GENERATOR=$env:CMAKE_GENERATOR"
    }

    # CMAKE_GENERATOR_INSTANCE / _PLATFORM / _TOOLSET maja sens tylko dla
    # generatora "Visual Studio 17 2022"; pod Ninja CMake wybucha:
    #   "Generator Ninja does not support instance specification".
    # cmake-rs czyta te env vars z PROCESU (nie PS scope) i przekazuje do
    # cmake. Niektore moga przyjsc z VsDevShell (CMAKE_GENERATOR_INSTANCE),
    # inne moze ustawic cargo/env. Czyscimy je twardo na poziomie procesu.
    foreach ($v in 'CMAKE_GENERATOR_INSTANCE','CMAKE_GENERATOR_PLATFORM','CMAKE_GENERATOR_TOOLSET') {
        $current = [Environment]::GetEnvironmentVariable($v, 'Process')
        if ($current) {
            [Environment]::SetEnvironmentVariable($v, $null, 'Process')
            Log-Info "Wyczyszczono $v (bylo: $current)"
        }
    }

    if (-not (Get-Command 'ninja' -ErrorAction SilentlyContinue)) {
        Log-Error "Ninja nie jest w PATH. Uruchom scripts\setup.ps1 i otworz nowy shell."
        exit 1
    }
}

# --- PROTOC fallback ---------------------------------------------------------

function Ensure-Protoc {
    # Setup ustawia PROTOC trwale. Jak go nie ma w sesji a jest w User scope,
    # zaciagnij. Jak nie ma nigdzie — ostrzez.
    if (-not $env:PROTOC) {
        $userProtoc = [Environment]::GetEnvironmentVariable('PROTOC', 'User')
        if ($userProtoc) {
            $env:PROTOC = $userProtoc
            Log-Info "PROTOC zaciagniete z User env: $env:PROTOC"
        } else {
            Log-Warn "PROTOC nie ustawione — tentaflow-voice build moze padac. Uruchom scripts\setup.ps1."
        }
    }
}

# --- Main --------------------------------------------------------------------

function Enter-CrateDir {
    # Brak workspace Cargo.toml w roocie repo — glowna binarka zyje w
    # tentaflow/. Jak user odpalil skrypt z D:\repos\TentaFlow (czesty case
    # przy `scripts\build ...` z cmd), wchodzimy do tentaflow/ automatycznie.
    if (Test-Path '.\Cargo.toml') { return }
    if (Test-Path '.\tentaflow\Cargo.toml') {
        Log-Info "Brak Cargo.toml w CWD — wchodze do tentaflow/"
        Set-Location -LiteralPath '.\tentaflow'
        return
    }
    Log-Error "Nie znalazlem Cargo.toml ani tentaflow/Cargo.toml. Uruchom z roota repo lub z katalogu crate'a."
    exit 1
}

function Main {
    Refresh-Env
    Initialize-VsEnv
    Ensure-Ninja
    Ensure-Protoc
    Enter-CrateDir

    # Cargo subcommand moze byc multi-word ("clean -p whisper-rs-sys") —
    # rozbij i polacz z pozostalymi argumentami.
    $cmdParts = $Cmd -split '\s+'
    $allArgs = @($cmdParts) + @($CargoArgs)

    Write-Host ''
    Log-Info "Uruchamiam: cargo $($allArgs -join ' ')"
    Write-Host ''

    & cargo @allArgs
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        Log-Error "cargo zakonczone z exit code $code"
        exit $code
    }
    Log-Ok "Build OK"
}

Main
