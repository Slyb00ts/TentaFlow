#!/usr/bin/env bash
# ===== File: build.sh — compile the CUDA MMQ kernel to a committed cubin =====
#
# The raw-CUDA kernel families in FORGE (ADR-0001 exceptions; see
# docs/CODEGEN_PROOF.md). Run BESIDE the Mojo build (`pixi run mojo
# build_kernels.mojo`); they do NOT touch manifest.json (Mojo-owned). Cubins are
# committed like the PTX artifacts and embedded by forge-kernels.
#
# These cubins are sm_89 SASS with no PTX JIT fallback, so forge-kernels loads
# them only on Ada+ parts; pre-Ada GPUs (e.g. RTX 3090 sm_86) run the portable
# Mojo PTX paths instead.
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

# W4A8 (int4-weight x int8-activation) prefill GEMM — QServe dense_kernel0 in-tree
# (ADR-0001 exception, MIT). Committed cubin, loaded via the same cuModuleLoadData
# path. Non-default: routed only under FORGE_GEMM=w4a8 (see forge-kernels).
nvcc -arch="${ARCH}" -cubin -O3 \
    -o "${OUT}/w4a8_gemm_cuda.cubin" \
    "${HERE}/w4a8_gemm.cu"

echo "wrote ${OUT}/w4a8_gemm_cuda.cubin"
cuobjdump -res-usage "${OUT}/w4a8_gemm_cuda.cubin" | grep -E 'Function|REG' || true

# Tensor-core causal flash-attention prefill (fattn_prefill.cu; ADR-0001 exception).
# f16 mma QK^T + online-softmax + P·V over the paged KV cache. Committed cubin,
# loaded via the same cuModuleLoadData path. Non-default: routed only under
# FORGE_ATTN=fa (the Mojo scalar attn_prefill stays the default).
nvcc -arch="${ARCH}" -cubin -O3 \
    -o "${OUT}/fattn_prefill_cuda.cubin" \
    "${HERE}/fattn_prefill.cu"

echo "wrote ${OUT}/fattn_prefill_cuda.cubin"
cuobjdump -res-usage "${OUT}/fattn_prefill_cuda.cubin" | grep -E 'Function|REG' || true
