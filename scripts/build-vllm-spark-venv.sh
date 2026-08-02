#!/bin/bash
# ===== File: scripts/build-vllm-spark-venv.sh — native vLLM bundle for DGX Spark =====
# Builds vLLM 0.26 from source into a self-contained venv for GB10 (sm_121a),
# then applies the one patch upstream still lacks (nvfp4_ds_mla).
#
# This is the artifact, not a convenience wrapper: the native deploy consumes
# the same recipe via `llm/python/vllm-spark/bundle.toml`, and our own clean
# image is a thin COPY over it. Building on a third-party runtime image is what
# produced the flashinfer-cubin deadlock -- that base ships cubin/jit-cache
# pinned to ITS vLLM (0.19), vLLM 0.26 wants flashinfer-python 0.26-era, and no
# matching cubin exists on PyPI. A clean venv never installs those packages at
# all, so flashinfer falls back to its own cache dir and the conflict cannot
# arise.
#
# Why source at all: the official aarch64 wheel carries no sm_121 SASS, and its
# _flashmla_C has only sm_90a/sm_100 -- the MLA path DeepSeek V4 needs is simply
# absent on GB10. Verified with cuobjdump, not assumed.
#
# Uzycie:
#   build-vllm-spark-venv.sh                 # buduj (pomija ukonczone kroki)
#   build-vllm-spark-venv.sh --clean         # od zera
#   PREFIX=/inna/sciezka build-vllm-spark-venv.sh
set -uo pipefail

PREFIX="${PREFIX:-/opt/TentaFlow/.runtime/bundles/vllm-spark}"
VLLM_REF="${VLLM_REF:-v0.26.0}"
TORCH_SPEC="${TORCH_SPEC:-torch==2.11.0+cu130}"
TORCH_INDEX="${TORCH_INDEX:-https://download.pytorch.org/whl/cu130}"
CUDA_HOME="${CUDA_HOME:-/usr/local/cuda}"
JOBS="${MAX_JOBS:-8}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# GB10 is sm_121a, but this list must say plain `12.1`: vLLM's CMake appends the
# `a` itself (CMakeLists maps 12.1 -> 12.1a for arch-specific kernels). Passing
# `12.1a` makes torch's arch parser drop the entry and silently fall back to
# 12.0, which then refuses to compile FlashMLA -- a build that succeeds and
# produces a runtime with no MLA kernels for this GPU.
ARCH_LIST="${TORCH_CUDA_ARCH_LIST:-12.1}"

VENV="$PREFIX/venv"
SRC="$PREFIX/src"
PY="$VENV/bin/python"
STAMP="$PREFIX/.stamps"

log() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
die() { printf '\n\033[1;31mBLAD: %s\033[0m\n' "$*" >&2; exit 1; }
done_step() { [ -f "$STAMP/$1" ]; }
mark() { touch "$STAMP/$1"; }

[ "${1:-}" = "--clean" ] && { log "czyszcze $PREFIX"; rm -rf "$PREFIX"; }
mkdir -p "$STAMP" || die "nie moge utworzyc $PREFIX"

command -v python3.12 >/dev/null || command -v python3 >/dev/null || die "brak pythona 3.12"
PYBIN=$(command -v python3.12 || command -v python3)
[ -x "$CUDA_HOME/bin/nvcc" ] || die "brak nvcc w $CUDA_HOME/bin — zainstaluj CUDA toolkit"
# A venv does NOT provide these -- CMake resolves Python_INCLUDE_DIRS from the
# BASE interpreter, so without the dev package the vLLM build dies at configure
# with "Could NOT find Python (missing: Python_INCLUDE_DIRS ...)" after the whole
# torch install has already succeeded.
PYVER=$("$PYBIN" -c 'import sys;print(f"{sys.version_info.major}.{sys.version_info.minor}")')
"$PYBIN" -c 'import sysconfig,os,sys; sys.exit(0 if os.path.isfile(os.path.join(sysconfig.get_paths()["include"],"Python.h")) else 1)' \
  || die "brak naglowkow Pythona — zainstaluj: sudo apt-get install -y python${PYVER}-dev"

log "venv: $VENV  (python: $($PYBIN -V 2>&1))"
if ! done_step venv; then
  "$PYBIN" -m venv "$VENV" || die "venv"
  "$PY" -m pip install -q --upgrade pip setuptools wheel || die "pip upgrade"
  mark venv
fi

# torch first and ALONE: vLLM's requirements would otherwise resolve a generic
# wheel over the cu130 aarch64 one and the CUDA kernels would build against the
# wrong runtime.
if ! done_step torch; then
  log "torch $TORCH_SPEC z $TORCH_INDEX (~2-4 min)"
  "$PY" -m pip install "$TORCH_SPEC" --index-url "$TORCH_INDEX" || die "torch"
  mark torch
fi
"$PY" - <<'EOF' || die "torch nie widzi CUDA"
import torch
print(f"torch {torch.__version__} | cuda {torch.version.cuda} | dostepne: {torch.cuda.is_available()}")
EOF

if ! done_step clone; then
  log "klonuje vllm $VLLM_REF"
  rm -rf "$SRC"
  git clone --depth 1 --branch "$VLLM_REF" https://github.com/vllm-project/vllm "$SRC" || die "git clone"
  # Reinstalling torch from the requirements would pull a non-Spark wheel over
  # the working one.
  sed -i '/^torch==/d;/^torchvision==/d;/^torchaudio==/d' "$SRC/requirements/cuda.txt"
  sed -i '/^torch *==/d' "$SRC/requirements/build/cuda.txt"
  mark clone
fi

export TORCH_CUDA_ARCH_LIST="$ARCH_LIST"
export VLLM_TARGET_DEVICE=cuda
export VLLM_USE_PRECOMPILED=0
export CUDA_HOME MAX_JOBS="$JOBS"
export CMAKE_BUILD_PARALLEL_LEVEL="$JOBS"
export PATH="$CUDA_HOME/bin:$PATH"

if ! done_step build; then
  log "kompiluje vLLM dla sm_${ARCH_LIST/./}a — MAX_JOBS=$JOBS, to potrwa 30-60 min"
  ( cd "$SRC" && "$PY" -m pip install --no-build-isolation -r requirements/build/cuda.txt ) || die "build deps"
  # Non-editable on purpose: an editable install leaves $SRC/vllm on sys.path,
  # where it SHADOWS the installed package. The patch below would then report
  # success against site-packages while the runtime imported unpatched sources.
  ( cd "$SRC" && "$PY" -m pip install --no-build-isolation . ) || die "kompilacja vllm"
  mark build
fi

log "naklada latke nvfp4_ds_mla"
"$PY" "$REPO_ROOT/tentaflow-containers/llm/patches/dspark/patch_nvfp4_026.py" || die "latka"

log "weryfikacja"
SITE=$("$PY" -c "import vllm,os;print(os.path.dirname(vllm.__file__))") || die "import vllm"
"$PY" -c "import vllm;print(f'vllm {vllm.__version__}')" || die "import vllm"

# The 12.1a fallback is invisible until a kernel launches, so prove the SASS is
# there. FlashMLA is the module that matters for DeepSeek V4.
miss=0
for so in "$SITE"/_C*.abi3.so "$SITE"/_flashmla_C*.abi3.so; do
  [ -f "$so" ] || continue
  archs=$("$CUDA_HOME/bin/cuobjdump" "$so" 2>/dev/null | grep -oE 'sm_[0-9]+a?' | sort -u | tr '\n' ' ')
  case "$archs" in
    *sm_121*) printf '  \033[32mOK\033[0m   %-34s %s\n' "$(basename "$so")" "$archs" ;;
    *)        printf '  \033[31mBRAK\033[0m %-34s %s\n' "$(basename "$so")" "$archs"; miss=1 ;;
  esac
done
[ "$miss" = 0 ] || die "brak kerneli sm_121a — build zszedl do innej architektury (sprawdz TORCH_CUDA_ARCH_LIST)"

# The patch is idempotent and errors on a missing anchor, but a successful run
# says nothing about whether the running interpreter imports THAT tree -- which
# is exactly how the docker attempt lied. Assert against the imported package.
grep -q nvfp4_ds_mla "$SITE/config/cache.py" \
  || die "nvfp4_ds_mla brak w $SITE/config/cache.py — latka trafila w inne drzewo"
echo "  latka nvfp4_ds_mla: OK ($SITE)"

log "gotowe: $VENV"
echo "serve: $VENV/bin/vllm serve ..."
