# GPU-Resident RTSP Video Path — staged execution checklist

Goal: a 4K RTSP frame never touches the CPU for decode/convert/resize on the hot
analysis path. Real bottleneck (measured on the live 4K camera): after NVDEC decode
(deployed, `IngestPath::NvdecCpuConvert`), the pipeline does `cudadownload → videoconvert`
NV12→RGB of the FULL 4K frame every frame — single-threaded ~90% of a core PER camera.
See [[rtsp-4k-cpu-decode-bottleneck]].

## Key facts (verified)
- Recordings are ALREADY off the RGB path (Branch B muxes RTP H.264 directly). No work.
- `streaming_bus().broadcast` early-returns when the camera has no subscribers — so full
  RGB is only CONSUMED when watched, but PRODUCED every frame. Unwatched cameras (the common
  case at 160 scale) waste the whole convert. **Stage 3 removes it.**
- Frame type is `Arc<[u8]>` RGB everywhere; add a `FramePixelFormat` tag (Rgb24 | Nv12), keep
  `Arc<[u8]>`; only the 2 hot consumers (detect, enrich crops) learn NV12. Additive, back-compat.
- Zero-copy device pointer IS feasible (`gst_memory_map` + `GST_MAP_CUDA=0x20000`, device-0
  primary context shared with ORT) but fragile (magic flag, context pinning, decode-stream
  sync, GstBuffer lifetime vs finite NVDEC surface pool) → NV12-download path first; zero-copy
  is optional Stage 4. gstreamer-rs 0.25 has NO CUDA bindings (raw FFI needed for Stage 4).
- GPU preprocess infra exists: `gpu_preprocess.rs` (CUDA FFI, thread-local stream + buffer pool,
  DeviceBatch, `TensorRefMut::from_raw(MemoryInfo::CUDA,...)`), `crop_resize_normalize.cu`
  (Q8 bilinear + normalize, bit-parity with `resize_rgb`), `classifier_stan::classify_batch_gpu`.

## Stages (each: build → deploy → measure CPU%/camera on the live 4K camera)
- **Stage 0 — NV12 kernel + FFI + parity test** (no pipeline change). `cuda/nv12_to_rgb_resize_normalize.cu`
  (fused YUV→RGB [BT.709 limited default, coeffs from caps] + Q8 bilinear resize reusing the
  existing helpers + normalize → `[n,3,S,S]`), `gpu_preprocess.rs::preprocess_nv12_batch_gpu`,
  `build.rs` nvcc. Gate = parity vs `videoconvert(NV12→RGB)+resize_rgb+normalize` within tolerance.
- **Stage 1 — detect from GPU NV12**: `detector_rfdetr::detect_batch_gpu` (device tensor, mirror
  classify_batch_gpu), rtsp `NvdecNv12` variant (`cudadownload → NV12 appsink`), detect slot uses
  the NV12→560 device tensor. Kills the detector's CPU resize; full videoconvert still runs for
  crops/display this stage.
- **Stage 2 — enrichment crops from NV12**: `crop_rgb` NV12-aware sibling, `classify_batch_local`/
  `read_batch_local` accept NV12 crops (even x/y offsets on subsampled UV). Removes the last
  per-frame full-RGB consumer.
- **Stage 3 — display/record split (HEADLINE WIN)**: remove full `videoconvert` from the NvdecNv12
  tail; add an on-demand RGB branch (modeled on `attach_mp4_branch`) gated by
  `streaming_bus().list_subscribers` + recording/preview state; store NV12 in the snapshot LRU,
  convert lazily in `snapshot()`. Unwatched cameras → zero full convert.
- **Stage 4 — zero-copy (optional, feature-gated)**: `gst_cuda_ffi.rs` (map GstCudaMemory,
  `cuPointerGetAttribute` validation, decode-stream sync), device DetectFrame variant + in-flight
  cap, force `cuda-device-id=0`. Removes the NV12 H2D upload (bandwidth, not CPU) — only if needed.

## Cross-cutting risks
1. GStreamer CUDA context vs ORT CUDA context — device 0 identity (absent for download Stages 1-3;
   critical for Stage 4).
2. Colorimetry / NV12→RGB matrix parity (BT.601 vs 709, limited vs full) from caps — parity tests gate.
3. NV12 chroma 2×2 subsample: even crop offsets, half-res UV sampling.
4. Frame lifetime: Stages 1-3 keep owned Arc<[u8]> NV12 (no lifetime change); Stage 4 borrows
   GstBuffer (ref-held + in-flight cap).
5. Fallback cascade `GpuResidentNvidia → NvdecNv12 → NvdecCpuConvert → Cpu`; non-NVDEC paths keep
   emitting Rgb24 (format tag is the single switch). Bench: add an NVDEC-NV12 mode to `pipeline_bench`
   so the real path stays measured. See [[bench-real-pipeline-not-reimpl]].
