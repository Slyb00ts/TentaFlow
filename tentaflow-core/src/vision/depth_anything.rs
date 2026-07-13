// =============================================================================
// File: vision/depth_anything.rs — Depth-Anything-V2 Metric (Burn, native)
// =============================================================================
//
// In-process metric depth estimation. Replaces the out-of-process Python `depth`
// service (HTTP + JSON/base64) on the camera→map path: an RGB frame goes straight
// to the vendored Burn model (`burn_depth_anything`, build-time ONNX→Burn codegen)
// and back as a metric depth map — zero IPC, zero serialization.
//
// The architecture is `Depth-Anything-V2-Metric-Indoor-Small` exported at a FIXED
// 518×518 (DINOv2 patch-14 grid, 37×37); weights load at runtime from
// `vision_models_dir()/depth-anything-v2-metric.bpk`. Runs on the `vision-*`
// backend (CUDA/Metal/ROCm native, wgpu/Vulkan fallback). Preprocess mirrors the
// reference image processor: RGB → 518×518 bilinear STRETCH → /255 → per-channel
// ImageNet normalize → NCHW f32 [1,3,518,518]. Output is metric depth (metres) at
// 518×518, row-major.

#![cfg(feature = "inference-vision-gpu")]

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Result};
use burn::tensor::{Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};
use tracing::info;

use crate::paths;
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
use crate::vision::burn_depth_anything::Model;

/// Fixed square input the exported DA-V2 graph expects (37×14 — the DINOv2 patch
/// grid the model was traced at; the vendored codegen's reshape is patched to 37²).
pub const RESOLUTION: u32 = 518;

/// Per-channel ImageNet normalization (matches the DA-V2 image processor).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Loaded DA-V2 metric model + backend device. `infer` keeps `&mut self` so the
/// global engine holds it behind a single mutex (one model, serial GPU access).
pub struct DepthAnything {
    model: Model<VisionBackend>,
    device: VisionDevice,
}

impl DepthAnything {
    /// Build from `vision_models_dir()/depth-anything-v2-metric.bpk`.
    pub fn load() -> Result<Self> {
        let weights = paths::vision_models_dir().join("depth-anything-v2-metric.bpk");
        if !weights.exists() {
            bail!("DA-V2 weights missing: {}", weights.display());
        }
        let device = burn_backend::device();
        let mut model = Model::<VisionBackend>::new(&device);
        let mut store = BurnpackStore::from_file(&weights);
        model
            .load_from(&mut store)
            .map_err(|e| anyhow!("load DA-V2 weights {}: {e}", weights.display()))?;
        info!(
            "[depth_anything] loaded {} (backend {})",
            weights.display(),
            std::any::type_name::<VisionBackend>()
        );
        Ok(Self { model, device })
    }

    /// RGB24 (`w`×`h`) → metric depth map (metres) at `RESOLUTION`×`RESOLUTION`,
    /// row-major. The single forward includes the GPU read-back of the depth plane.
    pub fn infer(&mut self, rgb: &[u8], w: u32, h: u32) -> Result<(Vec<f32>, u32, u32)> {
        Ok(self
            .infer_batch(&[(rgb, w, h)])?
            .into_iter()
            .next()
            .expect("infer_batch(1) yields one map"))
    }

    /// Batched metric depth: `N` RGB frames → `N` depth maps (518×518 metres, row-
    /// major), in input order, via ONE `[N,3,518,518]` forward. The whole point of
    /// batching is a SINGLE GPU launch shared across many robots/cameras instead of
    /// one per source. Empty input → empty output.
    pub fn infer_batch(
        &mut self,
        frames: &[(&[u8], u32, u32)],
    ) -> Result<Vec<(Vec<f32>, u32, u32)>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let n = frames.len();
        let res = RESOLUTION as usize;
        let plane = res * res;
        let mut data = vec![0f32; n * 3 * plane];
        for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
            let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
                .map_err(|e| anyhow!("resize_rgb: {e}"))?;
            let base = bi * 3 * plane;
            for y in 0..res {
                for x in 0..res {
                    let p = (y * res + x) * 3;
                    for c in 0..3 {
                        let v = resized[p + c] as f32 / 255.0;
                        data[base + c * plane + y * res + x] = (v - MEAN[c]) / STD[c];
                    }
                }
            }
        }
        let input = Tensor::<VisionBackend, 4>::from_data(
            TensorData::new(data, [n, 3, res, res]),
            &self.device,
        );
        let out = self.model.forward(input); // [n, 518, 518] metric depth
        let all: Vec<f32> = out
            .to_data()
            .to_vec()
            .map_err(|e| anyhow!("depth to_vec: {e:?}"))?;
        if all.len() != n * plane {
            bail!(
                "depth batch output {} != expected {} ({}×518²)",
                all.len(),
                n * plane,
                n
            );
        }
        Ok((0..n)
            .map(|i| {
                (
                    all[i * plane..(i + 1) * plane].to_vec(),
                    RESOLUTION,
                    RESOLUTION,
                )
            })
            .collect())
    }
}

/// Process-wide singleton: the model is heavy to load (~0.4 s) and autotunes its
/// kernels on the first forward, so it is built once and reused.
fn engine() -> &'static Mutex<Option<DepthAnything>> {
    static ENGINE: OnceLock<Mutex<Option<DepthAnything>>> = OnceLock::new();
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Batched metric depth via the global engine (lazily loaded on first use). One GPU
/// launch for all frames; results in input order, `(depth_metres_row_major, w, h)`.
pub fn infer_global_batch(frames: &[(&[u8], u32, u32)]) -> Result<Vec<(Vec<f32>, u32, u32)>> {
    let mut guard = engine()
        .lock()
        .map_err(|_| anyhow!("depth engine poisoned"))?;
    if guard.is_none() {
        *guard = Some(DepthAnything::load()?);
    }
    guard.as_mut().unwrap().infer_batch(frames)
}

/// Pre-load + autotune the model off the hot path (the first forward compiles GPU
/// kernels, ~20 s on wgpu). Call at startup so the first real frame isn't stalled.
pub fn prewarm() -> Result<()> {
    let mut guard = engine()
        .lock()
        .map_err(|_| anyhow!("depth engine poisoned"))?;
    if guard.is_none() {
        *guard = Some(DepthAnything::load()?);
    }
    let res = RESOLUTION;
    let dummy = vec![0u8; (res * res * 3) as usize];
    guard.as_mut().unwrap().infer(&dummy, res, res)?;
    info!("[depth_anything] prewarm complete (kernels autotuned)");
    Ok(())
}
