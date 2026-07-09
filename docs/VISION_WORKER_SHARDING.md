# Multi-process vision workers — staged execution checklist

Goal: ~120 cameras @25fps per GPU (3000 det/s), each frame within its 33-40 ms budget.
Measured basis (cam_scale, clean B300): ONE process walls at ~1400 forwards·batch/s at
<50% GPU util (kernel-launch serialization on the shared CUDA context — more
sessions/workers do NOT help); separate PROCESSES scale past it: 2 procs = 2199/s,
3 procs = 3008/s at 93-97% util (the fp16 compute ceiling). INT8 explicitly rejected
by the operator. MPS is NOT the mechanism (the wall is in-process submission, not
cross-process multiplexing) — only a Stage-A side experiment.

## Decision: Option (b) — full vision workers
Same `tentaflow` binary, new slim `vision-worker` subcommand mode. Each worker OWNS a
camera subset end-to-end: GStreamer/NVDEC ingest → NV12 device → detect → enrichment —
zero frame IPC, the entire zero-copy machinery works unmodified. Core spawns/supervises
N workers per GPU (default 3), shares TENTAFLOW_HOME (models, keys, recordings), workers
open the SQLite READ-ONLY (WAL; core stays the only writer). One loopback link per worker
(QUIC/UDS, CBOR frames reusing mesh payload shapes) carries: detections (coalesced
batches), fMP4 for visible tiles (camera_relay server/source shapes), frame pickups,
recording metadata, health. Mesh still sees ONE node per box (no parallel mesh).

Rejected: (a) detector-only inference workers — 2.8 GB/s frame IPC, kills zero-copy,
core remains a launch hotspot; (c) N meshed instances — no detections relay either,
splits camera rows/org model, ×N identity/ops surface.

## Camera assignment
`cameras.vision_worker_slot INTEGER` (nullable, migration) — core-owned, persisted,
default `hash(camera_id) % N` written at assign time. Workers act ONLY on
AssignCamera/RemoveCamera link commands (no reading the column — no split-brain).
Core gates `ensure_analysis` call sites (camera.rs hydrate + dispatch/camera_detections.rs)
on "assigned locally?".

## VRAM budget
Per worker ≈ 13-15 GB (RF-DETR 4×2.6 + enrichment pools + NVDEC/CUDA + buffers).
3 workers/GPU ≈ 40-45 GB of 275 GB B300. Knobs per worker: TENTAFLOW_VISION_GPUS=<one>,
_DETECTOR_SESSIONS, _INFLIGHT, _OPT/MAX_BATCH. Do NOT mix in CUDA_VISIBLE_DEVICES.

## Stages (gate = the REAL dashboard, not benches)
- **A — worker mode + spawn/supervision**: `tentaflow vision-worker {worker_id, gpu, link, token}`
  (main.rs subcommand); `vision_worker/mod.rs` boots slim runtime (logging, RO DB pool,
  GStreamer, vision singletons, LOCAL-ONLY ModelRuntimeExecutor with empty mesh slot +
  set_runtime_slot like routing/router.rs); core `services/vision_worker/supervisor.rs`
  spawns per (GPU × TENTAFLOW_VISION_WORKERS default 3) copying deploy/binary.rs spawn
  discipline (process_group(0), kill-group, piped logs) + supervisor.rs probe/backoff;
  link v0 (Hello/Heartbeat/Shutdown). GATE: N workers up, aggregate ≥3000 fwd·batch/s
  at >90% util (productizes cam_scale-mp); side experiment: same under MPS.
- **B — assignment + ingest + detections on the ONE dashboard**: migration + repository;
  link msgs AssignCamera(config+credentials_encrypted)/RemoveCamera/DetectionsBatch
  (coalesced per flush tick)/CameraHealth; worker: add_camera + ensure_analysis on assign,
  forwards its local detection_bus over the link; core: DetectionsBatch →
  publish_detections (dashboard handler unchanged); video via camera_relay-shaped
  server/source over the link, lazy on first tile subscribe. GATE (product): N real
  cameras on workers, tile shows video+overlay, capture→publish p99 < 40 ms per frame,
  ramp to 120 cams / 3 workers; decode-only NVDEC ramp first.
- **C — enrichment + frame lifecycle**: cold path moves with the camera (free) — verify
  alias resolution via the worker's local executor; frame pickup fallback over the link
  (frame_proxy shapes); recordings: worker writes files to shared dir, CORE inserts the
  DB row (single-writer). GATE: stan/tekst on overlay for worker cameras; snapshot +
  recording playback works.
- **D — failure handling + rebalance**: heartbeat timeout → kill group → respawn →
  REPLAY assignments (workers stateless, camera dark < 10 s); optional rebalance rewrites
  vision_worker_slot on permanent failure; clean shutdown = Shutdown → vision_analysis::
  drain() + ingest drain. GATE: kill -9 a worker under 120-cam load → tiles recover
  < 10 s, no zombie GPU memory, no double-publishing.

## Risks
Overlay anchoring (relay video+detections from the SAME worker pipeline — never a second
RTSP session); detections link is new code — keep it latest-wins/drop-on-lag so
backpressure can't stall the engine; NVDEC engine throughput at 120 sessions (decode-only
ramp gates it); VRAM ×N (fail-loud OOM exists); worker strictly RO on SQLite (writes via
link); process_group kill discipline (GPU memory is the zombie-resource); single-writer
assignment (no double analysis); mesh unaffected (workers never resolve MeshForward).

See memory: vision-capacity-one-gpu, no-benches-on-production-gpu. Related:
docs/GPU_RESIDENT_VIDEO_PATH.md (the in-process GPU path the workers inherit).
