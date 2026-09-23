# =============================================================================
# File: scripts/native-libs/build-all.ps1
# Purpose: Builds native-libs\windows-x86_64 on Windows. It prepares what the
#          shared bash scripts need - MSVC x64 environment, Ninja, CUDA/Vulkan
#          SDK variables, Python and the native cache - and runs
#          scripts/native-libs/build-all.sh through Git Bash, so the list of
#          steps, the versions (scripts/versions.env) and the layout are the
#          same as on Linux and macOS.
#
# Usage:
#   scripts\native-libs\build-all.ps1                     # full, backends auto-detected ("multi")
#   scripts\native-libs\build-all.ps1 -Backend cuda       # full, CUDA variant
#   scripts\native-libs\build-all.ps1 -Backend vulkan     # full, Vulkan variant
#   scripts\native-libs\build-all.ps1 -Backend cuda,vulkan
#   scripts\native-libs\build-all.ps1 -Edition slim       # zvec + pdfium only
#   scripts\native-libs\build-all.ps1 -Only whisper-cpp -Backend cuda
#   scripts\native-libs\build-all.ps1 -Update             # re-fetch sources/archives
#
# -Backend picks the llama.cpp / whisper.cpp variants: `auto` builds one
# "multi" variant with every backend whose toolkit is installed; explicit
# backends build one variant each (native-libs\...\llama-cpp\<backend>), which
# scripts\build.ps1 -Backend <backend> links. The GPU ONNX Runtime (with the
# vendored TensorRT/cuDNN/CUDA runtimes) is provisioned when CUDA is requested,
# or with `auto` on a machine with an NVIDIA GPU.
# =============================================================================

[CmdletBinding()]
param(
    # auto, cpu, cuda, vulkan - a PowerShell array or the single comma-joined
    # string that `powershell -File` (and build.bat) passes.
    [string[]]$Backend = @('auto'),
    [ValidateSet('full', 'slim')]
    [string]$Edition = 'full',
    [ValidateSet('', 'zvec', 'pdfium', 'llama-cpp', 'whisper-cpp', 'sherpa-onnx', 'onnxruntime')]
    [string]$Only = '',
    [switch]$Update
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\lib\windows.ps1')

if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'Only Windows x86_64 is supported.'
}
$Backend = @($Backend | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
foreach ($b in $Backend) {
    if ($b -notin @('auto', 'cpu', 'cuda', 'vulkan')) {
        throw "-Backend: unknown backend '$b' (auto, cpu, cuda, vulkan)."
    }
}
if ($Backend.Count -gt 1 -and $Backend -contains 'auto') {
    throw "-Backend auto cannot be combined with explicit backends."
}

Update-SessionEnvironment
[void](Import-TentaflowVersions)
Enter-VsDevEnvironment
Set-NinjaGenerator
Use-PinnedCuda

$python = Find-Python
if (-not $python) { throw 'Python 3.11+ not found. Run scripts\setup.ps1.' }
$env:TENTAFLOW_PYTHON = $python
$env:TENTAFLOW_NATIVE_CACHE = Get-NativeCacheDir
New-Item -ItemType Directory -Force -Path $env:TENTAFLOW_NATIVE_CACHE | Out-Null
if ($Update) { $env:TENTAFLOW_NATIVE_UPDATE = '1' }

$wantsCuda = $Backend -contains 'cuda'
$wantsVulkan = $Backend -contains 'vulkan'
if ($Backend -contains 'auto') {
    $env:LLAMA_CPP_BACKENDS = 'auto'
    $env:WHISPER_CPP_BACKENDS = 'auto'
    $wantsCuda = (Test-NvidiaGpu) -and (Test-Command 'nvcc.exe')
} else {
    $list = ($Backend | Select-Object -Unique) -join ','
    $env:LLAMA_CPP_BACKENDS = $list
    $env:WHISPER_CPP_BACKENDS = $list
}
if ($wantsCuda) {
    if (-not $env:CUDA_PATH -or -not (Test-Command 'nvcc.exe')) {
        throw 'CUDA backend requested but the CUDA toolkit (nvcc, CUDA_PATH) is missing. Run scripts\setup.ps1 -Cuda.'
    }
}
if ($wantsVulkan) {
    if (-not $env:VULKAN_SDK -or -not (Test-Command 'glslc.exe')) {
        throw 'Vulkan backend requested but the Vulkan SDK (VULKAN_SDK, glslc) is missing. Run scripts\setup.ps1.'
    }
}
if ($wantsCuda) { $env:ONNXRUNTIME_GPU = '1' } else { $env:ONNXRUNTIME_GPU = '0' }

$platform = 'windows-x86_64'
$bashArgs = @('--platform', $platform, '--edition', $Edition)
if ($Only) { $bashArgs += @('--only', $Only) }

Log-Section "native-libs $platform ($Edition)"
if ($Edition -eq 'full') {
    Log-Info "Backends:     $env:LLAMA_CPP_BACKENDS"
    Log-Info "ONNX Runtime: $(if ($wantsCuda) { 'GPU (CUDA/TensorRT)' } else { 'CPU' })"
}
Log-Info "Cache:        $env:TENTAFLOW_NATIVE_CACHE"
Log-Info "Git Bash:     $(Find-GitBash)"

$code = Invoke-GitBash -Script (Join-Path $PSScriptRoot 'build-all.sh') -Arguments $bashArgs
if ($code -ne 0) {
    Log-Error "build-all.sh failed (exit $code)"
    exit $code
}
Log-Ok "native-libs\$platform ready"
