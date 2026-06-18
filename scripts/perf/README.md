# Camera ingest per-frame perf harness

Isolated, network-free harness for measuring and profiling the per-frame camera
ingest hot path on the way to **thousands of cameras @ 25 fps**. It does **not**
run the production server, GStreamer, RTSP, or any socket — it drives the exact
appsink-callback body (`to_vec` + `Arc::from` + LRU insert + bus broadcast) as
camera-free benches in `tentaflow-core`.

## What it measures

| Bench | File | What |
|-------|------|------|
| `camera_frame_hotpath_perf` | `tentaflow-core/benches/camera_frame_hotpath_perf.rs` | criterion timings: frame clone, zero-copy baseline, insert+broadcast (0/1/4 subs), full callback — per resolution (720p / 1080p / 1600x1200) |
| `camera_frame_allocs` | `tentaflow-core/benches/camera_frame_allocs.rs` | dhat: exact allocations + bytes **per frame**, current vs zero-copy |

The unit measured is **one frame**. Per-camera-second cost = `full_callback`
time × 25. Multiply by the camera count for the fleet cost.

## Run

All cargo calls force `RUSTC_WRAPPER=` (sccache is broken repo-wide) and use the
shared `target_shared/` dir.

```bash
# everything: timings + alloc counts + flamegraph
./scripts/perf/camera-hotpath.sh all

# just criterion timings
./scripts/perf/camera-hotpath.sh bench

# just allocation counts (FRAMES env scales the sample, default 2000)
FRAMES=5000 ./scripts/perf/camera-hotpath.sh allocs

# flamegraph -> scripts/perf/out/camera-hotpath.svg  (needs cargo install flamegraph)
./scripts/perf/camera-hotpath.sh flame

# perf record -> scripts/perf/out/perf.data         (needs linux perf)
./scripts/perf/camera-hotpath.sh perf
```

Or directly:

```bash
cd tentaflow-core
RUSTC_WRAPPER= cargo bench --features camera --bench camera_frame_hotpath_perf
RUSTC_WRAPPER= cargo bench --features camera --bench camera_frame_allocs
```

## Reading the results

- `frame_clone_pipeline/to_vec_then_arc` vs `zero_copy_baseline/single_arc` —
  the gap is the **memcpy + extra allocation** the zero-copy redesign removes.
- `insert_broadcast` across 0/1/4 subscribers — the lock-contention slope from
  the LRU `Mutex` + DashMap shard + per-subscriber `try_send`.
- `camera_frame_allocs` `allocs/frame` — drops from the current count to the
  zero-copy count; `bytes/frame` shows the eliminated copy volume.

## Scope / non-goals

- No real decode: the benches start from an already-decoded RGB24 buffer, so
  they isolate the **post-decode** per-frame overhead this analysis targets.
  Decode cost (NVDEC vs CPU) is a separate axis covered by the optimization plan
  (decode-on-demand), not by this harness.
- No production server boot, no port binding, no native RTSP — safe to run on a
  dev box without touching a running TentaFlow instance.
