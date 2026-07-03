#!/usr/bin/env bash
# =============================================================================
# File: scripts/native-libs/build-onnxruntime.sh
# Purpose: Provisions ONNX Runtime into native-libs/<platform>/. Default mode
#          downloads the official prebuilt release (GPU variant on
#          linux-x86_64: libonnxruntime.so + CUDA/TensorRT provider libs, so
#          the `ort` crate with load-dynamic can register TensorRT/CUDA EPs).
#          `--from-source` builds from the pinned tag with native SM cubins.
#
# NVIDIA B300 (Blackwell Ultra, SM_103) notes:
#   - SM_103 needs the CUDA 13 toolchain and TensorRT >= 10.13. The official
#     `gpu_cuda13` release artifact is built against CUDA 13 / cuDNN 9 and its
#     TensorRT EP dlopens the system libnvinfer.so.10 at runtime.
#   - Runtime host requirements (not bundled in the archive): NVIDIA driver
#     R580+ (CUDA 13 compatible), cuDNN 9 for the CUDA EP, TensorRT >= 10.13
#     (libnvinfer) for the TensorRT EP. A missing TensorRT install only
#     disables the TRT EP — detector code falls back to CUDA gracefully.
#   - Prebuilt CUDA 13 binaries carry PTX, so kernels JIT-compile on new SMs
#     like SM_103. That works but slows the first session load; `--from-source`
#     compiles native SM_103 cubins (CMAKE_CUDA_ARCHITECTURES=103) for fast
#     startup. Prebuilt stays the default because it needs no local CUDA/TRT
#     build toolchain.
#
# Env knobs:
#   ONNXRUNTIME_REF        git tag / release tag (default: pinned below)
#   ONNXRUNTIME_MODE       dynamic (default) | static | source
#   ONNXRUNTIME_GPU        1 (default) | 0 — CPU-only archive on linux-x86_64
#   ONNXRUNTIME_CUDA       auto (default) | 12 | 13 — prebuilt CUDA variant
#   ONNXRUNTIME_SHA256     override/provide archive checksum for custom refs
#   ONNXRUNTIME_CUDA_ARCHS CMAKE_CUDA_ARCHITECTURES for source builds
#                          (default: 103 = B300)
#   CUDA_HOME / TENSORRT_HOME  toolchain roots for source builds
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM=""
MODE="${ONNXRUNTIME_MODE:-dynamic}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --from-source)
      MODE="source"
      shift
      ;;
    *)
      if [ -z "$PLATFORM" ]; then
        PLATFORM="$1"
        shift
      else
        echo "Unknown argument: $1" >&2
        exit 1
      fi
      ;;
  esac
done
PLATFORM="${PLATFORM:-$(detect_platform)}"

# v1.26.0: 1.22.0/1.23.x hang during session creation on some graphs (MoveNet,
# large Supertone models) — fixed in 1.24-1.26. 1.26.0 has prebuilt artifacts
# for every platform we ship (1.27.0 dropped CPU win-x64) and publishes a
# dedicated `gpu_cuda13` linux/win artifact (CUDA 13 + TensorRT 10.13+ EP),
# which is what SM_103 (B300) needs. Consumers are the `ort` crate paths
# (supertonic TTS + vision detectors, api-24 needs ORT >= 1.24); sherpa
# bundles its own onnxruntime, so this pin does not affect STT.
ONNXRUNTIME_REF="${ONNXRUNTIME_REF:-v1.26.0}"
prepare_layout "$PLATFORM"
require_cmd git

# ---------------------------------------------------------------------------
# Pinned SHA-256 checksums of official release artifacts (from the GitHub
# release asset digests). Overriding ONNXRUNTIME_REF without providing
# ONNXRUNTIME_SHA256 skips verification with a loud warning.
# ---------------------------------------------------------------------------
pinned_sha256() {
  case "$1" in
    v1.26.0/onnxruntime-linux-x64-1.26.0.tgz)            echo "1254da24fb389cf39dc0ff3451ab48301740ffbfcbaf646849df92f80ee92c57" ;;
    v1.26.0/onnxruntime-linux-x64-gpu-1.26.0.tgz)        echo "cb7df7ee2ca0f962c7ce7c839aeae36223d146a91fb4646d62fb0046f297479f" ;;
    v1.26.0/onnxruntime-linux-x64-gpu_cuda13-1.26.0.tgz) echo "aa619d5701bbe58046cc998b21e692d5b2aefac1479f375c4b988526cb80befa" ;;
    v1.26.0/onnxruntime-linux-aarch64-1.26.0.tgz)        echo "34ff1c2d0f12e2cf3d33a0c5f82e39792e1d581fbd6968fd7c30d173654be01a" ;;
    v1.26.0/onnxruntime-osx-arm64-1.26.0.tgz)            echo "7a1280bbb1701ea514f71828765237e7896e0f2e1cd332f1f70dbd5c3e33aca3" ;;
    v1.26.0/onnxruntime-win-x64-1.26.0.zip)              echo "6ebe99b5564bf4d029b6e93eac9ff423682b6212eade769e9ca3f685eaf500b4" ;;
    v1.26.0/onnxruntime-win-x64-gpu-1.26.0.zip)          echo "1133b1bcb0fb6f82b1c5b470b7cc15f9080a58b27dbc7b579a1fd63125ec2a15" ;;
    v1.26.0/onnxruntime-win-x64-gpu_cuda13-1.26.0.zip)   echo "4fa096030ee766b2e590d71fb6676bbd00595c92ab87acf497fe075e98834d8b" ;;
    *) return 1 ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

verify_archive_checksum() {
  local archive_path="$1"
  local key="$ONNXRUNTIME_REF/$(basename "$archive_path")"
  local expected
  if [ -n "${ONNXRUNTIME_SHA256:-}" ]; then
    expected="$ONNXRUNTIME_SHA256"
  elif ! expected="$(pinned_sha256 "$key")"; then
    echo ">>> WARNING: no pinned SHA-256 for $key — set ONNXRUNTIME_SHA256 to verify custom refs." >&2
    return 0
  fi
  local actual
  actual="$(sha256_of "$archive_path")"
  if [ "$actual" != "$expected" ]; then
    echo "ERROR: SHA-256 mismatch for $(basename "$archive_path")" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    echo "Delete $archive_path and retry (corrupted or tampered download)." >&2
    return 1
  fi
  echo ">>> SHA-256 verified: $(basename "$archive_path")"
}

# Selects the prebuilt CUDA major for linux-x86_64 GPU archives. `auto` reads
# the driver-supported CUDA version from nvidia-smi (both the classic
# "CUDA Version:" and the newer "CUDA UMD Version:" header formats); drivers
# supporting CUDA 13 get the gpu_cuda13 artifact (required for SM_103/B300),
# older drivers keep the CUDA 12 artifact. No nvidia-smi -> CUDA 12 (safe on
# hosts where the archive is provisioned for another machine).
detect_cuda_major() {
  local requested="${ONNXRUNTIME_CUDA:-auto}"
  if [ "$requested" != "auto" ]; then
    echo "$requested"
    return 0
  fi
  local detected
  detected="$(nvidia-smi 2>/dev/null | grep -oE 'CUDA[^:|]*Version[: ]+[0-9]+' | grep -oE '[0-9]+$' | head -n1 || true)"
  if [ -n "$detected" ] && [ "$detected" -ge 13 ]; then
    echo 13
  else
    echo 12
  fi
}

# Fails loudly when the GPU archive/build did not produce the provider libs the
# detector relies on, and prints what actually landed in lib-dynamic.
sanity_check_gpu_linux() {
  local lib_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic"
  local main_so
  main_so="$(find "$lib_dir" -maxdepth 1 -name 'libonnxruntime.so.*' -type f | sort | tail -n1)"
  if [ -z "$main_so" ]; then
    echo "ERROR: libonnxruntime.so.* missing in $lib_dir" >&2
    return 1
  fi
  local missing=0
  for provider in libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so libonnxruntime_providers_tensorrt.so; do
    if [ ! -f "$lib_dir/$provider" ]; then
      echo "ERROR: expected GPU provider $provider missing in $lib_dir" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || return 1
  echo ">>> ONNX Runtime GPU sanity check:"
  echo "    runtime:   $main_so"
  echo "    providers: $(find "$lib_dir" -maxdepth 1 -name 'libonnxruntime_providers_*.so' -exec basename {} \; | sort | tr '\n' ' ')"
  if command -v ldd >/dev/null 2>&1; then
    if ldd "$main_so" | grep -q 'not found'; then
      echo ">>> WARNING: unresolved dependencies in $(basename "$main_so"):" >&2
      ldd "$main_so" | grep 'not found' >&2 || true
    else
      echo "    ldd: all DT_NEEDED dependencies of $(basename "$main_so") resolve on this host"
    fi
    # CUDA/TensorRT provider libs resolve against the CUDA runtime and
    # libnvinfer, which only exist on the GPU host — report, don't fail.
    for provider_so in "$lib_dir"/libonnxruntime_providers_{cuda,tensorrt}.so; do
      local unresolved
      # `|| true` guards pipefail: no 'not found' lines means grep exits 1.
      unresolved="$(ldd "$provider_so" 2>/dev/null | grep 'not found' | awk '{print $1}' | tr '\n' ' ' || true)"
      if [ -n "$unresolved" ]; then
        echo "    note: $(basename "$provider_so") needs on the GPU host: $unresolved"
      fi
    done
  fi
}

# Removes stale runtime versions before copying the new one — otherwise after a
# version bump lib-dynamic holds two files (e.g. libonnxruntime.so.1.22.0 +
# .1.26.0) and the ort load-dynamic probe picks non-deterministically.
clean_stale_runtime() {
  rm -f "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/libonnxruntime*.so* \
        "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/libonnxruntime*.dylib \
        "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/onnxruntime.dll 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Mode: static — CPU-only static archives (legacy consumers).
# ---------------------------------------------------------------------------
if [ "$MODE" = "static" ]; then
  require_cmd python3 cmake
  SRC="$(repo_checkout onnxruntime https://github.com/microsoft/onnxruntime.git "$ONNXRUNTIME_REF")"
  BUILD="$NATIVE_CACHE/build/onnxruntime-$PLATFORM-static"
  reset_dir "$BUILD"
  (
    cd "$SRC"
    ./build.sh \
      --config Release \
      --build_dir "$BUILD" \
      --parallel "$(platform_cpu_count)" \
      --skip_tests \
      --build_shared_lib off \
      --compile_no_warning_as_error
  )
  copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-static" -name '*.a' -o -name '*.lib'
  append_manifest_library "$PLATFORM" "onnxruntime" "static" "$ONNXRUNTIME_REF" "Built from source via build.sh."
  exit 0
fi

# ---------------------------------------------------------------------------
# Mode: source — shared runtime with CUDA + TensorRT EPs and native cubins for
# the requested SMs (default 103 = B300). Use when prebuilt PTX JIT startup
# cost matters; the prebuilt gpu_cuda13 archive is otherwise equivalent.
# ---------------------------------------------------------------------------
if [ "$MODE" = "source" ]; then
  if [ "$PLATFORM" != "linux-x86_64" ]; then
    echo "ERROR: --from-source GPU build is implemented for linux-x86_64 only (got $PLATFORM)." >&2
    exit 1
  fi
  require_cmd python3 cmake
  CUDA_HOME="${CUDA_HOME:-/usr/local/cuda}"
  if [ ! -d "$CUDA_HOME" ]; then
    echo "ERROR: CUDA_HOME=$CUDA_HOME does not exist. Install the CUDA 13.x toolkit (SM_103 needs CUDA >= 13.0)." >&2
    exit 1
  fi
  if [ -z "${TENSORRT_HOME:-}" ] || [ ! -d "$TENSORRT_HOME" ]; then
    echo "ERROR: TENSORRT_HOME must point at a TensorRT >= 10.13 install (SM_103 support starts there)." >&2
    exit 1
  fi
  CUDA_ARCHS="${ONNXRUNTIME_CUDA_ARCHS:-103}"

  SRC="$(repo_checkout onnxruntime https://github.com/microsoft/onnxruntime.git "$ONNXRUNTIME_REF")"
  BUILD="$NATIVE_CACHE/build/onnxruntime-$PLATFORM-source-gpu"
  reset_dir "$BUILD"
  echo ">>> Building ONNX Runtime $ONNXRUNTIME_REF from source (CUDA_ARCHS=$CUDA_ARCHS, CUDA_HOME=$CUDA_HOME, TENSORRT_HOME=$TENSORRT_HOME)"
  (
    cd "$SRC"
    ./build.sh \
      --config Release \
      --build_dir "$BUILD" \
      --parallel "$(platform_cpu_count)" \
      --skip_tests \
      --build_shared_lib \
      --use_cuda \
      --cuda_home "$CUDA_HOME" \
      --use_tensorrt \
      --tensorrt_home "$TENSORRT_HOME" \
      --compile_no_warning_as_error \
      --cmake_extra_defines "CMAKE_CUDA_ARCHITECTURES=$CUDA_ARCHS" onnxruntime_BUILD_UNIT_TESTS=OFF
  )

  mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
  # Source tree keeps public headers under include/onnxruntime/core/session/;
  # flatten them to match the prebuilt archive layout consumers expect.
  copy_matching "$SRC/include/onnxruntime/core/session" "$NATIVE_ROOT/$PLATFORM/include/onnxruntime" -name '*.h'
  copy_matching "$SRC/include/onnxruntime/core/providers/tensorrt" "$NATIVE_ROOT/$PLATFORM/include/onnxruntime" -name '*.h'
  copy_matching "$SRC/include/onnxruntime/core/providers/cuda" "$NATIVE_ROOT/$PLATFORM/include/onnxruntime" -name '*.h'
  clean_stale_runtime
  copy_matching "$BUILD/Release" "$NATIVE_ROOT/$PLATFORM/lib-dynamic" -name 'libonnxruntime.so*' -o -name 'libonnxruntime_providers_*.so'
  sanity_check_gpu_linux
  append_manifest_library "$PLATFORM" "onnxruntime" "dynamic" "$ONNXRUNTIME_REF" \
    "Source build: CUDA+TensorRT EPs, native cubins for SM $CUDA_ARCHS."
  exit 0
fi

# ---------------------------------------------------------------------------
# Mode: dynamic (default) — official prebuilt release archive.
# ---------------------------------------------------------------------------
require_cmd curl tar
VERSION="${ONNXRUNTIME_REF#v}"
# linux-x86_64 (NVIDIA): GPU variant by default. The archive ships — next to
# libonnxruntime.so — the libonnxruntime_providers_{shared,cuda,tensorrt}.so
# provider libs, so the `ort` crate (load-dynamic, ORT_DYLIB_PATH -> this
# libonnxruntime.so) can register the TensorRT/CUDA EPs. A system
# /usr/lib/libonnxruntime.so usually has CUDA at best (or nothing).
# CUDA variant: `gpu` = CUDA 12 / cuDNN 9, `gpu_cuda13` = CUDA 13 / cuDNN 9 /
# TensorRT >= 10.13 EP — the latter is required for SM_103 (B300); kernels ship
# as PTX so they also JIT on future SMs. When the runtime cannot load the TRT
# provider, detector code falls back to the CUDA EP gracefully. Force CPU-only
# with ONNXRUNTIME_GPU=0.
# macos-arm64: the osx-arm64 archive has the CoreML EP built in (Metal/ANE) —
# no GPU variant exists or is needed.
ONNXRUNTIME_GPU="${ONNXRUNTIME_GPU:-1}"
GPU_LINUX=0
case "$PLATFORM" in
  linux-x86_64)
    if [ "$ONNXRUNTIME_GPU" = "1" ]; then
      CUDA_MAJOR="$(detect_cuda_major)"
      case "$CUDA_MAJOR" in
        13) ARCHIVE="onnxruntime-linux-x64-gpu_cuda13-$VERSION.tgz" ;;
        12) ARCHIVE="onnxruntime-linux-x64-gpu-$VERSION.tgz" ;;
        *) echo "ERROR: unsupported ONNXRUNTIME_CUDA=$CUDA_MAJOR (expected 12 or 13)." >&2; exit 1 ;;
      esac
      GPU_LINUX=1
      echo ">>> ONNX Runtime GPU archive: $ARCHIVE (CUDA $CUDA_MAJOR; override with ONNXRUNTIME_CUDA=12|13)"
    else
      ARCHIVE="onnxruntime-linux-x64-$VERSION.tgz"
    fi
    ;;
  linux-aarch64) ARCHIVE="onnxruntime-linux-aarch64-$VERSION.tgz" ;;
  macos-arm64) ARCHIVE="onnxruntime-osx-arm64-$VERSION.tgz" ;;
  windows-x86_64) ARCHIVE="onnxruntime-win-x64-$VERSION.zip" ;;
  *) echo "No official ONNX Runtime archive for $PLATFORM" >&2; exit 1 ;;
esac

URL="https://github.com/microsoft/onnxruntime/releases/download/$ONNXRUNTIME_REF/$ARCHIVE"
DOWNLOAD_DIR="$NATIVE_CACHE/downloads"
mkdir -p "$DOWNLOAD_DIR"
ARCHIVE_PATH="$DOWNLOAD_DIR/$ARCHIVE"

if [ ! -f "$ARCHIVE_PATH" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
  echo ">>> Downloading $URL"
  curl -fL "$URL" -o "$ARCHIVE_PATH"
fi
verify_archive_checksum "$ARCHIVE_PATH"

UNPACK="$NATIVE_CACHE/build/onnxruntime-$PLATFORM-dynamic"
reset_dir "$UNPACK"
case "$ARCHIVE" in
  *.tgz) tar -xzf "$ARCHIVE_PATH" -C "$UNPACK" --strip-components=1 ;;
  *.zip)
    require_cmd unzip
    unzip -q "$ARCHIVE_PATH" -d "$UNPACK/raw"
    first_dir="$(find "$UNPACK/raw" -mindepth 1 -maxdepth 1 -type d | head -n1)"
    cp -Rf "$first_dir/"* "$UNPACK/"
    ;;
esac

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
cp -Rf "$UNPACK/include/"* "$NATIVE_ROOT/$PLATFORM/include/onnxruntime/"
clean_stale_runtime
# On linux-x86_64 GPU the archive adds the TensorRT/CUDA providers
# (libonnxruntime_providers_{shared,cuda,tensorrt}.so) — the copy pattern
# 'libonnxruntime*.so*' below catches them together with the main runtime.
copy_matching "$UNPACK" "$NATIVE_ROOT/$PLATFORM/lib-dynamic" -name 'libonnxruntime*.so*' -o -name 'libonnxruntime*.dylib' -o -name 'onnxruntime.dll'

ORT_NOTE="Official prebuilt runtime; ONNXRUNTIME_MODE=static builds from source instead."
if [ "$GPU_LINUX" = "1" ]; then
  sanity_check_gpu_linux
  ORT_NOTE="GPU variant $ARCHIVE (TensorRT+CUDA providers); ONNXRUNTIME_GPU=0 -> CPU-only, ONNXRUNTIME_CUDA=12|13 pins the CUDA line, --from-source builds native SM cubins."
fi
append_manifest_library "$PLATFORM" "onnxruntime" "dynamic" "$ONNXRUNTIME_REF" "$ORT_NOTE"
