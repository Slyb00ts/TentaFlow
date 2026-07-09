#!/bin/bash
# ===== File: llm/docker/vllm-dspark/entrypoint.sh — cluster-only DSpark launcher =====
set -euo pipefail

# Cluster deploy drives every member through ENGINE_LAUNCH_CMD (the full
# role-specific `vllm serve --nnodes ... [--headless]` command). The 284B model
# does not fit a single Spark, so there is no meaningful single-node fallback.
if [ -n "${ENGINE_LAUNCH_CMD:-}" ]; then
  exec bash -c "$ENGINE_LAUNCH_CMD"
fi

echo "vllm-dspark is a cluster-only engine (2x DGX Spark). Deploy it through a" >&2
echo "TentaFlow cluster (Katalog -> DeepSeek V4 Flash DSpark -> cel: klaster)." >&2
exit 64
