#!/usr/bin/env sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cargo build --release --target-dir "$root/target" --manifest-path "$root/Cargo.toml"
cp "$root/target/release/tentaflow-coding-agent-bridge" "$root/server"
