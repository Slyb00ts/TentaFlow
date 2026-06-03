#!/bin/bash
# =============================================================================
# File: build-zvec.sh
# Purpose: Build the zvec embedded vector DB as ONE self-contained static archive
#          (libzvec_c_api.a) per platform and vendor it into tentaflow-zvec-sys.
#
# zvec depends on RocksDB + Arrow + protobuf + ANTLR etc. We compile it once with
# a pinned, known-good toolchain (Linux: Ubuntu 22.04 / gcc-11 in Docker, because
# RocksDB 8.1 does not build with very new GCC), then merge every component +
# third-party .a into a single archive (the "model B" layout). The archive is
# gitignored; this script regenerates it.
#
# Usage:
#   ./scripts/build-zvec.sh linux-x86_64       # Docker build (default)
#   ./scripts/build-zvec.sh macos-arm64        # native, must run on macOS
#   ./scripts/build-zvec.sh ios-arm64          # native, must run on macOS (Xcode)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SYS_CRATE="$ROOT/tentaflow-zvec-sys"
VENDOR_INCLUDE="$SYS_CRATE/vendor/include/zvec"

ZVEC_REPO="https://github.com/alibaba/zvec"
# zvec FTS/hybrid-search API (zvec_fts_*, reranker, multi_query) wszedl po tagu
# v0.4.0 (commit 02bfb31 #408) i nie ma go w zadnym tagu. Wrapper tentaflow-zvec
# go uzywa, wiec pinujemy konkretny commit main, ktorego c_api.h zgadza sie z
# zwendorowanym naglowkiem.
ZVEC_REF="${ZVEC_REF:-f562bdd636d454f18128cb18b41578128d1415a4}"
PLATFORM="${1:-linux-x86_64}"

SRC_DIR="${ZVEC_SRC_DIR:-/tmp/zvec-build}"

echo "=========================================="
echo "  Build zvec static archive (model B)"
echo "  Platform: $PLATFORM | ref: $ZVEC_REF"
echo "  Source:   $SRC_DIR"
echo "=========================================="

# 1. Source + submodules (RocksDB/Arrow/protobuf/...) at the pinned tag.
if [ ! -d "$SRC_DIR/.git" ]; then
    git clone "$ZVEC_REPO" "$SRC_DIR"
fi
( cd "$SRC_DIR" && git fetch origin -q && git checkout "$ZVEC_REF" \
  && git submodule update --init --recursive --depth 1 )

OUT_LIB_DIR="$SYS_CRATE/vendor/lib/$PLATFORM"
mkdir -p "$OUT_LIB_DIR" "$VENDOR_INCLUDE"

# Desktop builds link zvec's self-contained shared library (it bundles RocksDB/
# Arrow/protobuf + a static libstdc++ and exports only the C API). Mobile builds
# need a static archive instead — see the ios/android note below.

case "$PLATFORM" in
  linux-x86_64|linux-aarch64)
    # RocksDB 8.1 does not build with gcc >= 13; build in a pinned container.
    docker run --rm -v "$SRC_DIR:/src" -w /src ubuntu:22.04 bash -c '
      set -e
      export DEBIAN_FRONTEND=noninteractive CC=gcc-11 CXX=g++-11
      apt-get update -qq && apt-get install -y -qq build-essential gcc-11 g++-11 git python3 python3-pip libssl-dev pkg-config curl ca-certificates >/dev/null 2>&1
      pip3 install -q "cmake<4" ninja >/dev/null 2>&1
      rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
      cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=gcc-11 -DCMAKE_CXX_COMPILER=g++-11 \
        -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_C_BINDINGS=ON ..
      ninja zvec_c_api -j"$(nproc)"
      chown -R '"$(id -u):$(id -g)"' /src/build_zvec
    '
    cp "$SRC_DIR/build_zvec/lib/libzvec_c_api.so" "$OUT_LIB_DIR/libzvec_c_api.so"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.so"
    ;;
  macos-arm64)
    # zvec submoduly (googletest/RocksDB/Arrow) wymagaja CMake<4: CMake 4 usunal
    # kompatybilnosc z cmake_minimum_required<3.5, a wymuszenie starej polityki psuje
    # eksport include-dirs Arrow. Pinujemy cmake 3.x w lokalnym venv (jak Linux w Dockerze).
    if ! command -v python3 >/dev/null 2>&1; then
        echo "python3 nie znaleziony — wymagany do przypiecia cmake<4 (zainstaluj Xcode Command Line Tools)."
        exit 1
    fi
    CMAKE_PIN="$SRC_DIR/.cmake-pin"
    if [ ! -x "$CMAKE_PIN/bin/cmake" ]; then
        python3 -m venv "$CMAKE_PIN"
        "$CMAKE_PIN/bin/pip" install -q "cmake<4"
    fi
    PATH="$CMAKE_PIN/bin:$PATH"
    ( cd "$SRC_DIR" && rm -rf build_zvec && mkdir -p build_zvec && cd build_zvec
      cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_PYTHON_BINDINGS=OFF -DBUILD_TOOLS=OFF -DBUILD_C_BINDINGS=ON ..
      ninja zvec_c_api -j"$(sysctl -n hw.ncpu)" )
    cp "$SRC_DIR/build_zvec/lib/libzvec_c_api.dylib" "$OUT_LIB_DIR/libzvec_c_api.dylib"
    ARTIFACT="$OUT_LIB_DIR/libzvec_c_api.dylib"
    ;;
  ios-arm64|ios-sim-arm64|android-arm64)
    echo "Mobile ($PLATFORM) needs a STATIC build and must run on the platform SDK:"
    echo "  iOS:     zvec/scripts/build_ios.sh   (macOS + Xcode)"
    echo "  Android: zvec/scripts/build_android.sh (NDK)"
    echo "Then vendor libzvec_c_api.a (+ libzvec_deps.a) into $OUT_LIB_DIR."
    exit 1
    ;;
  *)
    echo "Unknown platform: $PLATFORM"
    exit 1
    ;;
esac

# Vendor the header (committed) — keep it in sync with the built lib.
cp "$SRC_DIR/src/include/zvec/c_api.h" "$VENDOR_INCLUDE/c_api.h"

echo ""
echo "=========================================="
echo "  Done: $ARTIFACT ($(du -h "$ARTIFACT" | cut -f1))"
echo "  Header: $VENDOR_INCLUDE/c_api.h"
echo "=========================================="
