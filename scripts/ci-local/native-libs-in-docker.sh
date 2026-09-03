#!/usr/bin/env bash
# =============================================================================
# File: scripts/ci-local/native-libs-in-docker.sh
# Purpose: Build native-libs the way a CI job would, in a container held to the
#          runner's shape (4 vCPU, 16 GB RAM), recording per-step wall time and
#          free disk. This answers the question that gates the release work: does
#          a cold native-libs build fit inside a 6-hour job on a runner with
#          ~14 GB free?
#
#          The image is ubuntu:22.04, not 24.04, for two reasons that both bite
#          in CI: zvec needs gcc-11 (RocksDB 8.1 does not build under gcc>=13)
#          and otherwise shells out to a sibling docker container, which a
#          container cannot do; and the glibc a native library is built against
#          becomes the floor for every machine that later installs the release
#          (24.04 would demand 2.39 and lock out Debian 12).
#          cmake comes from pip, not apt: zvec wants the [3.26, 4) window and
#          jammy ships 3.22 — the same trick zvec's own docker fallback uses.
#
# The repo is mounted from a throwaway git worktree, never the working tree —
# native-libs/ is a build output and a container run must not replace the
# CUDA-flavoured libraries a developer has locally.
#
# Usage: scripts/ci-local/native-libs-in-docker.sh [worktree_dir] [cache_dir]
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKTREE="${1:-$HOME/.cache/tentaflow-ci-local/worktree}"
CACHE="${2:-$HOME/.cache/tentaflow-ci-local/native-cache}"
LOG_DIR="${TENTAFLOW_CI_LOG_DIR:-$HOME/.cache/tentaflow-ci-local/logs}"

mkdir -p "$(dirname "$WORKTREE")" "$CACHE" "$LOG_DIR"

if [ ! -d "$WORKTREE/.git" ] && [ ! -f "$WORKTREE/.git" ]; then
  echo "[ci-local] creating worktree at $WORKTREE"
  git -C "$REPO_ROOT" worktree add --detach "$WORKTREE" HEAD
else
  echo "[ci-local] reusing worktree at $WORKTREE"
fi

# The runner shape: ubuntu-latest is 4 vCPU / 16 GB. Disk is the tighter limit
# (~14 GB free), so the run reports free space around every step instead of
# pretending a local 100 GB disk proves anything.
exec docker run --rm --name tentaflow-m0 \
  --cpus=4 --memory=16g \
  -v "$WORKTREE:/src" \
  -v "$CACHE:/native-cache" \
  -e TENTAFLOW_NATIVE_CACHE=/native-cache \
  -e ONNXRUNTIME_GPU=0 \
  -e DEBIAN_FRONTEND=noninteractive \
  -w /src \
  ubuntu:22.04 \
  bash -euo pipefail -c '
    step() { echo "[ci-local] $(date -u +%H:%M:%S) === $* === free=$(df -Ph /src | tail -1 | awk "{print \$4}")"; }

    step "apt: base toolchain (gcc-11 + cmake<4 + ninja — zvec builds natively only with these)"
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      build-essential gcc-11 g++-11 ninja-build git curl wget ca-certificates pkg-config \
      python3 python3-venv python3-pip protobuf-compiler libssl-dev unzip xz-utils \
      libglib2.0-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      clang libclang-dev >/dev/null

    step "pip: cmake in the [3.26, 4) window zvec requires"
    pip3 install -q "cmake<4" >/dev/null
    hash -r

    step "apt: Vulkan SDK (LunarG) — glslc is what enables the vulkan backend"
    wget -qO- https://packages.lunarg.com/lunarg-signing-key-pub.asc > /etc/apt/trusted.gpg.d/lunarg.asc
    CODENAME=$(. /etc/os-release && echo "$VERSION_CODENAME")
    wget -qO /etc/apt/sources.list.d/lunarg-vulkan.list \
      "https://packages.lunarg.com/vulkan/lunarg-vulkan-${CODENAME}.list"
    apt-get update -qq
    apt-get install -y -qq vulkan-sdk >/dev/null
    command -v glslc && glslc --version | head -1
    echo "[ci-local] gcc-11 $(gcc-11 -dumpversion) / $(cmake --version | head -1) / glibc $(ldd --version | head -1 | grep -o '[0-9.]*$')"

    step "rustup"
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
    . "$HOME/.cargo/env"

    step "native-libs: build-all.sh (ONNXRUNTIME_GPU=0, backends auto)"
    time ./scripts/native-libs/build-all.sh --platform linux-x86_64

    step "results"
    du -sh native-libs/linux-x86_64/* 2>/dev/null || true
    echo "[ci-local] vulkan artifact present?"
    ls -la native-libs/linux-x86_64/lib-static/llama-cpp/multi/libggml-vulkan.a 2>/dev/null \
      || echo "[ci-local] MISSING libggml-vulkan.a — the gpu-vulkan variant would be CPU-only"
  '
