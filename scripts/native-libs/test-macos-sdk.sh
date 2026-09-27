#!/usr/bin/env bash
# ============ File: test-macos-sdk.sh — Verify SDK selection with real C/C++ compile and link steps. ============
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

if [ "$(uname -s)" != Darwin ]; then
  echo "SKIP: macOS SDK integration test requires macOS"
  exit 0
fi
require_cmd cmake xcrun

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Non-macOS targets must retain their own SDK selection.
export SDKROOT="$WORK/other-sdk"
for platform in ios-arm64 ios-sim-arm64 android-arm64 linux-aarch64 windows-x86_64; do
  resolve_macos_sdk "$platform"
  [ "$SDKROOT" = "$WORK/other-sdk" ]
done
unset SDKROOT

resolve_macos_sdk "$(detect_platform)"
[ "$SDKROOT" = "$(xcrun --sdk macosx --show-sdk-path)" ]

cat > "$WORK/CMakeLists.txt" <<'CMAKE'
cmake_minimum_required(VERSION 3.20)
project(sdk_probe LANGUAGES C CXX)
if(NOT CMAKE_OSX_SYSROOT STREQUAL "$ENV{SDKROOT}")
  message(FATAL_ERROR "CMake did not select the active macOS SDK")
endif()
add_executable(probe_c probe.c)
add_executable(probe_cxx probe.cpp)
CMAKE
cat > "$WORK/probe.c" <<'C'
#include <stdio.h>
int main(void) { return puts("C SDK link OK") < 0; }
C
cat > "$WORK/probe.cpp" <<'CXX'
#include <iostream>
int main() { std::cout << "C++ SDK link OK\n"; }
CXX

cmake -S "$WORK" -B "$WORK/build"
cmake --build "$WORK/build"
"$WORK/build/probe_c"
"$WORK/build/probe_cxx"
# whisper also links its shared library outside CMake.
/usr/bin/c++ -dynamiclib "$WORK/probe.cpp" -o "$WORK/libprobe.dylib"
echo "PASS: CMake C/C++ compilation, direct dylib link, and non-macOS SDK isolation"
