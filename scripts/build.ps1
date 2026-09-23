# =============================================================================
# File: scripts/build.ps1
# Purpose: Windows entry point for Cargo. Loads the MSVC x64 environment, the
#          variables setup.ps1 persisted (LIBCLANG_PATH, PROTOC, VULKAN_SDK,
#          CUDA_PATH, GStreamer) and Ninja, then runs the same retention
#          wrapper as scripts/build.sh (scripts/cargo-build.py).
#
# Usage:
#   scripts\build.ps1 -Edition slim --release              # no local inference engine
#   scripts\build.ps1 -Edition full -Backend cuda --release
#   scripts\build.ps1 -Edition full -Backend vulkan --profile release-fast
#   scripts\build.ps1 -Edition full -Backend cpu --release
#   scripts\build.ps1 -Cmd test -p tentaflow-core --lib   # anything else: plain cargo arguments
#   scripts\build.ps1 -Cmd "clean -p whisper-rs-sys"
#
# -Edition/-Backend mirror the release matrix: they select the `tentaflow`
# package, its features and the native-libs variant built by
# scripts\native-libs\build-all.ps1 -Backend <same backend>.
# =============================================================================

# No param() block: PowerShell binds declared parameters by position and by
# prefix, so cargo's `-p <crate>` or a bare `--lib` would land in -Edition or
# -PipelineVariable. The three script options are taken out of $args by hand;
# everything else goes to cargo verbatim. A leading bare word is the command.
$Cmd = 'build'
$Edition = ''
$Backend = ''
$CargoArgs = @()
$rest = @($args)
for ($i = 0; $i -lt $rest.Count; $i++) {
    $a = [string]$rest[$i]
    switch -Regex ($a) {
        '^-Cmd$'     { $Cmd = [string]$rest[++$i]; continue }
        '^-Edition$' { $Edition = [string]$rest[++$i]; continue }
        '^-Backend$' { $Backend = [string]$rest[++$i]; continue }
        default {
            if ($i -eq 0 -and -not $a.StartsWith('-')) { $Cmd = $a } else { $CargoArgs += $a }
        }
    }
}
if ($Edition -notin @('', 'slim', 'full')) { throw "-Edition must be slim or full, got '$Edition'." }
if ($Backend -notin @('', 'cpu', 'cuda', 'vulkan')) { throw "-Backend must be cpu, cuda or vulkan, got '$Backend'." }

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lib\windows.ps1')

function Require-Env {
    param([string]$Name, [string]$Why)
    if (-not (Test-Path "Env:$Name")) {
        Log-Warn "$Name is not set - $Why. Run scripts\setup.ps1."
    }
}

Update-SessionEnvironment
[void](Import-TentaflowVersions)
Enter-VsDevEnvironment
Set-NinjaGenerator
Use-PinnedCuda
Require-Env 'LIBCLANG_PATH' 'bindgen (whisper/llama/sherpa/zvec -sys crates) needs libclang.dll'
Require-Env 'PROTOC' 'prost-build in tentaflow-voice needs protoc'
if (-not $env:TENTAFLOW_NATIVE_CACHE) { $env:TENTAFLOW_NATIVE_CACHE = Get-NativeCacheDir }

$editionArgs = @()
if ($Edition -eq 'slim') {
    if ($Backend) { throw '-Backend has no meaning for -Edition slim (it links no inference engine).' }
    $editionArgs = @('-p', 'tentaflow', '--no-default-features')
} elseif ($Edition -eq 'full') {
    if (-not $Backend) { throw '-Edition full needs -Backend cpu, cuda or vulkan.' }
    $editionArgs = @('-p', 'tentaflow')
    switch ($Backend) {
        'cuda'   { $editionArgs += @('--features', 'gpu-cuda') }
        'vulkan' { $editionArgs += @('--features', 'gpu-vulkan') }
    }
    # The -sys crates link native-libs\...\<library>\<variant>.
    $env:LLAMA_CPP_NATIVE_VARIANT = $Backend
    $env:WHISPER_CPP_NATIVE_VARIANT = $Backend
} elseif ($Backend) {
    throw '-Backend needs -Edition full.'
}

$cmdParts = @($Cmd -split '\s+' | Where-Object { $_ })
$allArgs = @($cmdParts) + $editionArgs + @($CargoArgs | Where-Object { $_ })

Push-Location $script:TentaflowRoot
try {
    Write-Host ''
    Log-Info "cargo $($allArgs -join ' ')"
    if ($env:LLAMA_CPP_NATIVE_VARIANT) { Log-Info "native-libs variant: $env:LLAMA_CPP_NATIVE_VARIANT" }
    Write-Host ''
    if ($cmdParts[0] -in @('build', 'test', 'check', 'prune', 'report')) {
        $python = Find-Python
        if (-not $python) { throw 'Python 3.11+ is required; run scripts\setup.ps1.' }
        & $python (Join-Path $PSScriptRoot 'cargo-build.py') @allArgs
    } else {
        & cargo @allArgs
    }
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}
if ($code -ne 0) {
    Log-Error "cargo exited with code $code"
    exit $code
}
Log-Ok 'Build OK'
