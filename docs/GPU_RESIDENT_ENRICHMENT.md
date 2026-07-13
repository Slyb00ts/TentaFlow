# GPU-Resident Enrichment — scaling camera-CV to ~100 cameras/GPU

## Problem
Measured: **one live camera uses 1–3 % of a B300** (util samples: 1,1,3,1,1,3 %). The card
is 97 % idle, yet the naive serial benchmark reported only ~5 cameras/GPU. That "5" is a
**methodology error** — it measured single-forward *latency* (~8 ms/frame) and divided
`1000/8 = 125 fps → 5 cams@25fps`, assuming the GPU runs one forward at a time. The GPU can
run dozens of forwards concurrently. The real limit is **how we feed the card**, not the card.

### Why the GPU sits idle (per-frame CPU work)
1. **JPEG decode** — CPU `jpegdec` (no `nvjpegdec` in this GStreamer build). MJPEG only; RTSP/H.264 → nvdec.
2. **Crop extraction** — CPU memcpy of each detection's sub-region from the full 4K frame.
3. **Preprocess per crop** — resize-to-model-input + RGB→gray + normalize, all on CPU, per crop.
4. **Per-forward CPU↔GPU transfer** — every ORT forward uploads input + downloads logits; for the
   tiny enrichment models (state/plate/ADR CRNN) the launch+transfer overhead **dwarfs** the compute.
5. **Postprocess** — argmax / CTC / softmax / box-decode on CPU.
6. **Tracking (IOU), overlay, CBOR serialization, IO** — CPU business logic.

Three compounding faults: **serial** (one camera, one crop at a time), **tiny forwards**
(overhead-bound), **CPU↔GPU round-trips** (frame downloaded, cropped on CPU, re-uploaded per crop).
Analogy: a freight truck hauling parcels one at a time, hand-loading between trips — 97 % idle,
not because it's slow but because it's fed wrong.

Detector nuance: RF-DETR is a heavy transformer; ~807 img/s @ batch 8 is legit for *that* model
(a Coral runs a tiny MobileNet-SSD, not comparable). But the **small enrichment models running at
~600–1000/s each is pure inefficiency** — those should do 10× more with batching + device I/O.

## Target architecture (GPU-resident, batched, concurrent)
```
Decode:   RTSP/H.264 → nvdec (GPU) → frame stays in GPU memory (NVMM), never touches CPU
Detect:   frame(GPU) → GPU resize 560 → TensorRT (device I/O) → only boxes return (tiny)
Central batcher (one per GPU, ~5–10 ms tick):
  collect crop-requests from ALL active cameras: (gpu_frame_ptr, bbox, model)
  GPU crop + resize + normalize (CV-CUDA / NPP) → one batched device tensor per model
  TensorRT batched forward, input+output bound to device memory (zero CPU transfer)
  → tiny results (labels/text) scattered back to each camera's overlay
```
CPU keeps only: box coords, tracking, overlay, serialization. Everything pixel-heavy stays on GPU.

## Technology (and today's gap)
1. **nvdec** — GPU H.264 decode (GStreamer `nvh264dec` / DeepStream). Today: CPU `jpegdec`. Ties to MJPEG→RTSP migration.
2. **CV-CUDA or NPP** — GPU crop/resize/normalize (CV-CUDA is purpose-built for batched inference preprocessing). Today: all CPU, per crop.
3. **ORT CUDA IO-binding / native TensorRT device buffers** — models consume tensors straight from GPU, output stays on GPU. Today: every forward round-trips through CPU.
4. **Central dynamic batcher** (à la NVIDIA Triton) — collects crops across cameras per tick, one batched forward per model. Today: crop-by-crop, serial.
5. **GPU postprocess** — argmax/CTC as small kernels, or pull back only tiny logits.

## Staged plan (each stage measurable: cameras/GPU + GPU util)
- **Stage 0 — real measurement** (days): concurrent multi-camera load test (N simulated streams on ONE B300), ramp N until per-frame latency exceeds the 40 ms @25fps budget or throughput plateaus → the HARD current cameras/GPU number (not the serial guess) + a util curve. Establishes the honest baseline before rewriting anything.
- **Stage 1 — cross-CAMERA batching** (best ROI, ~1–2 wk): central batcher collects crops from all cameras per tick → one forward per model. Still CPU preprocess, but big forwards fill the GPU. Biggest jump, lowest risk (no CV-CUDA yet).
- **Stage 2 — GPU-resident preprocess** (~2–4 wk): crop+resize+normalize on GPU (CV-CUDA/NPP), frame resident, ORT IO-binding device tensors — kills the CPU↔GPU round-trips.
- **Stage 3 — GPU decode** (~1–2 wk, with RTSP migration): nvdec, frame never hits CPU.
- **Stage 4 — full concurrency** (~1 wk): N concurrent camera loops feeding the shared batcher.

## Honest cost & expected outcome
This is weeks of **native engineering** (CV-CUDA/NPP via Rust FFI, IO-binding, batcher, nvdec) —
effectively building a mini inference server. Expected: from ~5 (serial) toward **50–100+
cameras/GPU** → **160 cameras on 2–4 B300s instead of 32**. Recommendation: **Stage 0 → Stage 1**
first; only go to 2–4 if 1 is insufficient.

## Stage 0 — measured (GPU 7, `examples/cam_scale.rs`, full pipeline every frame)
Real concurrent capacity, ramping N cameras @25fps until per-frame p99 > 40 ms budget:
- **Deployed config (detector pool=1):** ~10–14 cams/GPU. (The naive serial "5" was 2–3× too pessimistic.)
- **Raised pools (detector=6, enrich=8):** ~20 cams/GPU full-pipeline.
- **Detect-only (event-driven enrichment, the realistic ceiling):** ~80–100 cams/GPU → **160 cameras on ~2 B300s**.
- **GPU util stayed 0 % throughout, even saturated** → the wall is CPU/software, not the card.

## Bottleneck profiling (`perf`, cam_scale saturated) — measured, not guessed
- **#1 — ORT thread-pool BUSY-SPIN (FIXED).** 95 % of CPU in `libonnxruntime`, **82 % in one spin loop**; real GPU work (nvinfer/cuda) ~4 %. Each of the 30 pooled sessions spun its own thread pool waiting for work/GPU-sync → every core pinned spinning, GPU idle. Fix in `ort_common::session_builder_with_eps`: `with_intra_threads(1) + with_inter_threads(1) + with_{intra,inter}_op_spinning(false)` — CPU now SLEEPS on GPU sync. Result: plateau 740 → **970 frames/s (+30 %)**, spin 82 % → 1.7 %.
- **#2 — per-crop CPU preprocess + CPU↔GPU transfers (OPEN).** After #1, self-time spreads: `libcuda` 23 % (kernel launch + H2D/D2H transfers), our code 22 % + `libc` 21 % (crop/resize/normalize + memcpy/malloc), TRT 12 %. Hottest single fn: **`StateClassifier::preprocess` 9.6 %** (runs 3×/frame, per-crop resize+normalize, allocates `vec![0f32;3·S²]` each call), then transfers ~14 % (`cudaMemcpy*`), `resize_rgb` 4 %, malloc/free 5 %. This is the "thousand tiny CPU-preprocessed forwards + transfers" problem — exactly what Stage 1 (cross-camera batching → fewer/bigger launches) and Stage 2 (GPU-resident preprocess + device I/O → no CPU preprocess, no round-trips) eliminate.

## Measurement corrections + single-GPU truth (IMPORTANT)
Earlier cam_scale numbers were measured with `TENTAFLOW_VISION_GPUS=7`, which is **7 GPUs (a COUNT), not device 7** — work spread across devices 0–6 while the util sampler queried the empty GPU 7 (false "0 %"). So the "80–100 cams/GPU" figures were actually "cams across ~7 GPUs" and were ~7× too optimistic. Corrected, on a SINGLE pinned GPU (device 0), full pipeline every frame, with correct util sampling:
- **direct (per-camera): ~200 frames/s, GPU 55–60 %.**
- **batched (cross-camera batcher): ~250 frames/s (+25 %), GPU 62 %.**
- **more session pools made it WORSE** (enrich=8/det=2 → ~193 fps, GPU 50 %) — GPU is NOT session-limited; extra pools just add contention.
- **⇒ ~250 fps single-GPU = ~10 cameras/GPU full-pipeline** (detect-only + event-driven enrichment is higher). 160 cams full-pipeline ≈ 16 GPUs today; the event-driven realistic number is lower.

The GPU sits at ~60 %, NOT saturated: ~40 % of the time it is idle waiting for the CPU to prepare the next batch (matches the perf split: transfers 23 % + our preprocess 22 % + memcpy 21 %). The pools-hurt result CONFIRMS the wall is CPU preprocess + CPU↔GPU transfers, not GPU compute or concurrency. **The only lever left that can push past ~250 fps / 60 % util is GPU-resident preprocess (crop+resize+normalize on GPU + device-I/O binding).** Everything cheaper has been tried and measured: spin-fix (+30 %), batcher (+25 %), cold-consumer parallelization, preprocess-in-place, pool sizing (harmful past ~4).

## GPU-resident preprocess — tooling confirmed + first slice
All tools are present on the node: **NPP** (`libnppig`=resize/crop, `libnppial`=arithmetic/normalize, `libnppicc`=color), **nvcc** (`/usr/local/cuda-13.0`, so a custom fused crop+resize+normalize CUDA kernel is an option and often cleaner than chaining NPP), **libcudart** (FFI `cudaMalloc`/`cudaMemcpy`/`cudaFree` — no `cudarc`/`cust` crate in the tree yet), and **ORT IoBinding** (`session/io_binding.rs`: `bind_input(&Value)`, `bind_output_to_device`, `MemoryInfo::new(AllocationDevice::CUDA, …)`), which lets a device-memory tensor be bound as the session input with NO H2D copy.

Design of the fused path (per model): raw crop → one H2D of the small uint8 crop (or reuse the already-resident frame) → a single fused CUDA kernel does bilinear resize→S×S + /255 + per-channel normalize + HWC→CHW into a device f32 NCHW buffer → `IoBinding.bind_input` that device buffer → `session.run` → only the tiny logits come back. This deletes `StateClassifier::preprocess` (9.6 %) + the per-crop f32 H2D (part of the 23 % `libcuda`), so the GPU stops starving at 60 %.

FIRST SLICE (highest-ROI, testable in isolation): the **state classifier** (hottest preprocess at 9.6 %). New `vision/gpu_preprocess.rs` (FFI + the fused kernel via `build.rs`/nvcc), a `classify_batch` device path behind IoBinding, and a STANDALONE smoke example that runs CPU-preprocess vs GPU-resident on the same crop, asserts the logits match within fp tolerance (correctness gate), and times both (speedup gate). Only after the smoke gate passes does it wire into `run_cold_stages`; then expand to plate + detector. Key risk to solve first: constructing an `ort::Value` over a raw CUDA device pointer (the crate exposes `MemoryInfo::CUDA` + IoBinding; verify the device-Value / pre-allocated-output route before building the kernel).

## GPU-resident preprocess — first slice BUILT + measured (state classifier)
Implemented `cuda/crop_resize_normalize.cu` (fused bilinear-Q8 resize + /255 + ImageNet normalize + HWC→CHW, bit-parity with `resize_rgb`, `--fmad=false`), `vision/gpu_preprocess.rs` (CUDA FFI), build.rs nvcc integration, and `StateClassifier::classify_batch_gpu` feeding ORT a device tensor via `TensorRefMut::from_raw(MemoryInfo::CUDA, ptr, shape)` — ZERO H2D of the f32 input. Smoke gate `examples/gpu_preprocess_smoke`: **CORRECTNESS PASS (labels == CPU)**, single-thread **5.5× faster** (6.1 → 1.1 ms/batch).
- First naive version regressed under concurrency (250→120 fps) — per-call `cudaMalloc` + device-wide `cudaDeviceSynchronize` serialized every camera. Fixed: **per-thread non-blocking CUDA stream + thread_local grow-only device-buffer pool + `cudaStreamSynchronize`** (no per-call malloc, no device-wide sync). Correctness still PASS.
- **End-to-end (cam_scale --batched, GPU-resident STATE only): ~237 fps, GPU ~60 %** — i.e. on par with the all-CPU batcher, NOT better. Expected: state is 1 of 4 stages; **plate + ADR + detector still CPU-preprocess + H2D**, so the CPU still starves the GPU at 60 %. Removing state's 9.6 % alone can't lift the ceiling.
- **Conclusion: the win needs ALL stages GPU-resident.** The pattern is proven + reusable (kernel + FFI + per-thread stream/pool + `from_raw`); remaining work = apply it to plate OCR (NHWC uint8, no normalize), ADR CRNN (grayscale + orientation), and the detector input, so the whole ~40 % CPU preprocess+transfer disappears and the GPU saturates. Only then does per-GPU camera count rise materially above the ~10 (full-pipeline) / ~250 fps single-GPU plateau.

## Requirement context
Target deployment: **160 cameras**. Scenario is gates/weighbridges — vehicles arrive, sit, leave;
scenes are mostly static. So enrichment is **event-driven** (read a plate/placard a few times with
voting until confident, then hold), NOT 160×25fps continuous reading. Detect (cheap) runs on all
cameras; enrichment load = concurrent *active* vehicles, far below the worst case. The earlier
"no-cache, re-read every frame" is wrong at this scale — the correct model is **read → vote until
confident → hold, re-read on scene change**.
