#!/usr/bin/env bash
# ===== File: gen-rust.sh — regenerate the Rust addon-SDK UI catalog module =====
# Emits tentaflow-core/addon-sdk/sdk/src/ui_v1/components_g.rs from the
# tentaflow-sdk-spec registry via the tentaflow-sdk-gen gen_rust binary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/tentaflow-core/addon-sdk/sdk/src/ui_v1/components_g.rs"

cargo run --quiet \
    --manifest-path "$REPO_ROOT/tentaflow-sdk-gen/Cargo.toml" \
    --bin tentaflow-sdk-gen-rust > "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
echo "generated $OUT"
