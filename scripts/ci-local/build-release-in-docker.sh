#!/usr/bin/env bash
# =============================================================================
# File: scripts/ci-local/build-release-in-docker.sh
# Purpose: Second half of the M0 measurement — the release build itself, against
#          the native-libs produced by native-libs-in-docker.sh, in the same
#          container shape (4 vCPU, 16 GB, ubuntu:22.04). Reports wall time,
#          peak disk, binary size and the resulting NEEDED list.
#
#          The NEEDED list is the acceptance test, not the exit code: a build
#          that succeeds but links libcudart has silently produced an artifact
#          that will not start on a machine without CUDA.
#
# Usage: scripts/ci-local/build-release-in-docker.sh [edition] [worktree] [cache]
#        edition: full (default) | slim
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EDITION="${1:-full}"
WORKTREE="${2:-$HOME/.cache/tentaflow-ci-local/worktree}"
CARGO_CACHE="${3:-$HOME/.cache/tentaflow-ci-local/cargo}"

case "$EDITION" in
  full) FEATURES=(--features gpu-vulkan) ;;
  slim) FEATURES=(--no-default-features) ;;
  *) echo "edition must be full or slim" >&2; exit 1 ;;
esac

[ -d "$WORKTREE/native-libs/linux-x86_64" ] || {
  echo "brak native-libs w $WORKTREE — uruchom najpierw native-libs-in-docker.sh" >&2
  exit 1
}
mkdir -p "$CARGO_CACHE/registry" "$CARGO_CACHE/git"

exec docker run --rm --name "tentaflow-m0-build-$EDITION" \
  --cpus=4 --memory=16g \
  -v "$WORKTREE:/src" \
  -v "$CARGO_CACHE/registry:/root/.cargo/registry" \
  -v "$CARGO_CACHE/git:/root/.cargo/git" \
  -e DEBIAN_FRONTEND=noninteractive \
  -e CARGO_INCREMENTAL=0 \
  -e EDITION="$EDITION" \
  -w /src/tentaflow \
  ubuntu:22.04 \
  bash -euo pipefail -c '
    step() { echo "[ci-local] $(date -u +%H:%M:%S) === $* === free=$(df -Ph /src | tail -1 | awk "{print \$4}")"; }

    step "apt: build deps"
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      build-essential cmake ninja-build git curl wget ca-certificates pkg-config \
      python3 protobuf-compiler libssl-dev clang libclang-dev libasound2-dev libvulkan-dev \
      libglib2.0-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev >/dev/null

    step "rust toolchain + wasm targets (dashboard glue and addons come from these)"
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
    . "$HOME/.cargo/env"
    rustup target add wasm32-unknown-unknown wasm32-wasip1 >/dev/null
    cargo install wasm-bindgen-cli --version 0.2.125 --locked >/dev/null 2>&1 \
      || echo "[ci-local] WARN: wasm-bindgen-cli install failed — dashboard glue would be skipped"

    step "cargo build --release ($EDITION)"
    time cargo build --release --target x86_64-unknown-linux-gnu '"${FEATURES[*]}"'

    step "results"
    TARGET_DIR=$(cargo metadata --format-version 1 --no-deps | python3 -c "import json,sys; print(json.load(sys.stdin)[\"target_directory\"])")
    BIN="$TARGET_DIR/x86_64-unknown-linux-gnu/release/tentaflow"
    ls -lh "$BIN"
    echo "[ci-local] NEEDED:"
    readelf -d "$BIN" | grep NEEDED
    echo "[ci-local] target dir size: $(du -sh "$TARGET_DIR" | cut -f1)"
  '
