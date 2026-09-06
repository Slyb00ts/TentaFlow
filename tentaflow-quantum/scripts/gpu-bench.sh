#!/usr/bin/env bash
# ===== File: scripts/gpu-bench.sh — build the GPU backend and run the spike D harness =====
#
# Release build with feature `wgpu`, then examples/gpu_bench: one Hadamard and a
# whole GHZ circuit at 20/24/26/28 qubits on the GPU and on the CPU, plus the
# 2^28 kernel of plan 16, Faza 0, spike D and its agreement with the CPU.
#
#   ./scripts/gpu-bench.sh                        # every size, defaults
#   ./scripts/gpu-bench.sh --max-qubits 26        # stop earlier on a small card
#   ./scripts/gpu-bench.sh --compare-qubits 26    # widen the full-state check
#
# Every argument is passed straight to the example; see its header for the
# memory each flag governs.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo run \
  --manifest-path "$crate_dir/Cargo.toml" \
  --release \
  --features wgpu \
  --example gpu_bench \
  -- "$@"
