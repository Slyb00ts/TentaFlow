#!/usr/bin/env bash
# =============================================================================
# File: scripts/perf/camera-hotpath.sh — per-frame ingest profiling driver
# =============================================================================
#
# Isolated load/profiling harness for the camera per-frame hot path. It does NOT
# touch GStreamer, RTSP, the network, or the production server binary — it builds
# and profiles the camera-free benches in tentaflow-core (camera_frame_*), which
# replicate the appsink callback body (to_vec + Arc + LRU insert + bus
# broadcast). This is the unit replicated `cameras * 25` times per second.
#
# Subcommands:
#   bench      cargo bench --bench camera_frame_hotpath_perf  (criterion timings)
#   allocs     cargo bench --bench camera_frame_allocs        (dhat alloc counts)
#   flame      cargo flamegraph over the hot-path bench        -> camera-hotpath.svg
#   perf       perf record -g over the hot-path bench          -> perf.data
#   all        bench + allocs + flame
#
# Conventions: sccache is broken in this repo, so every cargo invocation forces
# RUSTC_WRAPPER= (empty). Builds use the repo-shared target_shared/ dir.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORE_DIR="${REPO_ROOT}/tentaflow-core"
OUT_DIR="${REPO_ROOT}/scripts/perf/out"
mkdir -p "${OUT_DIR}"

# sccache is broken repo-wide — force it off for every cargo call here.
export RUSTC_WRAPPER=
# Camera benches need the camera feature for the video pipeline types.
FEATURES="camera"
# Profiling needs frame pointers + symbols in the release bench binary.
export CARGO_PROFILE_BENCH_DEBUG=1
export CARGO_PROFILE_RELEASE_DEBUG=1

cmd="${1:-all}"

bench_timings() {
  echo ">> criterion timings (camera_frame_hotpath_perf)"
  ( cd "${CORE_DIR}" && cargo bench --features "${FEATURES}" --bench camera_frame_hotpath_perf )
}

bench_allocs() {
  echo ">> dhat allocation counts (camera_frame_allocs)"
  ( cd "${CORE_DIR}" && FRAMES="${FRAMES:-2000}" cargo bench --features "${FEATURES}" --bench camera_frame_allocs )
}

flame() {
  echo ">> flamegraph over camera_frame_hotpath_perf -> ${OUT_DIR}/camera-hotpath.svg"
  command -v cargo-flamegraph >/dev/null 2>&1 || {
    echo "!! cargo-flamegraph missing — install: cargo install flamegraph" >&2
    exit 2
  }
  ( cd "${CORE_DIR}" && cargo flamegraph --features "${FEATURES}" --bench camera_frame_hotpath_perf \
      --output "${OUT_DIR}/camera-hotpath.svg" \
      -- --bench --profile-time 20 )
  echo "   open ${OUT_DIR}/camera-hotpath.svg"
}

perf_record() {
  echo ">> perf record -g over camera_frame_hotpath_perf -> ${OUT_DIR}/perf.data"
  command -v perf >/dev/null 2>&1 || {
    echo "!! perf missing — install linux-perf / linux-tools" >&2
    exit 2
  }
  local bin
  bin="$( cd "${CORE_DIR}" && cargo bench --features "${FEATURES}" --bench camera_frame_hotpath_perf --no-run --message-format=json 2>/dev/null \
    | grep -o '"executable":"[^"]*camera_frame_hotpath_perf[^"]*"' | head -1 | cut -d'"' -f4 )"
  [ -n "${bin}" ] || { echo "!! could not locate bench binary" >&2; exit 2; }
  ( cd "${OUT_DIR}" && perf record -g --call-graph dwarf -o perf.data \
      "${bin}" --bench --profile-time 20 )
  echo "   inspect: perf report -i ${OUT_DIR}/perf.data"
}

case "${cmd}" in
  bench)  bench_timings ;;
  allocs) bench_allocs ;;
  flame)  flame ;;
  perf)   perf_record ;;
  all)    bench_timings; bench_allocs; flame ;;
  *) echo "usage: $0 {bench|allocs|flame|perf|all}" >&2; exit 1 ;;
esac
