# =============================================================================
# Plik: scripts/native-libs/build-zvec.ps1
# Opis: Kopiuje zwendorowane artefakty zvec dla Windows do native-libs.
# =============================================================================

param(
    [string]$Platform = "windows-x86_64"
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Root = Resolve-Path (Join-Path $ScriptDir "..\..")
$Vendor = Join-Path $Root "tentaflow-zvec-sys\vendor\lib\$Platform"
$Output = Join-Path $Root "native-libs\$Platform"
$StaticDir = Join-Path $Output "lib-static"
$DynamicDir = Join-Path $Output "lib-dynamic"
$IncludeDir = Join-Path $Output "include\zvec"

New-Item -ItemType Directory -Force -Path $StaticDir, $DynamicDir, $IncludeDir | Out-Null

$ImportLib = Join-Path $Vendor "zvec_c_api.lib"
$Dll = Join-Path $Vendor "zvec_c_api.dll"

if (-not (Test-Path $ImportLib) -or -not (Test-Path $Dll)) {
    throw "Brak $ImportLib albo $Dll. Zbuduj zvec dla Windows przez scripts\setup.ps1 albo dedykowany build MSVC i umieść artefakty w tentaflow-zvec-sys\vendor\lib\$Platform."
}

Copy-Item $ImportLib $StaticDir -Force
Copy-Item $Dll $DynamicDir -Force
Copy-Item (Join-Path $Root "tentaflow-zvec-sys\vendor\include\zvec\c_api.h") $IncludeDir -Force

$Manifest = Join-Path $Output "manifest.toml"
Add-Content $Manifest @"
[[library]]
name = "zvec"
linkage = "dynamic-import-lib"
ref = "vendored"
note = "MSVC używa import library, a DLL musi być obok binarki."

"@
