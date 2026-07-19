#!/usr/bin/env bash
# ===== File: build.sh — compile the CUDA MMQ kernel to a committed cubin =====
#
# The ONE raw-CUDA kernel family in FORGE (ADR-0001 exception; see gemm_i8mma.cu
# and docs/CODEGEN_PROOF.md). Runs BESIDE the Mojo build (`pixi run mojo
# build_kernels.mojo`); it does NOT touch manifest.json (Mojo-owned). The cubin
# is committed like the PTX artifacts and embedded by forge-kernels.
#
# Requires nvcc on PATH. sm_89 matches the committed build/sm_89 convention and
# gets offline ptxas scheduling (the whole point — a cubin, not JIT PTX).
#
# Usage: kernels/cuda/build.sh [sm_arch]   (default sm_89)
set -euo pipefail

ARCH="${1:-sm_89}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${HERE}/../mojo/build/${ARCH}"
mkdir -p "${OUT}"

nvcc -arch="${ARCH}" -cubin -O3 \
    -o "${OUT}/gemm_i8mma_cuda.cubin" \
    "${HERE}/gemm_i8mma.cu"

echo "wrote ${OUT}/gemm_i8mma_cuda.cubin"
cuobjdump -res-usage "${OUT}/gemm_i8mma_cuda.cubin" | grep -E 'Function|REG' || true
