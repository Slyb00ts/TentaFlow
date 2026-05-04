# =============================================================================
# Plik: scripts/setup.ps1
# Opis: Instalator zaleznosci do kompilacji TentaFlow na Windows.
#       Uzywa winget jako glownego menedzera pakietow, ustawia zmienne
#       srodowiskowe (LIBCLANG_PATH, PATH) i konfiguruje rustup + targety WASM.
#
# Uzycie:
#   PowerShell -ExecutionPolicy Bypass -File scripts/setup.ps1 [-Cuda] [-Vulkan] [-Rocm] [-AllGpu]
#
# Uwagi:
#   - Nie wymaga uruchomienia jako Administrator (winget pyta o UAC per pakiet).
#     WYJATEK: -Rocm wymaga uruchomienia w sesji Administratora — instalator
#     AMD Setup.exe odrzuca silent install bez elevated UAC.
#   - ROCm na Windows = AMD HIP SDK (najnowszy 7.1.1, luty 2026). AMD nie ma
#     pakietu winget; wymagana jest akceptacja EULA przed pobraniem. Skrypt
#     albo sciaga z URL podanego przez -RocmInstaller, albo otwiera strone
#     download i prowadzi krok po kroku.
# =============================================================================

[CmdletBinding()]
param(
    [switch]$Cuda,
    [switch]$Vulkan,
    [switch]$Rocm,
    [switch]$AllGpu,
    [string]$RocmInstaller,   # opcjonalna sciezka do recznie pobranego AMD Setup.exe
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

# PS 5.1 wraps native-command stderr (np. rustup, winget pisza info: na stderr)
# w NativeCommandError i pod $ErrorActionPreference='Stop' wybucha. Trzymamy
# Stop dla cmdletow, ale stderr natywnych komend nigdy nie powinien terminowac
# pipeline'a. Helper Invoke-NativeCapture lapie stdout bez 2>&1.
function Invoke-NativeCapture {
    param([Parameter(Mandatory)][scriptblock]$Script)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        # 2>$null wyrzuca stderr, dzieki czemu nie ma NativeCommandError-a
        & $Script 2>$null
    } finally {
        $ErrorActionPreference = $prev
    }
}

# Wersja MUSI byc zgodna z dependency w tentaflow-protocol-wasm/Cargo.toml
# oraz z hardkodowana wartoscia w tentaflow-core/build.rs.
$WasmBindgenVersion = '0.2.108'

# Lista zainstalowanych komponentow (do podsumowania)
$script:Installed = @()

# --- Logowanie ---

function Log-Info    { param([string]$Msg) Write-Host "[INFO] $Msg"    -ForegroundColor Blue }
function Log-Ok      { param([string]$Msg) Write-Host "[OK] $Msg"      -ForegroundColor Green }
function Log-Warn    { param([string]$Msg) Write-Host "[WARN] $Msg"    -ForegroundColor Yellow }
function Log-Error   { param([string]$Msg) Write-Host "[ERROR] $Msg"   -ForegroundColor Red }
function Log-Section { param([string]$Msg) Write-Host "`n=== $Msg ===`n" -ForegroundColor Cyan }

function Show-Usage {
    @"
TentaFlow - instalator zaleznosci (Windows)

Uzycie:
  PowerShell -ExecutionPolicy Bypass -File scripts/setup.ps1 [OPCJE]

Opcje:
  -Cuda                       Zainstaluj NVIDIA CUDA toolkit (winget Nvidia.CUDA)
  -Vulkan                     Zainstaluj Vulkan SDK (KhronosGroup.VulkanSDK)
  -Rocm                       Zainstaluj AMD HIP SDK (ROCm na Windows, 7.1.1+).
                              Wymaga sesji Administratora.
  -RocmInstaller <sciezka>    Sciezka do wczesniej pobranego AMD Setup.exe.
                              Bez tego skrypt otworzy strone download AMD
                              i poprosi o reczne pobranie.
  -AllGpu                     CUDA + Vulkan + ROCm (sesja Admina wymagana dla ROCm)
  -Help                       Pokaz te pomoc

Przyklady:
  scripts\setup.ps1                                      # Tylko bazowe zaleznosci
  scripts\setup.ps1 -Cuda                                # Baza + CUDA
  scripts\setup.ps1 -Rocm                                # Baza + HIP SDK (otworzy strone download)
  scripts\setup.ps1 -Rocm -RocmInstaller C:\dl\Setup.exe # Baza + HIP SDK z lokalnego pliku
  scripts\setup.ps1 -AllGpu                              # Wszystko (jako Admin)
"@ | Write-Host
}

if ($Help) { Show-Usage; exit 0 }
if ($AllGpu) { $Cuda = $true; $Vulkan = $true; $Rocm = $true }

# --- Helpers ---

function Test-Command {
    param([string]$Name)
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Refresh-Path {
    # Po kazdym wingetcie scieagamy zaktualizowany PATH z rejestru,
    # zeby kolejne komendy widzialy nowo zainstalowane narzedzia bez
    # restartu shella.
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $userPath    = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = "$machinePath;$userPath"
}

function Set-PersistentEnv {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Value,
        [ValidateSet('User','Machine')][string]$Scope = 'User'
    )
    [Environment]::SetEnvironmentVariable($Name, $Value, $Scope)
    Set-Item -Path "Env:$Name" -Value $Value
    Log-Ok "Ustawiono $Name=$Value (scope: $Scope)"
}

function Add-PersistentPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [ValidateSet('User','Machine')][string]$Scope = 'User'
    )
    if (-not (Test-Path $Path)) {
        Log-Warn "Sciezka nie istnieje, pomijam dodanie do PATH: $Path"
        return
    }
    $current = [Environment]::GetEnvironmentVariable('Path', $Scope)
    $entries = ($current -split ';') | Where-Object { $_ -and $_.Trim() }
    if ($entries -contains $Path) {
        Log-Info "PATH ($Scope) juz zawiera: $Path"
    } else {
        $new = ($entries + $Path) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $new, $Scope)
        Log-Ok "Dodano do PATH ($Scope): $Path"
    }
    if (-not (($env:Path -split ';') -contains $Path)) {
        $env:Path = "$env:Path;$Path"
    }
}

function Winget-Install {
    param(
        [Parameter(Mandatory)][string]$Id,
        [string]$Label = $null
    )
    if (-not $Label) { $Label = $Id }

    if (-not (Test-Command 'winget')) {
        Log-Error "winget nie jest dostepny. Zainstaluj 'App Installer' z Microsoft Store i uruchom ponownie."
        exit 1
    }

    # Sprawdz czy juz zainstalowane (silent, exit 0 = znalezione)
    $listOut = Invoke-NativeCapture { winget list --id $Id --accept-source-agreements } | Out-String
    if ($LASTEXITCODE -eq 0 -and $listOut -match [regex]::Escape($Id)) {
        Log-Ok "$Label juz zainstalowany (winget: $Id)"
        return $false
    }

    Log-Info "Instalacja $Label przez winget ($Id)..."
    & winget install --id $Id --exact --silent --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -ne 0) {
        # winget zwraca rozne kody bledow. Sprawdz czy mimo to pakiet jest dostepny.
        Refresh-Path
        $reCheck = Invoke-NativeCapture { winget list --id $Id } | Out-String
        if ($reCheck -match [regex]::Escape($Id)) {
            Log-Ok "$Label zainstalowany (winget zwrocil exit code $LASTEXITCODE, ale pakiet jest obecny)"
        } else {
            Log-Warn "winget install $Id zwrocil exit code $LASTEXITCODE — sprawdz recznie."
            return $false
        }
    } else {
        Log-Ok "$Label zainstalowany"
    }
    Refresh-Path
    $script:Installed += $Label
    return $true
}

# --- Sprawdzenia wstepne ---

function Check-Prereqs {
    Log-Section "Sprawdzenie wstepnych wymagan"

    if (-not (Test-Command 'winget')) {
        Log-Error "Brak winget. Zainstaluj 'App Installer' ze sklepu Microsoft Store:"
        Log-Error "  https://apps.microsoft.com/detail/9NBLGGH4NNS1"
        exit 1
    }
    Log-Ok "winget: $(Invoke-NativeCapture { winget --version } | Select-Object -First 1)"

    $psVer = $PSVersionTable.PSVersion
    Log-Info "PowerShell: $psVer"
    Log-Info "OS: $((Get-CimInstance Win32_OperatingSystem).Caption)"
}

# --- Bazowe narzedzia buildu ---

function Find-ProtocExe {
    # 1) Get-Command (jak juz w PATH)
    $cmd = Get-Command 'protoc.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    # 2) winget package dir (Google.Protobuf instaluje tutaj, dodaje alias do
    #    %LOCALAPPDATA%\Microsoft\WinGet\Links ale env var PROTOC nie jest
    #    ustawiana — a build.rs czyta wlasnie ja).
    $wingetPkgs = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages"
    if (Test-Path $wingetPkgs) {
        $found = Get-ChildItem $wingetPkgs -Directory -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^Google\.Protobuf' } |
            ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter 'protoc.exe' -ErrorAction SilentlyContinue } |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }

    # 3) chocolatey / scoop typowe lokalizacje
    foreach ($p in @(
        "$env:ProgramData\chocolatey\bin\protoc.exe",
        "$env:USERPROFILE\scoop\shims\protoc.exe"
    )) {
        if (Test-Path $p) { return $p }
    }

    return $null
}

function Configure-Protoc {
    $protoc = Find-ProtocExe
    if (-not $protoc) {
        Log-Warn "protoc.exe nie znalezione — tentaflow-voice build.rs wymaga env var PROTOC."
        return
    }

    Log-Ok "Znaleziono protoc: $protoc"

    # build.rs sprawdza tylko obecnosc env var PROTOC. PATH alias nie wystarczy.
    $userProtoc = [Environment]::GetEnvironmentVariable('PROTOC', 'User')
    if ($userProtoc -ne $protoc) {
        Set-PersistentEnv -Name 'PROTOC' -Value $protoc -Scope 'User'
        $script:Installed += "PROTOC = $protoc"
    } else {
        Log-Ok "PROTOC juz ustawione na $userProtoc"
    }

    # Dorzucamy tez bin do PATH zeby `protoc --version` dzialal w shellu.
    $protocBin = Split-Path -Parent $protoc
    Add-PersistentPath -Path $protocBin -Scope 'User'
}

function Install-Base {
    Log-Section "Bazowe narzedzia (VS Build Tools, CMake, LLVM, Git, pkg-config)"

    # Visual Studio Build Tools 2022 — MSVC + Windows SDK. Bez tego rustup-init
    # przy stable-x86_64-pc-windows-msvc zglosi 'link.exe not found'.
    # Workload Microsoft.VisualStudio.Workload.VCTools dociaga MSVC + SDK.
    if (-not (Test-Path 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools') -and
        -not (Test-Path 'C:\Program Files\Microsoft Visual Studio\2022\BuildTools') -and
        -not (Test-Path 'C:\Program Files\Microsoft Visual Studio\2022\Community') -and
        -not (Test-Path 'C:\Program Files\Microsoft Visual Studio\2022\Professional') -and
        -not (Test-Path 'C:\Program Files\Microsoft Visual Studio\2022\Enterprise')) {
        Log-Info "Instalacja Visual Studio 2022 Build Tools z workloadem C++..."
        # winget ma override do dodania workloadu VCTools.
        & winget install --id Microsoft.VisualStudio.2022.BuildTools --exact --silent `
            --accept-source-agreements --accept-package-agreements `
            --override "--quiet --wait --norestart --nocache --installPath `"C:\BuildTools`" --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
        if ($LASTEXITCODE -ne 0) {
            Log-Warn "winget VS BuildTools zwrocil exit code $LASTEXITCODE — moze byc juz zainstalowane lub wymagac restartu."
        } else {
            $script:Installed += "VS 2022 Build Tools (VCTools + Win11 SDK)"
        }
        Refresh-Path
    } else {
        Log-Ok "Visual Studio 2022 (Build Tools / Community / Pro / Enterprise) juz obecne"
    }

    # CMake
    Winget-Install -Id 'Kitware.CMake' -Label 'CMake' | Out-Null

    # LLVM (dostarcza clang.exe + libclang.dll potrzebne dla bindgen w whisper-rs-sys)
    $llvmInstalled = Winget-Install -Id 'LLVM.LLVM' -Label 'LLVM/Clang'

    # Ustaw LIBCLANG_PATH (wymagane dla bindgen — whisper-rs-sys, llama-cpp itd.)
    $llvmBin = 'C:\Program Files\LLVM\bin'
    if (Test-Path (Join-Path $llvmBin 'libclang.dll')) {
        if (-not $env:LIBCLANG_PATH -or $env:LIBCLANG_PATH -ne $llvmBin) {
            Set-PersistentEnv -Name 'LIBCLANG_PATH' -Value $llvmBin -Scope 'User'
            $script:Installed += "LIBCLANG_PATH = $llvmBin"
        } else {
            Log-Ok "LIBCLANG_PATH juz ustawione na $llvmBin"
        }
        Add-PersistentPath -Path $llvmBin -Scope 'User'
    } else {
        Log-Warn "Nie znaleziono libclang.dll w $llvmBin — ustaw LIBCLANG_PATH recznie po instalacji LLVM."
    }

    # Git
    Winget-Install -Id 'Git.Git' -Label 'Git' | Out-Null

    # pkg-config-lite — pkgconf.pkgconf nie ma Windows installera w winget,
    # bloodrock.pkg-config-lite tak (wersja 0.28 bez zaleznosci glib).
    Winget-Install -Id 'bloodrock.pkg-config-lite' -Label 'pkg-config-lite' | Out-Null

    # Ninja (przyspiesza CMake builds, uzywany przez wiele *-sys cratow)
    Winget-Install -Id 'Ninja-build.Ninja' -Label 'Ninja' | Out-Null

    # protoc (Protocol Buffers compiler) — wymagany przez tentaflow-voice/build.rs
    # i kazdy crate uzywajacy prost-build / tonic-build do generowania kodu z .proto.
    Winget-Install -Id 'Google.Protobuf' -Label 'protoc (Protocol Buffers)' | Out-Null
    Configure-Protoc

    Configure-CmakeGenerator

    Refresh-Path
    Log-Ok "Bazowe narzedzia zainstalowane"
}

function Configure-CmakeGenerator {
    # Domyslny generator cmake na Windows to "Visual Studio 17 2022" + MSBuild.
    # Trzy realne problemy:
    # 1. ExternalProject_Add (np. vulkan-shaders-gen w llama.cpp) odpala
    #    zagniezdzony cmake ktory NIE dziedziczy ustawien kompilatora —
    #    pada na "No CMAKE_C_COMPILER could be found".
    # 2. MSBuild w polskiej / non-English wersji Windowsa wybucha na
    #    "The system cannot find the batch label specified - VCEnd"
    #    przy custom build steps wywolujacych vcvarsall.bat.
    # 3. MSBuild jest po prostu wolny i nieczytelny w logach.
    #
    # Ninja flat-buduje wszystko w jednym procesie cmake, korzysta z auto-
    # detekcji MSVC przez `cc` crate (rejestr + vswhere), wiec dziala bez
    # Developer PowerShella i omija wszystkie powyzsze pulapki.
    if (-not (Test-Command 'ninja')) {
        Log-Warn "Ninja nie znalezione — pomijam ustawienie CMAKE_GENERATOR."
        return
    }
    $current = [Environment]::GetEnvironmentVariable('CMAKE_GENERATOR', 'User')
    if ($current -eq 'Ninja') {
        Log-Ok "CMAKE_GENERATOR juz ustawione na Ninja"
        if ($env:CMAKE_GENERATOR -ne 'Ninja') { $env:CMAKE_GENERATOR = 'Ninja' }
        return
    }
    Set-PersistentEnv -Name 'CMAKE_GENERATOR' -Value 'Ninja' -Scope 'User'
    $script:Installed += 'CMAKE_GENERATOR = Ninja (omija MSBuild bug VCEnd)'
}

# --- Rust toolchain ---

function Install-Rust {
    Log-Section "Rust toolchain (rustup + stable msvc)"

    if (Test-Command 'rustup') {
        Log-Ok "rustup juz zainstalowany: $(Invoke-NativeCapture { rustup --version } | Select-Object -First 1)"
        Log-Info "Aktualizacja stable toolchaina..."
        Invoke-NativeCapture { rustup update stable --no-self-update } | Out-Host
    } else {
        # Rustlang.Rustup z winget instaluje rustup + stable-msvc.
        Winget-Install -Id 'Rustlang.Rustup' -Label 'rustup' | Out-Null
        Refresh-Path
        if (-not (Test-Command 'rustup')) {
            Log-Error "rustup nadal nie jest w PATH po instalacji. Otworz nowy PowerShell i uruchom skrypt ponownie."
            exit 1
        }
    }

    Invoke-NativeCapture { rustup default stable-x86_64-pc-windows-msvc } | Out-Host
    Log-Ok "Rust: $(Invoke-NativeCapture { rustc --version } | Select-Object -First 1)"
    $script:Installed += 'rust-stable (msvc)'
}

# --- WASM targets ---

function Install-WasmTargets {
    Log-Section "WASM targety (wasm32-wasip1 + wasm32-unknown-unknown)"

    $installed = Invoke-NativeCapture { rustup target list --installed }

    if ($installed -match 'wasm32-wasip1') {
        Log-Ok "wasm32-wasip1 juz zainstalowany"
    } else {
        Log-Info "Dodawanie targetu wasm32-wasip1..."
        Invoke-NativeCapture { rustup target add wasm32-wasip1 } | Out-Host
        $script:Installed += 'wasm32-wasip1'
    }

    if ($installed -match 'wasm32-unknown-unknown') {
        Log-Ok "wasm32-unknown-unknown juz zainstalowany"
    } else {
        Log-Info "Dodawanie targetu wasm32-unknown-unknown..."
        Invoke-NativeCapture { rustup target add wasm32-unknown-unknown } | Out-Host
        $script:Installed += 'wasm32-unknown-unknown'
    }
}

# --- wasm-bindgen CLI ---

function Install-WasmBindgenCli {
    Log-Section "wasm-bindgen CLI (v$WasmBindgenVersion)"

    if (Test-Command 'wasm-bindgen') {
        $current = (Invoke-NativeCapture { wasm-bindgen --version } | Select-Object -First 1) -split '\s+' | Select-Object -Last 1
        if ($current -eq $WasmBindgenVersion) {
            Log-Ok "wasm-bindgen $current juz zainstalowany"
            return
        }
        Log-Warn "wasm-bindgen $current != wymagana $WasmBindgenVersion — reinstaluje"
    }

    Log-Info "Kompilacja wasm-bindgen-cli (moze potrwac kilka minut)..."
    & cargo install wasm-bindgen-cli --version $WasmBindgenVersion --locked
    if ($LASTEXITCODE -ne 0) {
        Log-Error "cargo install wasm-bindgen-cli failed (exit $LASTEXITCODE)"
        exit 1
    }
    $script:Installed += "wasm-bindgen-cli $WasmBindgenVersion"
}

# --- CUDA (opcjonalne) ---

function Install-Cuda {
    Log-Section "NVIDIA CUDA toolkit"

    if (Test-Command 'nvcc') {
        Log-Ok "CUDA juz zainstalowane: $(Invoke-NativeCapture { nvcc --version } | Select-Object -Last 1)"
        return
    }

    # Nvidia.CUDA dostarcza pelny toolkit (nvcc + cuBLAS + cuDNN nie wchodzi w sklad).
    Winget-Install -Id 'Nvidia.CUDA' -Label 'CUDA Toolkit' | Out-Null
}

# --- Vulkan SDK (opcjonalne) ---

function Install-Vulkan {
    Log-Section "Vulkan SDK (LunarG, full SDK z validation layers)"

    if (Test-Command 'vulkaninfo') {
        Log-Ok "Vulkan SDK juz zainstalowane: $(Invoke-NativeCapture { vulkaninfo --summary } | Select-Object -First 3)"
        return
    }

    Winget-Install -Id 'KhronosGroup.VulkanSDK' -Label 'Vulkan SDK' | Out-Null
    Refresh-Path

    # LunarG SDK ustawia VULKAN_SDK na Machine scope w trakcie instalacji.
    # Foldery w C:\VulkanSDK maja atrybut System+Hidden — bez -Force
    # Get-ChildItem zwraca pusta liste mimo ze foldery istnieja (analogicznie
    # do AMD ROCm). Czytamy najpierw env var z rejestru.
    $machineVulkan = [Environment]::GetEnvironmentVariable('VULKAN_SDK', 'Machine')
    if ($machineVulkan -and (Test-Path $machineVulkan)) {
        Log-Ok "VULKAN_SDK juz ustawione (Machine): $machineVulkan"
        if (-not $env:VULKAN_SDK) { $env:VULKAN_SDK = $machineVulkan }
        Add-PersistentPath -Path (Join-Path $machineVulkan 'Bin') -Scope 'User'
    } else {
        $candidate = Get-ChildItem 'C:\VulkanSDK' -Directory -Force -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
        if ($candidate) {
            Set-PersistentEnv -Name 'VULKAN_SDK' -Value $candidate.FullName -Scope 'User'
            Add-PersistentPath -Path (Join-Path $candidate.FullName 'Bin') -Scope 'User'
        } else {
            Log-Warn "Nie znaleziono katalogu C:\VulkanSDK\* — ustaw VULKAN_SDK recznie."
        }
    }
}

# --- ROCm / AMD HIP SDK (opcjonalne) ---

function Test-IsAdmin {
    $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($id)
    return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Find-HipInstallDir {
    # AMD instaluje do C:\Program Files\AMD\ROCm\<major.minor>\ (np. 7.1).
    # Foldery sa ustawiane z atrybutem System/Hidden — bez -Force
    # Get-ChildItem zwraca pusta liste mimo ze foldery istnieja.
    $root = 'C:\Program Files\AMD\ROCm'
    if (-not (Test-Path $root)) { return $null }
    $candidate = Get-ChildItem $root -Directory -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1
    if ($candidate) { return $candidate.FullName }
    return $null
}

function Install-Rocm {
    Log-Section "AMD HIP SDK (ROCm na Windows, 7.1.1+)"

    # Detekcja istniejacej instalacji
    $existing = Find-HipInstallDir
    if ($existing) {
        $hipBin = Join-Path $existing 'bin'
        if (Test-Path (Join-Path $hipBin 'hipcc.bin.exe')) {
            Log-Ok "HIP SDK juz zainstalowane: $existing"
            Configure-RocmEnv -InstallDir $existing
            return
        }
        Log-Warn "Znaleziono $existing ale brak hipcc.bin.exe — instalacja moze byc uszkodzona, kontynuuje."
    }

    # Wymagamy Admina, bo Setup.exe -install bez UAC sie wywala.
    if (-not (Test-IsAdmin)) {
        Log-Error "Instalacja HIP SDK wymaga sesji Administratora."
        Log-Error "Zamknij to okno i otworz PowerShella jako Administrator, potem uruchom ponownie:"
        $cmdHint = "  scripts\setup.ps1 -Rocm"
        if ($RocmInstaller) { $cmdHint += " -RocmInstaller `"$RocmInstaller`"" }
        Log-Error $cmdHint
        return
    }

    # Skad bierzemy Setup.exe?
    $installerPath = $null
    if ($RocmInstaller) {
        if (-not (Test-Path $RocmInstaller)) {
            Log-Error "Plik nie istnieje: $RocmInstaller"
            return
        }
        $installerPath = (Resolve-Path $RocmInstaller).Path
        Log-Ok "Uzywam podanego instalatora: $installerPath"
    } else {
        # AMD wymaga akceptacji EULA przed pobraniem — nie ma stabilnego direct URL.
        $downloadPage = 'https://www.amd.com/en/developer/resources/rocm-hub/hip-sdk.html'
        Log-Warn "Nie podano -RocmInstaller. AMD wymaga recznego pobrania (EULA)."
        Log-Info "Otwieram strone download w przegladarce..."
        Start-Process $downloadPage

        Write-Host ''
        Write-Host '==============================================================' -ForegroundColor Yellow
        Write-Host '  Instrukcja:' -ForegroundColor Yellow
        Write-Host '  1. Wybierz HIP SDK 7.1.1 (lub nowszy) for Windows 11' -ForegroundColor Yellow
        Write-Host '  2. Zaakceptuj EULA i pobierz Setup.exe (~600 MB)' -ForegroundColor Yellow
        Write-Host '  3. Wklej tu pelna sciezke do pobranego Setup.exe' -ForegroundColor Yellow
        Write-Host '==============================================================' -ForegroundColor Yellow
        Write-Host ''
        $userInput = Read-Host 'Sciezka do Setup.exe (lub ENTER zeby pominac ROCm)'
        if (-not $userInput) {
            Log-Warn "Pominieto instalacje HIP SDK."
            return
        }
        $userInput = $userInput.Trim('"').Trim("'")
        if (-not (Test-Path $userInput)) {
            Log-Error "Plik nie istnieje: $userInput"
            return
        }
        $installerPath = (Resolve-Path $userInput).Path
    }

    # Silent install. Flagi z dokumentacji AMD:
    #   -install        cicha instalacja (wszystkie komponenty)
    #   -log <file>     log
    # Setup.exe nie wspiera selektywnej listy komponentow w trybie CLI —
    # leci pelny bundle (driver + runtime + libs).
    $logFile = Join-Path $env:TEMP 'amd-hip-sdk-install.log'
    Log-Info "Uruchamiam silent install (moze potrwac 10-20 min, log: $logFile)..."
    $proc = Start-Process -FilePath $installerPath `
        -ArgumentList '-install', '-log', $logFile `
        -NoNewWindow -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        Log-Error "AMD Setup.exe zakonczony exit code $($proc.ExitCode). Sprawdz log: $logFile"
        return
    }
    Log-Ok "HIP SDK zainstalowane"
    $script:Installed += 'AMD HIP SDK (ROCm)'

    Refresh-Path
    $installDir = Find-HipInstallDir
    if (-not $installDir) {
        Log-Warn "Setup zakonczyl OK, ale nie znalazlem C:\Program Files\AMD\ROCm\<ver>\."
        Log-Warn "Mozliwe ze wymagany restart systemu — zrestartuj i uruchom ponownie z -Rocm."
        return
    }
    Configure-RocmEnv -InstallDir $installDir
}

function Configure-RocmEnv {
    param([Parameter(Mandatory)][string]$InstallDir)

    $hipBin = Join-Path $InstallDir 'bin'
    $expected = "$InstallDir\"

    # HIP_PATH — instalator AMD ustawia ja na Machine scope sam. Sprawdzamy
    # co jest faktycznie w rejestrze (a nie w $env, ktore moze byc stale ze
    # starej sesji). Tylko jak brak / niezgodne, dopisujemy.
    $machineHip = [Environment]::GetEnvironmentVariable('HIP_PATH', 'Machine')
    if ($machineHip -eq $expected) {
        Log-Ok "HIP_PATH juz ustawione (Machine): $machineHip"
        if ($env:HIP_PATH -ne $expected) { $env:HIP_PATH = $expected }
    } elseif (Test-IsAdmin) {
        Set-PersistentEnv -Name 'HIP_PATH' -Value $expected -Scope 'Machine'
        $script:Installed += "HIP_PATH = $expected"
    } else {
        Log-Warn "HIP_PATH nie zgadza sie (rejestr=$machineHip, oczekiwane=$expected)."
        Log-Warn "Uruchom skrypt jako Administrator albo ustaw recznie."
    }

    # PATH — User scope wystarczy i nie wymaga Admina. AMD MSI nie
    # dodaje hipcc.exe do PATH, wiec tu jest praca skryptu.
    Add-PersistentPath -Path $hipBin -Scope 'User'
}

# --- Silero VAD (teams-bot asset) ---

function Get-SileroVad {
    Log-Section "Pobieranie assetow teams-bot (Silero VAD)"

    $repoRoot = Split-Path -Parent $PSScriptRoot
    $modelDir = Join-Path $repoRoot 'tentaflow-containers\agents\native\teams-bot\models'
    $modelFile = Join-Path $modelDir 'silero_vad.onnx'
    $sileroUrl = 'https://github.com/snakers4/silero-vad/raw/v5.1/src/silero_vad/data/silero_vad.onnx'

    if (Test-Path $modelFile) {
        $size = '{0:N1} MB' -f ((Get-Item $modelFile).Length / 1MB)
        Log-Ok "Silero VAD juz istnieje ($size)"
        return
    }

    if (-not (Test-Path $modelDir)) {
        New-Item -ItemType Directory -Path $modelDir -Force | Out-Null
    }

    Log-Info "Pobieram Silero VAD: $sileroUrl"
    try {
        $progressBackup = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $sileroUrl -OutFile $modelFile -UseBasicParsing
        $ProgressPreference = $progressBackup
        $size = '{0:N1} MB' -f ((Get-Item $modelFile).Length / 1MB)
        Log-Ok "Silero VAD pobrany ($size)"
        $script:Installed += 'silero_vad.onnx (teams-bot)'
    } catch {
        Log-Warn "Nie udalo sie pobrac Silero VAD: $_"
        Log-Warn "Bot uzyje fallback RMS (gorsza jakosc VAD)."
        if (Test-Path $modelFile) { Remove-Item $modelFile -Force }
    }
}

# --- Weryfikacja ---

function Verify-Installation {
    Log-Section "Weryfikacja instalacji"
    Refresh-Path

    $ok = $true

    function Check-Tool {
        param([string]$Cmd, [string]$Label, [scriptblock]$VersionScript, [switch]$Required)
        if (Test-Command $Cmd) {
            $ver = Invoke-NativeCapture $VersionScript | Select-Object -First 1
            Log-Ok "${Label}: $ver"
        } else {
            if ($Required) { Log-Error "${Label}: NIE ZNALEZIONO"; $script:ok = $false }
            else { Log-Warn "${Label}: NIE ZNALEZIONO" }
        }
    }

    Check-Tool 'cmake'      'cmake'      { & cmake --version }      -Required
    Check-Tool 'clang'      'clang'      { & clang --version }      -Required
    Check-Tool 'rustc'      'rustc'      { & rustc --version }      -Required
    Check-Tool 'cargo'      'cargo'      { & cargo --version }      -Required
    if (Test-Command 'pkg-config') {
        Check-Tool 'pkg-config' 'pkg-config' { & pkg-config --version }
    } else {
        Check-Tool 'pkgconf'    'pkgconf'    { & pkgconf --version }
    }
    Check-Tool 'ninja'      'ninja'      { & ninja --version }
    Check-Tool 'protoc'     'protoc'     { & protoc --version }     -Required
    Check-Tool 'git'        'git'        { & git --version }        -Required
    Check-Tool 'wasm-bindgen' 'wasm-bindgen' { & wasm-bindgen --version }

    if ($env:LIBCLANG_PATH -and (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
        Log-Ok "LIBCLANG_PATH: $env:LIBCLANG_PATH (libclang.dll obecny)"
    } else {
        Log-Error "LIBCLANG_PATH nie wskazuje na katalog z libclang.dll — bindgen nie zadziala"
        $ok = $false
    }

    $rustTargets = Invoke-NativeCapture { rustup target list --installed }
    foreach ($t in 'wasm32-wasip1','wasm32-unknown-unknown') {
        if ($rustTargets -match $t) {
            Log-Ok "$t : zainstalowany"
        } else {
            Log-Error "$t : BRAK"
            $ok = $false
        }
    }

    if ($Cuda) {
        if (Test-Command 'nvcc') {
            Log-Ok "nvcc (CUDA): $(Invoke-NativeCapture { nvcc --version } | Select-Object -Last 1)"
        } else {
            Log-Warn "nvcc (CUDA): NIE ZNALEZIONO (uruchom nowy shell po instalacji CUDA)"
        }
    }

    if ($Vulkan) {
        if (Test-Command 'vulkaninfo') {
            Log-Ok "vulkaninfo: dostepny"
        } else {
            Log-Warn "vulkaninfo: NIE ZNALEZIONO (otworz nowy shell — VULKAN_SDK ladowane przy starcie)"
        }
    }

    if ($Rocm) {
        $hipDir = Find-HipInstallDir
        if ($hipDir) {
            Log-Ok "HIP SDK: $hipDir"
            if ($env:HIP_PATH) {
                Log-Ok "HIP_PATH: $env:HIP_PATH"
            } else {
                Log-Warn "HIP_PATH nie widoczne w tej sesji — otworz nowy shell."
            }
            if (Test-Command 'hipcc') {
                Log-Ok "hipcc: $(Invoke-NativeCapture { hipcc --version } | Select-Object -First 1)"
            } else {
                Log-Warn "hipcc: NIE w PATH (otworz nowy shell)"
            }
        } else {
            Log-Warn "HIP SDK: NIE ZNALEZIONO w C:\Program Files\AMD\ROCm\"
        }
    }

    Write-Host ''
    if ($ok) {
        Log-Ok 'Wszystkie wymagane zaleznosci sa dostepne.'
    } else {
        Log-Error 'Brakuje niektorych wymaganych zaleznosci. Otworz NOWY PowerShell i sprobuj ponownie.'
        return $false
    }
    return $true
}

# --- Podsumowanie ---

function Print-Summary {
    Log-Section 'Podsumowanie'

    if ($script:Installed.Count -eq 0) {
        Log-Info 'Wszystko bylo juz zainstalowane, nic nie zmieniono.'
    } else {
        Log-Info 'Zainstalowane / zaktualizowane komponenty:'
        foreach ($item in $script:Installed) {
            Write-Host "  + $item" -ForegroundColor Green
        }
    }

    Write-Host ''
    Log-Warn 'WAZNE: zamknij to okno PowerShella i otworz NOWE,'
    Log-Warn 'zeby aktywowac LIBCLANG_PATH i zaktualizowane PATH.'
    Write-Host ''
    Log-Info 'Potem zbuduj TentaFlow:'
    Write-Host '  cd tentaflow' -ForegroundColor White
    Write-Host '  cargo build --release' -ForegroundColor White
    Write-Host ''
}

# --- Main ---

function Main {
    Write-Host ''
    Write-Host '  _____          _        _____ _               ' -ForegroundColor Cyan
    Write-Host ' |_   _|__ _ __ | |_ __ _|  ___| | _____      __' -ForegroundColor Cyan
    Write-Host '   | |/ _ \ ''_ \| __/ _` | |_  | |/ _ \ \ /\ / /' -ForegroundColor Cyan
    Write-Host '   | |  __/ | | | || (_| |  _| | | (_) \ V  V / ' -ForegroundColor Cyan
    Write-Host '   |_|\___|_| |_|\__\__,_|_|   |_|\___/ \_/\_/  ' -ForegroundColor Cyan
    Write-Host ''
    Write-Host 'Instalator zaleznosci (Windows)' -ForegroundColor White
    Write-Host ''

    Check-Prereqs

    Get-SileroVad

    Install-Base
    Install-Rust
    Install-WasmTargets
    Install-WasmBindgenCli

    if ($Cuda)   { Install-Cuda }
    if ($Vulkan) { Install-Vulkan }
    if ($Rocm)   { Install-Rocm }

    [void](Verify-Installation)
    Print-Summary
}

Main
