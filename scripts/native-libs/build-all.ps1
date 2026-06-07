# =============================================================================
# Plik: scripts/native-libs/build-all.ps1
# Opis: Buduje natywne biblioteki na Windows do katalogu native-libs.
# =============================================================================

param(
    [string]$Platform = "",
    [string]$Only = "",
    [switch]$Update,
    [string]$Cache = ""
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Resolve-Path (Join-Path $ScriptDir "..\..")

if ($Platform -eq "") {
    $arch = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { throw "Obsługiwany jest tylko Windows x86_64" }
    $Platform = "windows-$arch"
}

if ($Cache -eq "") {
    $Cache = if ($env:TENTAFLOW_NATIVE_CACHE) { $env:TENTAFLOW_NATIVE_CACHE } else { Join-Path $env:TEMP "tentaflow-native-libs" }
}

$env:TENTAFLOW_NATIVE_CACHE = $Cache
if ($Update) {
    $env:TENTAFLOW_NATIVE_UPDATE = "1"
}

Write-Host "Platforma: $Platform"
Write-Host "Cache:     $Cache"
Write-Host "Output:    $(Join-Path $Root "native-libs\$Platform")"

function Run-Step($Name, $Command) {
    if ($Only -ne "" -and $Only -ne $Name) {
        return
    }
    & $Command $Platform
    if ($LASTEXITCODE -ne 0) {
        throw "Krok $Name nie powiódł się"
    }
}

$bash = Get-Command bash -ErrorAction SilentlyContinue
if (-not $bash) {
    throw "build-all.ps1 wymaga bash z Git for Windows albo MSYS2 dla skryptów CMake."
}

New-Item -ItemType Directory -Force -Path (Join-Path $Root "native-libs\$Platform") | Out-Null
$Manifest = Join-Path $Root "native-libs\$Platform\manifest.toml"
@"
# Wygenerowane przez scripts/native-libs/build-all.ps1
platform = "$Platform"
cache_dir = "$Cache"
generated_at_unix = $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())

"@ | Set-Content $Manifest

Run-Step "zvec" { param($p) & (Join-Path $ScriptDir "build-zvec.ps1") -Platform $p }
Run-Step "llama-cpp" { param($p) bash (Join-Path $ScriptDir "build-llama-cpp.sh") $p }
Run-Step "whisper-cpp" { param($p) bash (Join-Path $ScriptDir "build-whisper-cpp.sh") $p }
Run-Step "sherpa-onnx" { param($p) bash (Join-Path $ScriptDir "build-sherpa-onnx.sh") $p }
Run-Step "onnxruntime" { param($p) bash (Join-Path $ScriptDir "build-onnxruntime.sh") $p }

Write-Host "Gotowe."
