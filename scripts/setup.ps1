# =============================================================================
# File: scripts/setup.ps1
# Purpose: Installs everything needed to build TentaFlow on Windows x86_64:
#          Visual Studio C++ Build Tools, Git (Git Bash runs the shared
#          native-libs scripts), CMake, Ninja, LLVM (libclang for bindgen),
#          Python, protoc, pkg-config, GStreamer SDK, .NET SDK + WASI SDK
#          (C# addons), Rust + WASM targets + wasm-bindgen CLI, FFmpeg, and
#          the GPU toolkits (CUDA when an NVIDIA GPU is present, Vulkan SDK).
#
# Versions that must match across machines (GStreamer, CUDA, Vulkan SDK,
# .NET, WASI SDK) come from scripts/versions.env; wasm-bindgen from the root
# Cargo.toml.
#
# Usage:
#   scripts\setup.ps1              # base + CUDA (NVIDIA GPU present) + Vulkan SDK
#   scripts\setup.ps1 -Cuda        # CUDA even without a visible NVIDIA GPU
#   scripts\setup.ps1 -NoCuda      # skip CUDA
#   scripts\setup.ps1 -NoVulkan    # skip the Vulkan SDK
#   scripts\setup.ps1 -Minimal     # no GPU toolkits (slim / CPU builds)
#
# Notes:
#   - Does not need to run as Administrator: winget asks for elevation (UAC)
#     per machine-wide installer.
#   - AMD and Intel GPUs run on Vulkan - there is no ROCm/HIP build.
# =============================================================================

param(
    [switch]$Cuda,
    [switch]$NoCuda,
    [switch]$NoVulkan,
    [switch]$Minimal,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib\windows.ps1')

$script:Installed = @()

function Show-Usage {
    @"
TentaFlow - instalator zaleznosci (Windows x86_64)

Uzycie:
  scripts\setup.ps1 [-Cuda] [-NoCuda] [-NoVulkan] [-Minimal]

  (domyslnie)  baza + CUDA (gdy widoczne GPU NVIDIA) + Vulkan SDK
  -Cuda        CUDA toolkit nawet bez widocznego GPU NVIDIA
  -NoCuda      pomin CUDA toolkit
  -NoVulkan    pomin Vulkan SDK
  -Minimal     bez GPU toolkitow (edycja slim / build CPU)
"@ | Write-Host
}

if ($Help) { Show-Usage; exit 0 }

# PS 5.1 wraps native-command stderr in NativeCommandError under
# ErrorActionPreference=Stop (rustup and winget print info to stderr), so
# native commands whose stderr is noise run through this helper.
function Invoke-NativeCapture {
    param([Parameter(Mandatory)][scriptblock]$Script)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Script 2>$null
    } finally {
        $ErrorActionPreference = $prev
    }
}

function Set-PersistentEnv {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Value
    )
    if ([Environment]::GetEnvironmentVariable($Name, 'User') -ne $Value) {
        [Environment]::SetEnvironmentVariable($Name, $Value, 'User')
        $script:Installed += "$Name = $Value"
    }
    Set-Item -Path "Env:$Name" -Value $Value
    Log-Ok "$Name = $Value"
}

function Add-PersistentPath {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path $Path)) {
        Log-Warn "Path does not exist, not adding it to PATH: $Path"
        return
    }
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @(($current -split ';') | Where-Object { $_ -and $_.Trim() })
    if ($entries -notcontains $Path) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $Path) -join ';'), 'User')
        Log-Ok "Added to PATH (User): $Path"
    }
    if (-not (($env:Path -split ';') -contains $Path)) {
        $env:Path = "$env:Path;$Path"
    }
}

function Add-PersistentPathList {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Path
    )
    $existing = [Environment]::GetEnvironmentVariable($Name, 'User')
    $entries = @()
    if ($existing) { $entries = @(($existing -split ';') | Where-Object { $_ -and $_.Trim() }) }
    if ($entries -notcontains $Path) { $entries += $Path }
    Set-PersistentEnv -Name $Name -Value ($entries -join ';')
}

function Test-WingetPackage {
    param([string]$Id, [string]$Version = '')
    $out = Invoke-NativeCapture { winget list --id $Id --exact --accept-source-agreements --disable-interactivity } | Out-String
    if (-not ($out -match [regex]::Escape($Id))) { return $false }
    if (-not $Version) { return $true }
    return ($out -match "\s$([regex]::Escape($Version))(\s|$)")
}

# Installs a winget package unless present. With -Version the exact version is
# required (a different installed version is upgraded/replaced).
function Install-WingetPackage {
    param(
        [Parameter(Mandatory)][string]$Id,
        [string]$Label = '',
        [string]$Version = '',
        [string]$Custom = '',
        [string]$Override = '',
        [ValidateSet('', 'user', 'machine')][string]$Scope = ''
    )
    if (-not $Label) { $Label = $Id }
    if (Test-WingetPackage -Id $Id -Version $Version) {
        Log-Ok "$Label already installed ($Id $Version)"
        return
    }
    $wingetArgs = @('install', '--id', $Id, '--exact', '--silent', '--accept-source-agreements',
        '--accept-package-agreements', '--disable-interactivity')
    if ($Version) { $wingetArgs += @('--version', $Version, '--force') }
    if ($Custom) { $wingetArgs += @('--custom', $Custom) }
    if ($Override) { $wingetArgs += @('--override', $Override) }
    if ($Scope) { $wingetArgs += @('--scope', $Scope) }
    Log-Info "Installing $Label ($Id $Version)..."
    & winget @wingetArgs
    $code = $LASTEXITCODE
    Update-SessionEnvironment
    if ($code -ne 0 -and -not (Test-WingetPackage -Id $Id -Version $Version)) {
        throw "winget install $Id failed (exit $code)."
    }
    Log-Ok "$Label installed"
    $script:Installed += "$Label $Version"
}

# --- Checks ------------------------------------------------------------------

function Check-Prereqs {
    Log-Section 'Prerequisites'
    if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw 'Only Windows x86_64 is supported.'
    }
    if (-not (Test-Command 'winget')) {
        throw "winget is missing. Install 'App Installer' from the Microsoft Store: https://apps.microsoft.com/detail/9NBLGGH4NNS1"
    }
    Log-Ok "winget $(Invoke-NativeCapture { winget --version } | Select-Object -First 1)"
    Log-Info "PowerShell $($PSVersionTable.PSVersion), $((Get-CimInstance Win32_OperatingSystem).Caption)"
}

# --- Visual Studio C++ toolset -------------------------------------------------

function Install-VisualStudio {
    Log-Section 'Visual Studio C++ Build Tools'
    $vs = Get-VsInstallPath
    if ($vs) {
        Log-Ok "C++ x64 toolset found: $vs"
        return
    }
    # The VCTools workload brings MSVC and, with recommended components, the
    # Windows SDK. Visual Studio 2022 is what the CUDA toolkit pinned in
    # versions.env supports; any newer VS already installed is accepted above.
    Install-WingetPackage -Id 'Microsoft.VisualStudio.2022.BuildTools' -Label 'Visual Studio 2022 Build Tools' `
        -Override '--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    if (-not (Get-VsInstallPath)) {
        throw 'Visual Studio Build Tools installed, but vswhere does not report the C++ x64 toolset. A reboot may be pending.'
    }
}

# --- Base tools ------------------------------------------------------------------

function Find-LlvmBin {
    foreach ($dir in @("$env:ProgramFiles\LLVM\bin", "${env:ProgramFiles(x86)}\LLVM\bin")) {
        if (Test-Path (Join-Path $dir 'libclang.dll')) { return $dir }
    }
    return $null
}

function Find-ProtocExe {
    $cmd = Get-Command 'protoc.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # winget's Google.Protobuf only adds a link under WinGet\Links; build.rs of
    # prost-build reads PROTOC, so the real executable is located here.
    $packages = Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages'
    if (Test-Path $packages) {
        $found = Get-ChildItem $packages -Directory -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'Google.Protobuf*' } |
            ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter 'protoc.exe' -ErrorAction SilentlyContinue } |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

# pkg-config-lite is a zip package: winget unpacks it under WinGet\Packages and
# links nothing. The pkg-config first on PATH is often a different one - the
# GitHub runner image carries Strawberry Perl's, which answers --modversion for
# the first module only - so builds are pointed at this one through PKG_CONFIG.
function Find-PkgConfigExe {
    foreach ($packages in @((Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages'),
                            (Join-Path $env:ProgramFiles 'WinGet\Packages'))) {
        if (-not (Test-Path $packages)) { continue }
        $found = Get-ChildItem $packages -Directory -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'bloodrock.pkg-config-lite*' } |
            ForEach-Object { Get-ChildItem $_.FullName -Recurse -Filter 'pkg-config.exe' -ErrorAction SilentlyContinue } |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

function Install-BaseTools {
    Log-Section 'Base tools'
    Install-WingetPackage -Id 'Git.Git' -Label 'Git for Windows'
    Install-WingetPackage -Id 'Kitware.CMake' -Label 'CMake'
    Install-WingetPackage -Id 'Ninja-build.Ninja' -Label 'Ninja'
    Install-WingetPackage -Id 'LLVM.LLVM' -Label 'LLVM/Clang'
    Install-WingetPackage -Id 'Google.Protobuf' -Label 'protoc'
    # pkgconf has no Windows installer in winget; pkg-config-lite has no glib
    # dependency. gstreamer-sys finds the SDK through it.
    Install-WingetPackage -Id 'bloodrock.pkg-config-lite' -Label 'pkg-config-lite'
    # ML Studio decodes recording segments with the ffmpeg CLI at runtime.
    Install-WingetPackage -Id 'Gyan.FFmpeg' -Label 'FFmpeg'

    $llvm = Find-LlvmBin
    if (-not $llvm) { throw 'libclang.dll not found after installing LLVM.' }
    Set-PersistentEnv -Name 'LIBCLANG_PATH' -Value $llvm
    Add-PersistentPath -Path $llvm

    $protoc = Find-ProtocExe
    if (-not $protoc) { throw 'protoc.exe not found after installing Google.Protobuf.' }
    Set-PersistentEnv -Name 'PROTOC' -Value $protoc
    Add-PersistentPath -Path (Split-Path -Parent $protoc)

    $pkgConfig = Find-PkgConfigExe
    if (-not $pkgConfig) { throw 'pkg-config.exe (pkg-config-lite) not found after installation.' }
    # The pkg-config crate behind gstreamer-sys honours PKG_CONFIG before PATH.
    Set-PersistentEnv -Name 'PKG_CONFIG' -Value $pkgConfig
    Add-PersistentPath -Path (Split-Path -Parent $pkgConfig)
}

function Install-Python {
    Log-Section 'Python 3.11+'
    $python = Find-Python
    if (-not $python) {
        Install-WingetPackage -Id 'Python.Python.3.13' -Label 'Python 3.13'
        $python = Find-Python
    }
    if (-not $python) {
        throw 'Python 3.11+ still not found. Open a new PowerShell and run setup again.'
    }
    $env:TENTAFLOW_PYTHON = $python
    Log-Ok "Python: $python"
}

# --- GStreamer ---------------------------------------------------------------------

function Find-GstreamerRoot {
    # Per-user install (what setup does: no UAC) lands in {autopf} of the
    # user, i.e. %LOCALAPPDATA%\Programs; a machine-wide one in Program Files.
    $roots = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\gstreamer\1.0\msvc_x86_64'),
        (Join-Path $env:ProgramFiles 'gstreamer\1.0\msvc_x86_64'),
        'C:\gstreamer\1.0\msvc_x86_64'
    )
    foreach ($root in $roots) {
        if (Test-Path (Join-Path $root 'lib\pkgconfig\gstreamer-1.0.pc')) { return $root }
    }
    return $null
}

function Get-GstreamerVersion {
    param([string]$Root)
    $pc = Join-Path $Root 'lib\pkgconfig\gstreamer-1.0.pc'
    $line = Get-Content $pc | Where-Object { $_ -like 'Version:*' } | Select-Object -First 1
    if ($line) { return $line.Substring(8).Trim() }
    return $null
}

function Install-Gstreamer {
    Log-Section "GStreamer SDK $env:GSTREAMER_VERSION (camera, RTSP, video)"
    $root = Find-GstreamerRoot
    $current = $null
    if ($root) { $current = Get-GstreamerVersion -Root $root }
    if ($current -ne $env:GSTREAMER_VERSION) {
        if ($current) { Log-Info "Installed GStreamer $current, required $env:GSTREAMER_VERSION" }
        # Since 1.28 the SDK is one Inno Setup installer. The `devel` type is
        # runtime + headers/pkg-config files with every plugin set (good, bad,
        # ugly, libav) the camera pipeline resolves at runtime.
        Install-WingetPackage -Id 'gstreamerproject.gstreamer' -Label 'GStreamer SDK' -Version $env:GSTREAMER_VERSION `
            -Scope user -Custom '/TYPE=devel /TASKS=environment_variables,registry_install_dir'
        $root = Find-GstreamerRoot
    }
    if (-not $root -or (Get-GstreamerVersion -Root $root) -ne $env:GSTREAMER_VERSION) {
        throw "GStreamer $env:GSTREAMER_VERSION (MSVC x86_64, with development files) not found after installation."
    }
    Add-PersistentPathList -Name 'PKG_CONFIG_PATH' -Path (Join-Path $root 'lib\pkgconfig')
    Add-PersistentPath -Path (Join-Path $root 'bin')
    Set-PersistentEnv -Name 'GSTREAMER_1_0_ROOT_MSVC_X86_64' -Value "$root\"
}

# --- Rust ------------------------------------------------------------------------------

function Install-Rust {
    Log-Section 'Rust (rustup, stable MSVC, WASM targets)'
    if (-not (Test-Command 'rustup')) {
        Install-WingetPackage -Id 'Rustlang.Rustup' -Label 'rustup'
    }
    if (-not (Test-Command 'rustup')) {
        throw 'rustup is not on PATH after installation. Open a new PowerShell and run setup again.'
    }
    Invoke-NativeCapture { rustup toolchain install stable-x86_64-pc-windows-msvc --profile minimal --component rustfmt,clippy } | Out-Host
    $default = Invoke-NativeCapture { rustup default } | Out-String
    if ($default -notmatch 'windows-msvc') {
        Invoke-NativeCapture { rustup default stable-x86_64-pc-windows-msvc } | Out-Host
    }
    $installed = Invoke-NativeCapture { rustup target list --installed } | Out-String
    foreach ($target in @('wasm32-wasip1', 'wasm32-unknown-unknown')) {
        if ($installed -notmatch [regex]::Escape($target)) {
            Invoke-NativeCapture { rustup target add $target } | Out-Host
            $script:Installed += $target
        }
    }
    Log-Ok "$(Invoke-NativeCapture { rustc --version } | Select-Object -First 1)"
}

function Install-WasmBindgenCli {
    $version = & $env:TENTAFLOW_PYTHON (Join-Path $PSScriptRoot 'workspace-version.py') 'wasm-bindgen'
    if ($LASTEXITCODE -ne 0 -or -not $version) { throw 'Cannot read the wasm-bindgen version from Cargo.toml.' }
    $version = "$version".Trim()
    Log-Section "wasm-bindgen CLI $version"
    if (Test-Command 'wasm-bindgen') {
        $current = ((Invoke-NativeCapture { wasm-bindgen --version } | Select-Object -First 1) -split '\s+')[-1]
        if ($current -eq $version) {
            Log-Ok "wasm-bindgen $current already installed"
            return
        }
        Log-Info "wasm-bindgen $current != $version - reinstalling"
    }
    & cargo install wasm-bindgen-cli --version $version --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo install wasm-bindgen-cli $version failed." }
    $script:Installed += "wasm-bindgen-cli $version"
}

# --- .NET + WASI SDK (C# addons) -------------------------------------------------------

function Install-DotnetSdk {
    $major = ($env:DOTNET_SDK_CHANNEL -split '\.')[0]
    Log-Section ".NET $env:DOTNET_SDK_CHANNEL SDK (C# addons)"
    $sdks = ''
    if (Test-Command 'dotnet') { $sdks = Invoke-NativeCapture { dotnet --list-sdks } | Out-String }
    if ($sdks -match "(?m)^$major\.") {
        Log-Ok ".NET $major SDK present"
        return
    }
    Install-WingetPackage -Id "Microsoft.DotNet.SDK.$major" -Label ".NET $major SDK"
}

function Install-WasiSdk {
    $version = $env:WASI_SDK_VERSION
    $name = "wasi-sdk-$version-x86_64-windows"
    $cache = Get-NativeCacheDir
    $target = Join-Path $cache $name
    Log-Section "WASI SDK $version (C# addons)"
    if (-not (Test-Path (Join-Path $target 'share\wasi-sysroot'))) {
        New-Item -ItemType Directory -Force -Path $cache | Out-Null
        $archive = Join-Path $cache "$name.tar.gz"
        $major = ($version -split '\.')[0]
        $url = "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$major/$name.tar.gz"
        Log-Info "Downloading $url"
        $progress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
        $ProgressPreference = $progress
        $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
        if ($actual -ne $env:WASI_SDK_SHA256_WINDOWS_X86_64) {
            Remove-Item $archive -Force
            throw "WASI SDK checksum mismatch: $actual (scripts/versions.env: $env:WASI_SDK_SHA256_WINDOWS_X86_64)"
        }
        # The system bsdtar, not Git's GNU tar: it takes native paths as-is.
        & "$env:SystemRoot\System32\tar.exe" -xzf $archive -C $cache
        if ($LASTEXITCODE -ne 0) { throw "Extracting $archive failed." }
        Remove-Item $archive -Force
        $script:Installed += "WASI SDK $version"
    }
    # Compile + link catches a missing sysroot or a wrong host binary.
    $probe = Join-Path ([IO.Path]::GetTempPath()) "tentaflow-wasi-$PID"
    New-Item -ItemType Directory -Force -Path $probe | Out-Null
    try {
        Set-Content -Path (Join-Path $probe 'check.c') -Value 'int main(void) { return 0; }' -Encoding ascii
        & (Join-Path $target 'bin\clang.exe') "--sysroot=$(Join-Path $target 'share\wasi-sysroot')" (Join-Path $probe 'check.c') -o (Join-Path $probe 'check.wasm')
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $probe 'check.wasm'))) {
            throw "WASI SDK in $target cannot compile a test program."
        }
    } finally {
        Remove-Item $probe -Recurse -Force -ErrorAction SilentlyContinue
    }
    Log-Ok "WASI SDK: $target"
}

# --- GPU toolkits -------------------------------------------------------------------------

function Install-Cuda {
    $version = $env:CUDA_TOOLKIT_VERSION
    Log-Section "NVIDIA CUDA Toolkit $version"
    $pathVar = 'CUDA_PATH_V' + ($version -replace '\.', '_')
    $root = [Environment]::GetEnvironmentVariable($pathVar, 'Machine')
    if (-not ($root -and (Test-Path (Join-Path $root 'bin\nvcc.exe')))) {
        Install-WingetPackage -Id 'Nvidia.CUDA' -Label 'CUDA Toolkit' -Version $version
        $root = [Environment]::GetEnvironmentVariable($pathVar, 'Machine')
    }
    if (-not ($root -and (Test-Path (Join-Path $root 'bin\nvcc.exe')))) {
        throw "CUDA $version not found after installation ($pathVar)."
    }
    # Several toolkits can live side by side; builds use the pinned one
    # (scripts\lib\windows.ps1 Use-PinnedCuda puts it first). A bin of another
    # version that setup added to the user PATH earlier goes away.
    Set-PersistentEnv -Name 'CUDA_PATH' -Value $root
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $pinnedBin = (Join-Path $root 'bin').TrimEnd('\')
    $kept = @(($userPath -split ';') | Where-Object {
        $isCudaBin = $_ -match '\\NVIDIA GPU Computing Toolkit\\CUDA\\v[0-9.]+\\bin\\?$'
        $_ -and -not ($isCudaBin -and ($_.TrimEnd('\') -ine $pinnedBin))
    })
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
    Add-PersistentPath -Path (Join-Path $root 'bin')
    Log-Ok "nvcc: $(Invoke-NativeCapture { & (Join-Path $root 'bin\nvcc.exe') --version } | Select-Object -Last 2 | Select-Object -First 1)"
}

function Install-Vulkan {
    $version = $env:VULKAN_SDK_VERSION
    Log-Section "Vulkan SDK $version"
    # vulkaninfo alone ships with every GPU driver (System32); the SDK is what
    # provides glslc, headers and vulkan-1.lib for ggml-vulkan.
    $root = Join-Path 'C:\VulkanSDK' $version
    if (-not (Test-Path (Join-Path $root 'Bin\glslc.exe'))) {
        Install-WingetPackage -Id 'KhronosGroup.VulkanSDK' -Label 'Vulkan SDK' -Version $version
    }
    if (-not (Test-Path (Join-Path $root 'Bin\glslc.exe'))) {
        throw "Vulkan SDK $version not found in $root after installation."
    }
    Set-PersistentEnv -Name 'VULKAN_SDK' -Value $root
    Add-PersistentPath -Path (Join-Path $root 'Bin')
}

# --- Teams bot asset --------------------------------------------------------------------

function Get-SileroVad {
    Log-Section 'teams-bot assets (Silero VAD)'
    $modelDir = Join-Path $script:TentaflowRoot 'tentaflow-containers\agents\native\teams-bot\models'
    $modelFile = Join-Path $modelDir 'silero_vad.onnx'
    if (Test-Path $modelFile) {
        Log-Ok 'Silero VAD present'
        return
    }
    New-Item -ItemType Directory -Force -Path $modelDir | Out-Null
    $url = 'https://github.com/snakers4/silero-vad/raw/v5.1/src/silero_vad/data/silero_vad.onnx'
    try {
        $progress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $url -OutFile $modelFile -UseBasicParsing
        $ProgressPreference = $progress
        Log-Ok 'Silero VAD downloaded'
        $script:Installed += 'silero_vad.onnx (teams-bot)'
    } catch {
        Log-Warn "Silero VAD download failed: $_ - the bot falls back to RMS VAD."
        if (Test-Path $modelFile) { Remove-Item $modelFile -Force }
    }
}

# --- Verification ------------------------------------------------------------------------

function Verify-Installation {
    param([bool]$WithCuda, [bool]$WithVulkan)
    Log-Section 'Verification'
    Update-SessionEnvironment
    $ok = $true
    foreach ($tool in @('git', 'cmake', 'ninja', 'clang', 'protoc', 'cargo', 'rustc', 'wasm-bindgen', 'dotnet', 'ffmpeg')) {
        if (Test-Command $tool) {
            Log-Ok "$tool"
        } else {
            Log-Error "${tool}: NOT FOUND"
            $ok = $false
        }
    }
    try { [void](Find-GitBash); Log-Ok 'Git Bash' } catch { Log-Error $_; $ok = $false }
    if (Get-VsInstallPath) { Log-Ok "MSVC x64: $(Get-VsInstallPath)" } else { Log-Error 'MSVC x64 toolset: NOT FOUND'; $ok = $false }
    if ($env:LIBCLANG_PATH -and (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
        Log-Ok "LIBCLANG_PATH: $env:LIBCLANG_PATH"
    } else {
        Log-Error 'LIBCLANG_PATH does not point at libclang.dll'
        $ok = $false
    }
    # All output first, exit code second: `| Select-Object -First 1` would stop
    # the pipeline after the first of three lines and kill pkg-config, leaving
    # a non-zero $LASTEXITCODE whenever the process had not exited yet.
    $gstModules = @('gstreamer-1.0', 'gstreamer-app-1.0', 'gstreamer-video-1.0')
    if ($env:PKG_CONFIG -and (Test-Path $env:PKG_CONFIG)) {
        Log-Ok "PKG_CONFIG: $env:PKG_CONFIG"
    } else {
        Log-Error 'PKG_CONFIG does not point at pkg-config.exe'
        $ok = $false
    }
    $gstVersions = @(Invoke-NativeCapture { & $env:PKG_CONFIG --modversion @gstModules })
    $gstCode = $LASTEXITCODE
    $gstWrong = @($gstVersions | Where-Object { $_.Trim() -ne $env:GSTREAMER_VERSION })
    if ($gstCode -eq 0 -and $gstVersions.Count -eq $gstModules.Count -and $gstWrong.Count -eq 0) {
        Log-Ok "GStreamer (pkg-config): $($gstVersions[0])"
    } else {
        Log-Error "GStreamer $env:GSTREAMER_VERSION not visible through pkg-config for $($gstModules -join ', ') (exit $gstCode, got '$($gstVersions -join ', ')')"
        $ok = $false
    }
    $targets = Invoke-NativeCapture { rustup target list --installed } | Out-String
    foreach ($target in @('wasm32-wasip1', 'wasm32-unknown-unknown')) {
        if ($targets -match [regex]::Escape($target)) { Log-Ok $target } else { Log-Error "${target}: MISSING"; $ok = $false }
    }
    if ($WithCuda) {
        if ($env:CUDA_PATH -and (Test-Path (Join-Path $env:CUDA_PATH 'bin\nvcc.exe'))) { Log-Ok "CUDA_PATH: $env:CUDA_PATH" } else { Log-Error 'CUDA toolkit: NOT FOUND'; $ok = $false }
    }
    if ($WithVulkan) {
        if ($env:VULKAN_SDK -and (Test-Path (Join-Path $env:VULKAN_SDK 'Bin\glslc.exe'))) { Log-Ok "VULKAN_SDK: $env:VULKAN_SDK" } else { Log-Error 'Vulkan SDK: NOT FOUND'; $ok = $false }
    }
    if (-not $ok) {
        throw 'Some required dependencies are missing (see above).'
    }
    Log-Ok 'All required dependencies are available.'
}

function Print-Summary {
    param([bool]$WithCuda, [bool]$WithVulkan)
    Log-Section 'Summary'
    if ($script:Installed.Count -eq 0) {
        Log-Info 'Everything was already installed.'
    } else {
        foreach ($item in $script:Installed) { Write-Host "  + $item" -ForegroundColor Green }
    }
    $backend = 'cpu'
    if ($WithVulkan) { $backend = 'vulkan' }
    if ($WithCuda) { $backend = 'cuda' }
    Write-Host ''
    Log-Warn 'Open a NEW terminal so it sees the updated PATH and variables, then:'
    Write-Host "  1. scripts\native-libs\build-all.ps1 -Backend $backend     # native libraries (not in the repo)" -ForegroundColor White
    Write-Host "  2. scripts\build.ps1 -Edition full -Backend $backend --release" -ForegroundColor White
    Write-Host '     (slim: build-all.ps1 -Edition slim, then build.ps1 -Edition slim --release)' -ForegroundColor White
    Write-Host ''
}

# --- Main ------------------------------------------------------------------------------------

Check-Prereqs
Update-SessionEnvironment
[void](Import-TentaflowVersions)

$withCuda = $false
if (-not $Minimal -and -not $NoCuda) {
    $withCuda = $Cuda -or (Test-NvidiaGpu)
}
$withVulkan = -not $Minimal -and -not $NoVulkan

Get-SileroVad
Install-VisualStudio
Install-BaseTools
Install-Python
Install-Gstreamer
Install-Rust
Install-WasmBindgenCli
Install-DotnetSdk
Install-WasiSdk
Set-PersistentEnv -Name 'TENTAFLOW_NATIVE_CACHE' -Value (Get-NativeCacheDir)
if ($withCuda) { Install-Cuda } else { Log-Info 'CUDA skipped (no NVIDIA GPU, -NoCuda or -Minimal).' }
if ($withVulkan) { Install-Vulkan } else { Log-Info 'Vulkan SDK skipped (-NoVulkan or -Minimal).' }

Verify-Installation -WithCuda $withCuda -WithVulkan $withVulkan
Print-Summary -WithCuda $withCuda -WithVulkan $withVulkan
