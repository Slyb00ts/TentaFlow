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
#   - The TensorRT + cuDNN runtimes are vendored below, so the only host
#     requirement left is an NVIDIA R580+ driver (CUDA 13 compatible). If the
#     TRT EP still cannot load at runtime, detector code falls back to the
#     CUDA EP gracefully.
#   - Prebuilt CUDA 13 binaries carry PTX, so kernels JIT-compile on new SMs
#     like SM_103. That works but slows the first session load; `--from-source`
#     compiles native SM_103 cubins (CMAKE_CUDA_ARCHITECTURES=103) for fast
#     startup. Prebuilt stays the default because it needs no local CUDA/TRT
#     build toolchain.
#
# NVIDIA GB10 (Grace-Blackwell, DGX Spark, aarch64/sbsa, SM_121) notes:
#   - Microsoft ships NO aarch64 ONNX Runtime GPU tarball — only linux-x64 GPU
#     archives. The aarch64 GPU providers come from the community
#     `onnxruntime-gpu` aarch64 wheel on the Jetson AI Lab devpi index
#     (pypi.jetson-ai-lab.io/sbsa/cu130), built against CUDA 13 for sbsa. Its
#     `libonnxruntime_providers_cuda.so` carries NATIVE sm_121 cubins (verified
#     with cuobjdump: sm_87/sm_110/sm_120a/sm_121), so GB10 runs on real CUDA
#     kernels with no PTX-JIT and no source build.
#   - That wheel is ORT 1.24.0 (newest aarch64 GPU build published); the base
#     libonnxruntime.so.1.24.0 comes from the SAME wheel so the provider bridge
#     ABI matches (provider libs are NOT ABI-stable across ORT minors — mixing a
#     1.24 provider with the 1.26 CPU base would fail to register). The `ort`
#     crate uses feature `api-24` (C API v24, ORT >= 1.24), so 1.24.0 satisfies
#     it exactly. This replaces the aarch64 CPU-only base with the GPU base.
#   - GB10 hosts have the CUDA 13 sbsa toolkit at /usr/local/cuda (in ldconfig),
#     so only cuDNN 9 (aarch64) has to be vendored for the CUDA EP; the toolkit
#     runtime libs (cudart/cublas/cublasLt/cufft) resolve from the host. The
#     TensorRT EP is skipped by default on aarch64 (sm_121 TRT engine building is
#     unproven on this brand-new SM); the CUDA EP is the reliable GPU target and
#     ort_common falls TensorRT->CUDA->CPU softly. Opt into TRT vendoring with
#     TENTAFLOW_SKIP_TENSORRT_VENDOR=0.
#
# Self-contained GPU runtime (no system TensorRT/cuDNN on the target host):
#   When the CUDA-13 GPU variant is selected, the script additionally vendors
#   the TensorRT and cuDNN runtime libs from the official NVIDIA wheels
#   (pypi.nvidia.com / pypi.org, no login) into lib-dynamic/, flat, so
#   tentaflow/build.rs copies them next to the binary and libonnxruntime
#   resolves them via rpath/$ORIGIN. Vendored set:
#     TensorRT: libnvinfer.so.10, libnvinfer_plugin.so.10,
#               libnvonnxparser.so.10, libnvinfer_builder_resource_<sm>/_ptx
#               (per-SM builder resources since TRT 10.15; older 10.13/10.14
#               wheels ship one monolithic ~1.3 GB resource)
#     cuDNN 9:  libcudnn.so.9 + all split libs it dlopens (ops/cnn/adv/graph/
#               heuristic/engines_*)
#     CUDA toolkit runtime (the EPs DT_NEED these and a driver-only host has
#     none of them): libcudart.so.13, libcublas.so.13 + libcublasLt.so.13,
#     libcufft.so.12, libcurand.so.10 — from the nvidia-cuda-runtime /
#     nvidia-cublas / nvidia-cufft / nvidia-curand wheels (CUDA 13 line
#     dropped the -cuXX package suffix; the CUDA major is in the version).
#   This is ~3.2 GB on disk (and again next to the binary). Opt-outs:
#   TENTAFLOW_SKIP_TRT_VENDOR=1 skips ALL vendoring (system TRT+cuDNN+CUDA
#   toolkit expected); TENTAFLOW_SKIP_CUDA_VENDOR=1 skips only the CUDA
#   toolkit libs (host has the toolkit but no TensorRT/cuDNN).
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
#   TENTAFLOW_SKIP_TRT_VENDOR  1 = do not vendor TensorRT/cuDNN/CUDA wheels
#   TENTAFLOW_SKIP_TENSORRT_VENDOR 1 = vendor cuDNN (+CUDA) but NOT TensorRT
#                          (CUDA EP only). Default: aarch64 = 1, x86_64 = 0.
#   TENSORRT_VENDOR_REF    TensorRT wheel version (default: pinned below)
#   TENSORRT_VENDOR_SHA256 wheel checksum override for custom versions
#   TENSORRT_SMS           builder-resource buckets: auto (default) | all |
#                          comma list (sm75,sm80,sm86,sm89,sm90,sm100,sm120);
#                          the ptx resource is always included. Cross-
#                          provisioning for B300: TENSORRT_SMS=sm100.
#   CUDNN_VENDOR_REF       cuDNN wheel version (default: pinned below)
#   CUDNN_VENDOR_SHA256    wheel checksum override for custom versions
#   TENTAFLOW_SKIP_CUDA_VENDOR 1 = do not vendor CUDA toolkit runtime libs
#   CUDART_VENDOR_REF / CUBLAS_VENDOR_REF / CUFFT_VENDOR_REF /
#   CURAND_VENDOR_REF      CUDA toolkit wheel versions (defaults pinned
#                          below; overriding without a pinned checksum
#                          downgrades to a loud unverified-download warning)
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

# NVIDIA PyPI wheels use the platform's manylinux arch tag; the vendoring paths
# below are shared between x86_64 and aarch64 and key off this.
case "$PLATFORM" in
  linux-x86_64)  WHEEL_ARCH="x86_64" ;;
  linux-aarch64) WHEEL_ARCH="aarch64" ;;
  *)             WHEEL_ARCH="" ;;
esac

# aarch64 GPU: newest published `onnxruntime-gpu` aarch64 (sbsa/cu130) wheel on
# the Jetson AI Lab devpi index. Ships libonnxruntime.so.1.24.0 + the
# shared/cuda/tensorrt providers with native sm_121 (GB10) cubins.
ONNXRUNTIME_AARCH64_GPU_REF="${ONNXRUNTIME_AARCH64_GPU_REF:-1.24.0}"
ONNXRUNTIME_AARCH64_GPU_WHEEL="onnxruntime_gpu-${ONNXRUNTIME_AARCH64_GPU_REF}-cp312-cp312-linux_aarch64.whl"
ONNXRUNTIME_AARCH64_GPU_URL="${ONNXRUNTIME_AARCH64_GPU_URL:-https://pypi.jetson-ai-lab.io/sbsa/cu130/+f/012/c10bef23a39f0/${ONNXRUNTIME_AARCH64_GPU_WHEEL}}"

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

# ---------------------------------------------------------------------------
# TensorRT + cuDNN runtime vendoring (CUDA-13 GPU variant only).
# TRT 10.16.1.11: newest 10.x CUDA-13 line (soname .10 matches what the ORT
# TRT EP dlopens; SM_103 support landed in 10.13). Since 10.15 the wheel
# splits builder resources per SM, so we vendor only the needed bucket + ptx
# (~1.24 GB) instead of the monolithic 10.13 resource (~1.34 GB alone).
# cuDNN 9.24.0.43: newest cuDNN 9 CUDA-13 wheel; libcudnn.so.9 is a shim that
# dlopens the split libs, so the whole lib/ set is required (~0.97 GB).
# ---------------------------------------------------------------------------
TENSORRT_VENDOR_REF="${TENSORRT_VENDOR_REF:-10.16.1.11}"
CUDNN_VENDOR_REF="${CUDNN_VENDOR_REF:-9.24.0.43}"
# CUDA 13 toolkit runtime lines (versions differ per component; the CUDA
# major moved into the wheel version when NVIDIA dropped the -cuXX suffix).
CUDART_VENDOR_REF="${CUDART_VENDOR_REF:-13.3.29}"
CUBLAS_VENDOR_REF="${CUBLAS_VENDOR_REF:-13.6.0.2}"
CUFFT_VENDOR_REF="${CUFFT_VENDOR_REF:-12.3.0.29}"
CURAND_VENDOR_REF="${CURAND_VENDOR_REF:-10.4.3.29}"

pinned_wheel_sha256() {
  case "$1" in
    tensorrt_cu13_libs-10.16.1.11-py3-none-manylinux_2_28_x86_64.whl) echo "91142c8ab3c58bed213cf1a563a6eb4e4f0ac529d05b5f909073acece0e3b712" ;;
    nvidia_cudnn_cu13-9.24.0.43-py3-none-manylinux_2_27_x86_64.whl)   echo "71f181cd810e90f9b6023b01186fe82d13d65f0ec098581ee201d39fad769e4b" ;;
    nvidia_cuda_runtime-13.3.29-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl) echo "e04420616e72f563167a7733272992d7e6df6dc5cb54b2f94f9f1520ea9e30c1" ;;
    nvidia_cublas-13.6.0.2-py3-none-manylinux_2_27_x86_64.whl)        echo "b82c80c886cea6da6e149a5c3bdba274f12b7e4ec4b00a050b916b0446fb4153" ;;
    nvidia_cufft-12.3.0.29-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl)      echo "edb25c0626bd202ee5acc035b5dd361a3b89ed3b75a81a52df72c89150cb57c2" ;;
    nvidia_curand-10.4.3.29-py3-none-manylinux_2_27_x86_64.whl)       echo "1859bf37a62754d2c65001393096ca79de399f995971fa7826d0adfd88c3cf7b" ;;
    # aarch64 (sbsa / GB10) GPU artifacts.
    onnxruntime_gpu-1.24.0-cp312-cp312-linux_aarch64.whl)             echo "012c10bef23a39f074730d158b72f797a7314f2695c65835a0669b57282422f6" ;;
    nvidia_cudnn_cu13-9.24.0.43-py3-none-manylinux_2_27_aarch64.whl)  echo "a6812a554a1ff0413e9c52b84c26c050380649ab9615f9c16bded368ce9f421f" ;;
    tensorrt_cu13_libs-10.16.1.11-py3-none-manylinux_2_35_aarch64.whl) echo "26f58281c79591b68c3ba1cf061ea11fac747feba0622954dbc892c2998d0153" ;;
    *) return 1 ;;
  esac
}

# Resolves the wheel filename for a package/version from the NVIDIA simple
# index for the current $WHEEL_ARCH — the manylinux platform tag varies per
# package/version (e.g. TRT is manylinux_2_28_x86_64 vs manylinux_2_35_aarch64),
# so it cannot be reconstructed from the version alone.
resolve_nvidia_wheel() {
  local package="$1"
  local version="$2"
  local underscored="${package//-/_}"
  local filename
  filename="$(curl -fsSL --max-time 30 "https://pypi.nvidia.com/$package/" \
    | grep -oE "href=\"${underscored}-${version}-[^\"#]*${WHEEL_ARCH}[^\"#]*\\.whl" \
    | head -n1 | sed 's/^href="//')"
  if [ -z "$filename" ]; then
    echo "ERROR: no $WHEEL_ARCH wheel for $package==$version on pypi.nvidia.com" >&2
    return 1
  fi
  printf '%s\n' "$filename"
}

# Downloads + verifies + extracts one CUDA toolkit runtime wheel from the
# NVIDIA index. Members live under nvidia/cu13/lib/ in every cu13-era wheel.
vendor_cuda_wheel() {
  local package="$1"
  local version="$2"
  local member_regex="$3"
  local lib_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic"
  local wheel
  wheel="$(resolve_nvidia_wheel "$package" "$version")" || return 1
  local wheel_path="$NATIVE_CACHE/downloads/$wheel"
  download_cached "https://pypi.nvidia.com/$package/$wheel" "$wheel_path" || return 1
  verify_wheel_checksum "$wheel_path" "" || return 1
  echo ">>> Vendoring $package $version:"
  extract_wheel_members "$wheel_path" "^nvidia/cu13/lib/($member_regex)$" "$lib_dir" || return 1
}

verify_wheel_checksum() {
  local wheel_path="$1"
  local override="$2"
  local expected
  if [ -n "$override" ]; then
    expected="$override"
  elif ! expected="$(pinned_wheel_sha256 "$(basename "$wheel_path")")"; then
    echo ">>> WARNING: no pinned SHA-256 for $(basename "$wheel_path") — set the *_VENDOR_SHA256 env to verify custom versions." >&2
    return 0
  fi
  local actual
  actual="$(sha256_of "$wheel_path")"
  if [ "$actual" != "$expected" ]; then
    echo "ERROR: SHA-256 mismatch for $(basename "$wheel_path")" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    echo "Delete $wheel_path and retry (corrupted or tampered download)." >&2
    return 1
  fi
  echo ">>> SHA-256 verified: $(basename "$wheel_path")"
}

download_cached() {
  local url="$1"
  local dest="$2"
  if [ ! -f "$dest" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
    echo ">>> Downloading $url"
    curl -fL "$url" -o "$dest"
  fi
}

# Extracts wheel members matching a python regex flat (basename only) into a
# directory. Wheels are plain zips; python3 is already a script prerequisite.
extract_wheel_members() {
  local wheel_path="$1"
  local pattern="$2"
  local dest="$3"
  WHEEL="$wheel_path" PATTERN="$pattern" DEST="$dest" python3 - <<'PYEOF'
import os, re, sys, zipfile
pattern = re.compile(os.environ["PATTERN"])
dest = os.environ["DEST"]
extracted = []
with zipfile.ZipFile(os.environ["WHEEL"]) as zf:
    for info in zf.infolist():
        if info.is_dir() or not pattern.search(info.filename):
            continue
        target = os.path.join(dest, os.path.basename(info.filename))
        with zf.open(info) as src, open(target, "wb") as out:
            while chunk := src.read(1 << 20):
                out.write(chunk)
        os.chmod(target, 0o755)
        extracted.append((os.path.basename(info.filename), info.file_size))
if not extracted:
    print(f"ERROR: no members matching {pattern.pattern} in {os.environ['WHEEL']}", file=sys.stderr)
    sys.exit(1)
for name, size in sorted(extracted):
    print(f"    {size / 1e6:9.1f} MB  {name}")
PYEOF
}

# Maps the host GPU compute capability to a TRT builder-resource bucket.
# B300 (Blackwell Ultra) reports cap 10.3 -> the sm100 datacenter-Blackwell
# resource. No GPU on the provisioning host defaults to sm100 because B300 is
# the primary cross-provisioning target (override with TENSORRT_SMS).
detect_trt_sm_bucket() {
  local cap
  cap="$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -n1 | tr -d ' ' || true)"
  case "$cap" in
    7.5) echo "sm75" ;;
    8.0|8.7) echo "sm80" ;;
    8.6) echo "sm86" ;;
    8.9) echo "sm89" ;;
    9.*) echo "sm90" ;;
    10.*) echo "sm100" ;;
    12.*) echo "sm120" ;;
    "") echo "sm100" ;;
    *) echo "" ;;
  esac
}

# Vendors the TensorRT + cuDNN + CUDA toolkit runtimes into lib-dynamic/ so
# the shipped binary needs nothing but the NVIDIA driver on the target host —
# the EP libs DT_NEED/dlopen libnvinfer.so.10, libnvonnxparser.so.10,
# libcudnn.so.9, libcudart.so.13, libcublas(Lt).so.13, libcufft.so.12 and
# libcurand.so.10 from the binary directory ($ORIGIN) after
# tentaflow/build.rs copies them.
vendor_nvidia_runtimes() {
  if [ "${TENTAFLOW_SKIP_TRT_VENDOR:-0}" = "1" ]; then
    echo ">>> Skipping TensorRT/cuDNN/CUDA vendoring (TENTAFLOW_SKIP_TRT_VENDOR=1 — system TRT/cuDNN/CUDA expected on the target host)."
    return 0
  fi
  require_cmd python3
  local lib_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic"
  local download_dir="$NATIVE_CACHE/downloads"
  mkdir -p "$download_dir"
  local required_libs=()

  # --- TensorRT runtime from the NVIDIA PyPI index (pypi.nvidia.com serves the
  # wheel files directly next to the simple-index page). Skipped (CUDA EP only)
  # when TENTAFLOW_SKIP_TENSORRT_VENDOR=1 — default on aarch64/GB10 where sm_121
  # TRT is unproven; ort_common falls TensorRT->CUDA->CPU softly. ---
  if [ "${TENTAFLOW_SKIP_TENSORRT_VENDOR:-0}" = "1" ]; then
    echo ">>> Skipping TensorRT vendoring (TENTAFLOW_SKIP_TENSORRT_VENDOR=1 — CUDA EP only)."
    rm -f "$lib_dir"/libnvinfer*.so* "$lib_dir"/libnvonnxparser*.so* 2>/dev/null || true
  else
    local trt_wheel
    trt_wheel="$(resolve_nvidia_wheel tensorrt-cu13-libs "$TENSORRT_VENDOR_REF")" || return 1
    local trt_path="$download_dir/$trt_wheel"
    download_cached "https://pypi.nvidia.com/tensorrt-cu13-libs/$trt_wheel" "$trt_path" || return 1
    verify_wheel_checksum "$trt_path" "${TENSORRT_VENDOR_SHA256:-}" || return 1

    local sms="${TENSORRT_SMS:-auto}"
    local bucket_re
    case "$sms" in
      auto)
        local bucket
        bucket="$(detect_trt_sm_bucket)"
        if [ -z "$bucket" ]; then
          echo ">>> WARNING: unrecognized GPU compute capability — vendoring only the ptx builder resource (JIT on first engine build)." >&2
          bucket_re=""
        else
          bucket_re="|$bucket"
        fi
        ;;
      all) bucket_re="|sm[0-9]+" ;;
      *) bucket_re="|$(printf '%s' "$sms" | tr ',' '|')" ;;
    esac
    # The optional suffix group matches only ptx + the selected SM buckets, so
    # the win_* Windows builder resources bundled in the same wheel stay out;
    # the empty-suffix branch keeps the monolithic pre-10.15 resource working.
    local trt_pattern="^tensorrt_libs/(libnvinfer\\.so\\.10|libnvinfer_plugin\\.so\\.10|libnvonnxparser\\.so\\.10|libnvinfer_builder_resource(_(ptx${bucket_re}))?\\.so\\..*)$"
    rm -f "$lib_dir"/libnvinfer*.so* "$lib_dir"/libnvonnxparser*.so* 2>/dev/null || true
    echo ">>> Vendoring TensorRT $TENSORRT_VENDOR_REF runtime (builder resources: ptx${bucket_re//|/, }):"
    extract_wheel_members "$trt_path" "$trt_pattern" "$lib_dir" || return 1
    # A builder resource MUST have landed: either the monolithic pre-10.15 file
    # or at least one per-SM/ptx variant. A typo in TENSORRT_SMS would otherwise
    # ship a runtime that cannot build engines.
    if ! ls "$lib_dir"/libnvinfer_builder_resource*.so.* >/dev/null 2>&1; then
      echo "ERROR: no libnvinfer_builder_resource* extracted — check TENSORRT_SMS ('$sms') against the wheel contents." >&2
      return 1
    fi
    required_libs+=(libnvinfer.so.10 libnvonnxparser.so.10)
  fi

  # --- cuDNN 9 runtime; the wheel URL on files.pythonhosted.org contains a
  # content hash, so resolve it through the PyPI JSON API. ---
  local cudnn_wheel="nvidia_cudnn_cu13-$CUDNN_VENDOR_REF-py3-none-manylinux_2_27_$WHEEL_ARCH.whl"
  local cudnn_path="$download_dir/$cudnn_wheel"
  if [ ! -f "$cudnn_path" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
    local cudnn_url
    cudnn_url="$(curl -fsSL --max-time 30 "https://pypi.org/pypi/nvidia-cudnn-cu13/$CUDNN_VENDOR_REF/json" \
      | WHEEL_NAME="$cudnn_wheel" python3 -c '
import json, os, sys
data = json.load(sys.stdin)
for url in data["urls"]:
    if url["filename"] == os.environ["WHEEL_NAME"]:
        print(url["url"])
        break
else:
    sys.exit(f"wheel {os.environ['WHEEL_NAME']} not found in PyPI release")')"
    download_cached "$cudnn_url" "$cudnn_path"
  fi
  verify_wheel_checksum "$cudnn_path" "${CUDNN_VENDOR_SHA256:-}" || return 1
  rm -f "$lib_dir"/libcudnn*.so* 2>/dev/null || true
  echo ">>> Vendoring cuDNN $CUDNN_VENDOR_REF runtime:"
  extract_wheel_members "$cudnn_path" '^nvidia/cudnn/lib/libcudnn.*\.so\.9.*$' "$lib_dir" || return 1
  required_libs+=(libcudnn.so.9)

  # --- CUDA toolkit runtime libs the EPs DT_NEED (a driver-only host has no
  # toolkit): cudart, cublas/cublasLt, cufft, curand. libnvblas and libcufftw
  # are deliberately excluded — nothing in the EP chain references them. ---
  if [ "${TENTAFLOW_SKIP_CUDA_VENDOR:-0}" = "1" ]; then
    echo ">>> Skipping CUDA toolkit runtime vendoring (TENTAFLOW_SKIP_CUDA_VENDOR=1 — system CUDA toolkit expected on the target host)."
  else
    rm -f "$lib_dir"/libcudart*.so* "$lib_dir"/libcublas*.so* \
          "$lib_dir"/libcufft*.so* "$lib_dir"/libcurand*.so* 2>/dev/null || true
    vendor_cuda_wheel nvidia-cuda-runtime "$CUDART_VENDOR_REF" 'libcudart\.so\..*' || return 1
    vendor_cuda_wheel nvidia-cublas "$CUBLAS_VENDOR_REF" 'libcublas(Lt)?\.so\..*' || return 1
    vendor_cuda_wheel nvidia-cufft "$CUFFT_VENDOR_REF" 'libcufft\.so\..*' || return 1
    vendor_cuda_wheel nvidia-curand "$CURAND_VENDOR_REF" 'libcurand\.so\..*' || return 1
    required_libs+=(libcudart.so.13 libcublas.so.13 libcublasLt.so.13 libcufft.so.12 libcurand.so.10)
  fi

  # --- Post-vendor sanity: both EP provider libs must resolve their NVIDIA
  # deps from the vendored dir (this is exactly what $ORIGIN gives the
  # binary). ---
  local missing=0
  for lib in "${required_libs[@]}"; do
    if [ ! -f "$lib_dir/$lib" ]; then
      echo "ERROR: vendored $lib missing in $lib_dir" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || return 1
  if command -v ldd >/dev/null 2>&1; then
    local provider unresolved
    for provider in libonnxruntime_providers_tensorrt.so libonnxruntime_providers_cuda.so; do
      [ -f "$lib_dir/$provider" ] || continue
      unresolved="$(LD_LIBRARY_PATH="$lib_dir" ldd "$lib_dir/$provider" 2>/dev/null | grep 'not found' | awk '{print $1}' | tr '\n' ' ' || true)"
      if [ -n "$unresolved" ]; then
        echo ">>> NOTE: $provider still resolves these from the target host: $unresolved" >&2
      else
        echo ">>> $provider resolves fully against the vendored lib-dynamic."
      fi
    done
  fi
  # `|| true` guards pipefail: with TENTAFLOW_SKIP_CUDA_VENDOR=1 some globs
  # stay unexpanded and du exits non-zero while still totalling the rest.
  local vendored_size
  vendored_size="$(du -shc "$lib_dir"/libnvinfer*.so* "$lib_dir"/libnvonnxparser*.so* "$lib_dir"/libcudnn*.so* \
    "$lib_dir"/libcudart*.so* "$lib_dir"/libcublas*.so* "$lib_dir"/libcufft*.so* "$lib_dir"/libcurand*.so* 2>/dev/null \
    | tail -n1 | awk '{print $1}' || true)"
  echo ">>> Vendored NVIDIA runtimes total: $vendored_size"
  if [ "${TENTAFLOW_SKIP_TENSORRT_VENDOR:-0}" != "1" ]; then
    append_manifest_library "$PLATFORM" "tensorrt-runtime" "dynamic" "$TENSORRT_VENDOR_REF" \
      "Vendored from tensorrt-cu13-libs wheel (nvinfer/plugin/onnxparser + builder resources); TENTAFLOW_SKIP_TRT_VENDOR=1 skips."
  fi
  append_manifest_library "$PLATFORM" "cudnn-runtime" "dynamic" "$CUDNN_VENDOR_REF" \
    "Vendored from nvidia-cudnn-cu13 wheel (full split-lib set, ~1 GB)."
  if [ "${TENTAFLOW_SKIP_CUDA_VENDOR:-0}" != "1" ]; then
    append_manifest_library "$PLATFORM" "cuda-runtime" "dynamic" \
      "cudart=$CUDART_VENDOR_REF cublas=$CUBLAS_VENDOR_REF cufft=$CUFFT_VENDOR_REF curand=$CURAND_VENDOR_REF" \
      "Vendored CUDA toolkit runtime (cudart/cublas/cublasLt/cufft/curand, ~1 GB); TENTAFLOW_SKIP_CUDA_VENDOR=1 skips. Total vendored NVIDIA runtimes: $vendored_size."
  fi
}

# True when an NVIDIA GPU is visible on this host (drives GPU-provider vendoring
# so a plain build-all.sh run auto-provisions the GPU stack only where it helps).
gpu_present() {
  nvidia-smi -L >/dev/null 2>&1
}

# Extracts the aarch64 GPU providers (base runtime + shared/cuda/tensorrt) from
# the Jetson AI Lab `onnxruntime-gpu` aarch64 wheel into lib-dynamic, replacing
# the CPU-only base so the provider-bridge ABI matches (all from one wheel).
provision_aarch64_gpu_ort() {
  require_cmd curl python3
  local lib_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic"
  local download_dir="$NATIVE_CACHE/downloads"
  mkdir -p "$download_dir"
  local wheel_path="$download_dir/$ONNXRUNTIME_AARCH64_GPU_WHEEL"
  download_cached "$ONNXRUNTIME_AARCH64_GPU_URL" "$wheel_path" || return 1
  verify_wheel_checksum "$wheel_path" "${ONNXRUNTIME_AARCH64_GPU_SHA256:-}" || return 1
  mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
  clean_stale_runtime
  echo ">>> Vendoring aarch64 ONNX Runtime GPU $ONNXRUNTIME_AARCH64_GPU_REF (base + shared/cuda/tensorrt providers):"
  extract_wheel_members "$wheel_path" \
    '^onnxruntime/capi/(libonnxruntime\.so\..*|libonnxruntime_providers_(shared|cuda|tensorrt)\.so)$' \
    "$lib_dir" || return 1
  sanity_check_gpu_linux
  append_manifest_library "$PLATFORM" "onnxruntime" "dynamic" "$ONNXRUNTIME_AARCH64_GPU_REF" \
    "aarch64 GPU wheel (Jetson AI Lab sbsa/cu130): CUDA+TensorRT providers with native sm_121 (GB10) cubins; ONNXRUNTIME_GPU=0 -> CPU-only Microsoft tarball."
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

  # The ORT provider libs (providers_cuda/tensorrt) ship with an EMPTY rpath and
  # are dlopened by libonnxruntime at session-build time. The main binary's
  # DT_RUNPATH ($ORIGIN + native-libs) does NOT propagate to transitively
  # dlopened libraries, so on a host without system TensorRT/cuDNN the provider's
  # DT_NEEDED (libnvinfer.so.10, libcudnn.so.9, libcublas*, libcudart) fail to
  # resolve, the GPU EP silently soft-fails, and inference falls back to CPU
  # (~60x slower — 250 ms vs 4 ms/frame). Give each provider its own `$ORIGIN`
  # rpath so it finds the sibling vendored NVIDIA runtimes regardless of the
  # launch LD_LIBRARY_PATH. The NVIDIA wheel libs already carry $ORIGIN rpaths.
  if command -v patchelf >/dev/null 2>&1; then
    for provider in libonnxruntime_providers_cuda.so libonnxruntime_providers_tensorrt.so libonnxruntime_providers_shared.so; do
      [ -f "$lib_dir/$provider" ] || continue
      if [ -z "$(patchelf --print-rpath "$lib_dir/$provider" 2>/dev/null)" ]; then
        patchelf --set-rpath '$ORIGIN' "$lib_dir/$provider" \
          && echo "    rpath: set \$ORIGIN on $provider (self-resolve vendored NVIDIA deps)"
      fi
    done
  else
    echo ">>> WARNING: patchelf not found — GPU provider libs keep an empty rpath and may fall back to CPU unless launched with LD_LIBRARY_PATH=<lib-dynamic>. Install patchelf (setup.sh does)." >&2
  fi

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
  # A source build implies the CUDA 13 toolchain; the target host still needs
  # the TRT/cuDNN runtimes, so vendor them the same way as the prebuilt path.
  vendor_nvidia_runtimes
  exit 0
fi

# ---------------------------------------------------------------------------
# Mode: dynamic, linux-aarch64 GPU — Microsoft ships no aarch64 GPU tarball, so
# a GPU host (nvidia-smi visible) gets the community `onnxruntime-gpu` aarch64
# wheel (base + CUDA/TensorRT providers, native sm_121 cubins) + cuDNN. The
# CUDA toolkit and TensorRT are host-side by default (see the skip flags below),
# so only cuDNN is vendored for the CUDA EP. ONNXRUNTIME_GPU=0 forces the
# CPU-only Microsoft aarch64 tarball via the generic path below.
# ---------------------------------------------------------------------------
if [ "$PLATFORM" = "linux-aarch64" ] && [ "${ONNXRUNTIME_GPU:-1}" = "1" ]; then
  if gpu_present; then
    provision_aarch64_gpu_ort
    # aarch64/GB10 defaults: sm_121 TRT is unproven -> CUDA EP only; the CUDA 13
    # sbsa toolkit lives at /usr/local/cuda (ldconfig) so its runtime libs need
    # not be vendored. Both respect an explicit user override.
    : "${TENTAFLOW_SKIP_TENSORRT_VENDOR:=1}"
    : "${TENTAFLOW_SKIP_CUDA_VENDOR:=1}"
    export TENTAFLOW_SKIP_TENSORRT_VENDOR TENTAFLOW_SKIP_CUDA_VENDOR
    vendor_nvidia_runtimes
    exit 0
  fi
  echo ">>> No NVIDIA GPU detected on this aarch64 host — provisioning the CPU-only ONNX Runtime tarball (set ONNXRUNTIME_GPU=0 to silence, or run on a GB10 box for the GPU stack)."
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
mkdir -p "$UNPACK/raw"
case "$ARCHIVE" in
  *.tgz) tar -xzf "$ARCHIVE_PATH" -C "$UNPACK/raw" ;;
  *.zip)
    require_cmd unzip
    unzip -q "$ARCHIVE_PATH" -d "$UNPACK/raw"
    ;;
esac

if [ -d "$UNPACK/raw/include" ] && [ -d "$UNPACK/raw/lib" ]; then
  package_root="$UNPACK/raw"
else
  package_root="$(find "$UNPACK/raw" -mindepth 1 -maxdepth 1 -type d | head -n1)"
fi
if [ -z "$package_root" ] || [ ! -d "$package_root/include" ] || [ ! -d "$package_root/lib" ]; then
  echo "Invalid ONNX Runtime archive layout: expected include/ and lib/ in $ARCHIVE" >&2
  exit 1
fi
cp -Rf "$package_root/." "$UNPACK/"
rm -rf "$UNPACK/raw"

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

# CUDA-12 hosts pair with a system TensorRT/cuDNN matching their driver; only
# the CUDA-13 variant targets self-contained deployments (B300 has no shell
# access for installing system packages).
if [ "$GPU_LINUX" = "1" ] && [ "$CUDA_MAJOR" = "13" ]; then
  vendor_nvidia_runtimes
fi
