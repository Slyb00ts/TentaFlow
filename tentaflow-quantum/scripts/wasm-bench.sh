#!/usr/bin/env bash
# ===== File: scripts/wasm-bench.sh — build the browser glue and run the spike B harness =====
#
# Produces exactly what tentaflow-core/build.rs produces (cargo build for
# wasm32-unknown-unknown with feature `wasm`, then wasm-bindgen --target web)
# but into a scratch directory, then runs scripts/wasm-bench.mjs against it.
# Nothing here writes into tentaflow-core/www.
#
#   ./scripts/wasm-bench.sh              # build into a temp dir and measure
#   ./scripts/wasm-bench.sh <out-dir>    # keep the glue somewhere for a browser
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-}"
if [[ -z "$out_dir" ]]; then
  out_dir="$(mktemp -d "${TMPDIR:-/tmp}/tentaquant-glue.XXXXXX")"
  trap 'rm -rf "$out_dir"' EXIT
fi
mkdir -p "$out_dir"

if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
  echo "missing target: rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "missing wasm-bindgen CLI: cargo install wasm-bindgen-cli --version 0.2.125 --locked" >&2
  exit 1
fi

# A separate target directory, like build.rs uses, so a wasm build never takes
# the lock a native build is holding.
target_dir="${TENTAQUANT_WASM_TARGET:-$crate_dir/target/wasm-bench}"

cargo build \
  --manifest-path "$crate_dir/Cargo.toml" \
  --target wasm32-unknown-unknown \
  --release \
  --features wasm \
  --target-dir "$target_dir"

wasm-bindgen \
  --target web \
  --out-dir "$out_dir" \
  --out-name quantum_glue \
  --no-typescript \
  "$target_dir/wasm32-unknown-unknown/release/tentaflow_quantum.wasm"

ls -l "$out_dir"
echo
node "$crate_dir/scripts/wasm-bench.mjs" "$out_dir"
