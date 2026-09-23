# ===== File: scripts/lib/windows.ps1 - shared helpers of the Windows build and setup scripts =====
#
# Dot-sourced by scripts/setup.ps1, scripts/build.ps1 and
# scripts/native-libs/build-all.ps1. Windows PowerShell 5.1 compatible: no
# ternary, no null-coalescing, no pipeline chain operators.

$script:TentaflowRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

function Log-Info    { param([string]$Msg) Write-Host "[INFO] $Msg" -ForegroundColor Blue }
function Log-Ok      { param([string]$Msg) Write-Host "[OK] $Msg" -ForegroundColor Green }
function Log-Warn    { param([string]$Msg) Write-Host "[WARN] $Msg" -ForegroundColor Yellow }
# On GitHub Actions an error also becomes an annotation: job logs need admin
# rights to read, annotations are what a failed run shows everyone else.
function Log-Error {
    param([string]$Msg)
    Write-Host "[ERROR] $Msg" -ForegroundColor Red
    if ($env:GITHUB_ACTIONS -eq 'true') { Write-Host "::error::$Msg" }
}
function Log-Section { param([string]$Msg) Write-Host "`n=== $Msg ===`n" -ForegroundColor Cyan }

function Test-Command {
    param([string]$Name)
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

# Reads scripts/versions.env (the single source of versions) into the process
# environment. Same rules as scripts/lib/versions.sh: parsed, never executed,
# and a variable already set in the environment wins.
function Import-TentaflowVersions {
    $file = Join-Path $script:TentaflowRoot 'scripts\versions.env'
    if (-not (Test-Path $file)) { throw "Missing $file" }
    $versions = @{}
    $lineNo = 0
    foreach ($line in [IO.File]::ReadAllLines($file)) {
        $lineNo++
        if ($line -match '^\s*$' -or $line.StartsWith('#')) { continue }
        if ($line -notmatch '^([A-Z][A-Z0-9_]*)=([^\s"''`$]*)$') {
            throw "${file}:${lineNo}: malformed line: $line"
        }
        $versions[$Matches[1]] = $Matches[2]
        if (-not (Test-Path "Env:$($Matches[1])")) {
            Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2]
        }
    }
    return $versions
}

# Merges Machine + User environment into this process. Setup writes PATH and
# variables (VULKAN_SDK, LIBCLANG_PATH, PROTOC, ...) to the registry, and a shell
# started earlier - or by an IDE - does not see them yet. Entries the current
# process added on its own stay at the end of PATH.
function Update-SessionEnvironment {
    foreach ($scope in @('Machine', 'User')) {
        $vars = [Environment]::GetEnvironmentVariables($scope)
        foreach ($name in $vars.Keys) {
            if ($name -ieq 'Path') { continue }
            if (-not (Test-Path "Env:$name")) {
                Set-Item -Path "Env:$name" -Value $vars[$name]
            }
        }
    }
    # Variables the installers own: take the registry value even when the
    # process inherited an older one (e.g. CUDA_PATH of a replaced toolkit).
    foreach ($name in @('CUDA_PATH', 'VULKAN_SDK', 'GSTREAMER_1_0_ROOT_MSVC_X86_64')) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Machine')
        $userValue = [Environment]::GetEnvironmentVariable($name, 'User')
        if ($userValue) { $value = $userValue }
        if ($value) { Set-Item -Path "Env:$name" -Value $value }
    }
    $env:Path = Merge-PathList -Name 'Path'
    # A list, not a value: setup.ps1 records GStreamer's pkgconfig directory in
    # the registry, while a tool that ran earlier (actions/setup-python, a conda
    # shell) may already have set its own. Taking either alone loses the other,
    # and without GStreamer's entry glib-sys cannot find glib-2.0.pc.
    $pkgConfigPath = Merge-PathList -Name 'PKG_CONFIG_PATH'
    if ($pkgConfigPath) { $env:PKG_CONFIG_PATH = $pkgConfigPath }
    Clear-InvalidCompilerEnv
}

# Machine, then User, then this process' entries of a ';'-separated list,
# without duplicates. The process keeps whatever it added on its own.
function Merge-PathList {
    param([Parameter(Mandatory)][string]$Name)
    $entries = New-Object System.Collections.Generic.List[string]
    $seen = @{}
    $sources = @(
        [Environment]::GetEnvironmentVariable($Name, 'Machine'),
        [Environment]::GetEnvironmentVariable($Name, 'User'),
        [Environment]::GetEnvironmentVariable($Name, 'Process')
    )
    foreach ($source in $sources) {
        if (-not $source) { continue }
        foreach ($entry in ($source -split ';')) {
            $trimmed = $entry.Trim()
            if (-not $trimmed) { continue }
            $key = $trimmed.TrimEnd('\').ToLowerInvariant()
            if ($seen.ContainsKey($key)) { continue }
            $seen[$key] = $true
            $entries.Add($trimmed)
        }
    }
    return ($entries -join ';')
}

# cc-rs and CMake run whatever CC/CXX/AR name. A value that is not an existing
# file (e.g. a directory left behind in the Machine environment) breaks every C
# build with "access denied", so it is dropped for this process with a warning
# instead of letting cc-rs execute a folder.
$script:WarnedCompilerEnv = @{}
function Clear-InvalidCompilerEnv {
    foreach ($name in @('CC', 'CXX', 'AR')) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        if (-not $value) { continue }
        $exe = ($value -split '\s+')[0].Trim('"')
        $found = (Test-Path -LiteralPath $exe -PathType Leaf) -or (Get-Command $exe -CommandType Application -ErrorAction SilentlyContinue)
        if (-not $found) {
            if (-not $script:WarnedCompilerEnv[$name]) {
                Log-Warn "$name='$value' is not a compiler executable - ignoring it for this build. Remove it from the system environment (sysdm.cpl -> Environment Variables)."
                $script:WarnedCompilerEnv[$name] = $true
            }
            [Environment]::SetEnvironmentVariable($name, $null, 'Process')
        }
    }
}

# Several CUDA toolkits can be installed side by side and each installer adds
# its bin to PATH. The pinned one (CUDA_PATH, set by setup.ps1 from
# versions.env) goes first, so nvcc, CMake and cudarc all see the same toolkit.
function Use-PinnedCuda {
    if (-not $env:CUDA_PATH) { return }
    $bin = Join-Path $env:CUDA_PATH 'bin'
    if (-not (Test-Path (Join-Path $bin 'nvcc.exe'))) { return }
    $rest = ($env:Path -split ';') | Where-Object { $_ -and ($_.TrimEnd('\') -ine $bin.TrimEnd('\')) }
    $env:Path = (@($bin) + $rest) -join ';'
}

function Get-VsWherePath {
    $path = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $path) { return $path }
    return $null
}

# Newest Visual Studio 2022+ (any edition, Build Tools included) that has the
# x64 C++ toolset. Found through vswhere, so the install location is irrelevant.
function Get-VsInstallPath {
    $vswhere = Get-VsWherePath
    if (-not $vswhere) { return $null }
    $path = & $vswhere -latest -products * -version '[17.0,)' `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath 2>$null | Select-Object -First 1
    if ($path -and (Test-Path $path)) { return $path }
    return $null
}

# Loads the x64 MSVC environment (cl, link, INCLUDE, LIB, Windows SDK) into
# this process. cmake + Ninja and nvcc need cl on PATH; cc-rs would find MSVC
# through the registry, native CMake builds do not.
function Enter-VsDevEnvironment {
    if ($env:VCINSTALLDIR -and (Test-Command 'cl.exe') -and ($env:VSCMD_ARG_TGT_ARCH -eq 'x64')) {
        Log-Ok "MSVC x64 environment already loaded: $env:VCINSTALLDIR"
        return
    }
    $vs = Get-VsInstallPath
    if (-not $vs) {
        throw 'No Visual Studio 2022+ with the C++ x64 toolset found. Run scripts\setup.ps1.'
    }
    $launcher = Join-Path $vs 'Common7\Tools\Launch-VsDevShell.ps1'
    if (-not (Test-Path $launcher)) { throw "Missing $launcher" }
    Log-Info "Loading MSVC x64 environment: $vs"
    # Launch-VsDevShell runs vswhere.exe from PATH, and its installer directory
    # is not on PATH by default.
    $installer = Split-Path -Parent (Get-VsWherePath)
    if (-not (($env:Path -split ';') -contains $installer)) { $env:Path = "$env:Path;$installer" }
    & $launcher -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
    if (-not (Test-Command 'cl.exe')) {
        throw "Launch-VsDevShell finished but cl.exe is not on PATH ($vs)."
    }
    Log-Ok "MSVC environment: $env:VCToolsVersion ($vs)"
}

# CMake builds run with Ninja: one flat build, readable logs, and none of the
# MSBuild custom-step failures on localized Windows. The generator-instance
# variables VsDevShell exports are only valid for the Visual Studio generator
# and make CMake abort under Ninja.
function Set-NinjaGenerator {
    if (-not (Test-Command 'ninja.exe')) { throw 'ninja is not on PATH. Run scripts\setup.ps1.' }
    $env:CMAKE_GENERATOR = 'Ninja'
    foreach ($name in @('CMAKE_GENERATOR_INSTANCE', 'CMAKE_GENERATOR_PLATFORM', 'CMAKE_GENERATOR_TOOLSET')) {
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
}

# bash.exe of Git for Windows. Never a bare `bash` from PATH: on Windows that
# is often C:\Windows\System32\bash.exe, which runs the script inside WSL.
function Find-GitBash {
    $roots = New-Object System.Collections.Generic.List[string]
    foreach ($hive in @('HKLM:\SOFTWARE\GitForWindows', 'HKCU:\SOFTWARE\GitForWindows')) {
        $item = Get-ItemProperty -Path $hive -Name InstallPath -ErrorAction SilentlyContinue
        if ($item -and $item.InstallPath) { $roots.Add($item.InstallPath) }
    }
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($git) { $roots.Add((Split-Path -Parent (Split-Path -Parent $git.Source))) }
    foreach ($root in $roots) {
        $bash = Join-Path $root 'bin\bash.exe'
        if (Test-Path $bash) { return $bash }
    }
    throw 'Git for Windows (bin\bash.exe) not found. Run scripts\setup.ps1.'
}

# Python 3.11+ interpreter path. The WindowsApps python.exe is the Microsoft
# Store installer alias, not Python, and is skipped.
function Find-Python {
    $candidates = @()
    if ($env:TENTAFLOW_PYTHON) { $candidates += , @($env:TENTAFLOW_PYTHON) }
    $candidates += , @('py', '-3')
    $candidates += , @('python')
    $candidates += , @('python3')
    # A fresh per-user install is not on this process' PATH yet.
    foreach ($base in @((Join-Path $env:LOCALAPPDATA 'Programs\Python'), $env:ProgramFiles)) {
        Get-ChildItem $base -Directory -Filter 'Python3*' -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object { $candidates += , @((Join-Path $_.FullName 'python.exe')) }
    }
    foreach ($candidate in $candidates) {
        $exe = $candidate[0]
        $cmd = Get-Command $exe -ErrorAction SilentlyContinue
        if (-not $cmd) { continue }
        if ($cmd.Source -like '*\WindowsApps\*') { continue }
        $extra = @()
        if ($candidate.Count -gt 1) { $extra = $candidate[1..($candidate.Count - 1)] }
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $path = & $cmd.Source @extra -c 'import sys; sys.exit(1) if sys.version_info < (3, 11) else print(sys.executable)' 2>$null
        $code = $LASTEXITCODE
        $ErrorActionPreference = $prev
        if ($code -eq 0 -and $path) { return "$path".Trim() }
    }
    return $null
}

function Test-NvidiaGpu {
    if (-not (Test-Command 'nvidia-smi.exe')) { return $false }
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $out = & nvidia-smi.exe -L 2>$null
    $ErrorActionPreference = $prev
    return ($LASTEXITCODE -eq 0 -and ($out -match '^GPU '))
}

# Default native-libs cache, shared with scripts/native-libs/common.sh and the
# WASI SDK lookup of tentaflow-core/build.rs.
# Same default as scripts/native-libs/common.sh: the drive root, because
# link.exe cannot open paths past MAX_PATH and the build trees under a user
# profile run past it (see default_native_cache there).
function Get-NativeCacheDir {
    if ($env:TENTAFLOW_NATIVE_CACHE) { return $env:TENTAFLOW_NATIVE_CACHE }
    return (Join-Path "$env:SystemDrive\" 'tentaflow-native')
}

# Runs a script through Git Bash with this process' environment (MSVC, CUDA,
# Vulkan) and returns its exit code.
function Invoke-GitBash {
    param(
        [Parameter(Mandatory)][string]$Script,
        [string[]]$Arguments = @()
    )
    $bash = Find-GitBash
    $scriptPath = (Resolve-Path $Script).Path -replace '\\', '/'
    # Out-Host keeps the script output out of the return value.
    & $bash $scriptPath @Arguments | Out-Host
    return $LASTEXITCODE
}
