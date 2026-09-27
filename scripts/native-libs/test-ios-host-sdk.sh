#!/usr/bin/env bash
# ============ File: test-ios-host-sdk.sh — Verify macOS code generation during iOS cross-compilation. ============
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

if [ "$(uname -s)" != Darwin ]; then
  echo "SKIP: iOS host SDK integration test requires macOS and Xcode"
  exit 0
fi
require_cmd cmake xcrun
resolve_macos_sdk "$(detect_platform)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/CMakeLists.txt" <<'CMAKE'
cmake_minimum_required(VERSION 3.20)
project(ios_host_sdk_probe LANGUAGES C)
if(CMAKE_OSX_SYSROOT STREQUAL "$ENV{SDKROOT}")
  message(FATAL_ERROR "The iOS target must not use the host SDK")
endif()
add_custom_command(
  OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/generated.h"
  COMMAND /usr/bin/cc "${CMAKE_CURRENT_SOURCE_DIR}/host.c"
          -o "${CMAKE_CURRENT_BINARY_DIR}/host_generator"
  COMMAND "${CMAKE_CURRENT_BINARY_DIR}/host_generator"
          "${CMAKE_CURRENT_BINARY_DIR}/generated.h"
  DEPENDS host.c
  VERBATIM
)
add_library(ios_probe STATIC target.c "${CMAKE_CURRENT_BINARY_DIR}/generated.h")
target_include_directories(ios_probe PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
CMAKE
cat > "$WORK/host.c" <<'C'
#include <TargetConditionals.h>
#include <stdio.h>
#if !TARGET_OS_OSX
#error The code generator must run on macOS
#endif
int main(int argc, char **argv) {
  if (argc != 2) return 1;
  FILE *out = fopen(argv[1], "w");
  if (!out) return 1;
  int result = fputs("#define GENERATED_VALUE 42\n", out);
  int closed = fclose(out);
  return result < 0 || closed != 0;
}
C
cat > "$WORK/target.c" <<'C'
#include <TargetConditionals.h>
#include "generated.h"
#if !TARGET_OS_IPHONE
#error The target library must use the iOS SDK
#endif
int generated_value(void) { return GENERATED_VALUE; }
C

for sdk in iphoneos iphonesimulator; do
  sdk_path="$(xcrun --sdk "$sdk" --show-sdk-path)"
  cmake -S "$WORK" -B "$WORK/$sdk" \
    -DCMAKE_SYSTEM_NAME=iOS \
    -DCMAKE_OSX_ARCHITECTURES=arm64 \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=13.0 \
    -DCMAKE_OSX_SYSROOT="$sdk_path" \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY
  cmake --build "$WORK/$sdk"
done
echo "PASS: macOS code generation with separate iOS device and simulator SDKs"
