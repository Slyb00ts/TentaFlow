#!/usr/bin/env bash
# ===== ptxas_fp8_shim.sh — external ptxas wrapper that lifts PTX ISA .version =====
# Mojo's embedded NVPTX assembler emits `.version 8.1` for sm_89, which ptxas
# rejects for fp8 (e4m3) mma (needs >= 8.4). Point MODULAR_NVPTX_COMPILER_PATH at
# this script: it rewrites the `.version` line of the input .ptx to 8.4 (only when
# lower) and forwards every argument unchanged to the real ptxas. Ada has 4th-gen
# fp8 tensor cores, so the resulting cubin is hardware-valid; this only removes an
# emitter-side version cap, it does not change kernel semantics.
set -euo pipefail

REAL_PTXAS="${FORGE_REAL_PTXAS:-/opt/cuda/bin/ptxas}"
if [[ -n "${FORGE_PTXAS_AUDIT_LOG:-}" ]]; then
    printf '%q ' "$@" >> "$FORGE_PTXAS_AUDIT_LOG"
    printf '\n' >> "$FORGE_PTXAS_AUDIT_LOG"
fi

# Find the .ptx input argument (ptxas is invoked as: ptxas [opts] input.ptx -o out ...).
patched=()
for arg in "$@"; do
    if [[ "$arg" == *.ptx && -f "$arg" ]]; then
        tmp="$(mktemp --suffix=.ptx)"
        # Bump only 8.0/8.1/8.2/8.3 -> 8.4; leave >= 8.4 untouched.
        awk '
            /^\.version[ \t]+8\.[0-3]([ \t]|$)/ { print ".version 8.4"; next }
            { print }
        ' "$arg" > "$tmp"
        patched+=("$tmp")
    else
        patched+=("$arg")
    fi
done

exec "$REAL_PTXAS" "${patched[@]}"
