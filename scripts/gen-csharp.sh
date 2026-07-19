#!/usr/bin/env bash
# ===== File: gen-csharp.sh — regenerate the C# component catalog =====
# Emits tentaflow-sdk-dotnet/TentaFlow.Sdk/Components.g.cs from the
# tentaflow-sdk-spec registry via the tentaflow-sdk-gen gen_csharp binary.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/tentaflow-sdk-dotnet/TentaFlow.Sdk/Components.g.cs"

cargo run --quiet \
    --manifest-path "$REPO_ROOT/tentaflow-sdk-gen/Cargo.toml" \
    --bin tentaflow-sdk-gen-csharp > "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
echo "generated $OUT"
