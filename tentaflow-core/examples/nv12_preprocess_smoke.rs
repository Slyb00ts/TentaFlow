// =============================================================================
// File: examples/nv12_preprocess_smoke.rs — NV12 GPU-preprocess parity gate
// =============================================================================
//
// Stage-0 foundation check for the GPU-resident RTSP video path. It does NOT
// touch the pipeline/detector — it only proves the fused
// `nv12_to_rgb_resize_normalize` CUDA kernel is numerically faithful to the CPU
// reference it will replace.
//
// For each synthetic frame it:
//   1. builds a smooth RGB image, encodes it to NV12 (4:2:0) with the forward of
//      the SAME BT.709-limited matrix the kernel inverts (2x2 chroma averaging),
//   2. GPU path : `preprocess_nv12_batch_gpu` -> device `[n,3,S,S]` -> host copy,
//   3. CPU ref  : NV12->RGB (same f32 formula) -> `resize_rgb` -> /255+normalize,
//   4. reports max/mean abs diff and PASS/FAIL against a tolerance.
//
// Tolerance rationale: both paths consume the SAME NV12 and share a bit-identical
// Q8 resize + integer normalize, so the ONLY divergence is CPU-vs-GPU f32
// rounding of the YUV->RGB stage (a few u8 LSB before resize). We gate at
// TOL = 0.08 in normalized space ~= 5 u8 LSB / 255 / min(std). Anything beyond a
// few LSB means a real matrix/siting bug that would drift detection thresholds in
// Stage 1 — which is exactly what this gate guards.
//
// Run (needs a CUDA GPU):
//   cargo run -p tentaflow-core --release \
//     --features inference-vision-gpu,inference-supertonic \
//     --example nv12_preprocess_smoke

use std::time::Instant;

use anyhow::{bail, Result};
use tentaflow_core::vision::gpu_preprocess::{preprocess_nv12_batch_gpu, ColorCoeffs, Nv12Frame};
use tentaflow_core::vision::resize::resize_rgb;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
const S: usize = 560;
// Gate ~5 u8 LSB in normalized space; only the f32 YUV->RGB stage can differ.
const TOL: f32 = 0.08;

/// Deterministic smooth RGB gradient so bilinear resampling reads meaningful
/// low-frequency structure (not pure noise).
fn synth_rgb(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut v = vec![0u8; wu * hu * 3];
    for y in 0..hu {
        for x in 0..wu {
            let o = (y * wu + x) * 3;
            v[o] = ((x * 255) / wu) as u8;
            v[o + 1] = ((y * 255) / hu) as u8;
            v[o + 2] = (((x + y).wrapping_add(seed as usize) * 255) / (wu + hu)) as u8;
        }
    }
    v
}

/// YUV->RGB (u8), the exact f32 formula the CUDA kernel uses (BT.601/709 +
/// limited/full via `ColorCoeffs`). Keep in lockstep with the .cu.
fn yuv_to_rgb_u8(yv: i32, u: i32, vv: i32, c: ColorCoeffs) -> [u8; 3] {
    let kg = 1.0f32 - c.kr - c.kb;
    let (y, cb, cr) = if c.full_range {
        (
            yv as f32 / 255.0,
            (u as f32 - 128.0) / 255.0,
            (vv as f32 - 128.0) / 255.0,
        )
    } else {
        (
            (yv as f32 - 16.0) / 219.0,
            (u as f32 - 128.0) / 224.0,
            (vv as f32 - 128.0) / 224.0,
        )
    };
    let r = y + 2.0 * (1.0 - c.kr) * cr;
    let b = y + 2.0 * (1.0 - c.kb) * cb;
    let g = y - (2.0 * c.kr * (1.0 - c.kr) / kg) * cr - (2.0 * c.kb * (1.0 - c.kb) / kg) * cb;
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as i32 as u8;
    [q(r), q(g), q(b)]
}

/// Forward RGB->YUV (float chroma) for the SAME matrix, used only to synthesize
/// a valid NV12 test frame. Returns (Y sample u8, cb float, cr float).
fn rgb_to_ycc(r: u8, g: u8, b: u8, c: ColorCoeffs) -> (u8, f32, f32) {
    let kg = 1.0f32 - c.kr - c.kb;
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let yl = c.kr * rf + kg * gf + c.kb * bf;
    let cb = (bf - yl) / (2.0 * (1.0 - c.kb));
    let cr = (rf - yl) / (2.0 * (1.0 - c.kr));
    let yv = if c.full_range {
        yl * 255.0
    } else {
        yl * 219.0 + 16.0
    };
    ((yv + 0.5).clamp(0.0, 255.0) as u8, cb, cr)
}

/// Encodes an RGB24 image to packed NV12 (y_stride = w, uv_stride = w). Chroma is
/// the 2x2-block average, quantized to the range the kernel decodes.
fn rgb_to_nv12(rgb: &[u8], w: usize, h: usize, c: ColorCoeffs) -> (Vec<u8>, Vec<u8>) {
    let mut yp = vec![0u8; w * h];
    let mut uv = vec![128u8; w * (h / 2)];
    // Y per pixel.
    let mut cbcr = vec![(0.0f32, 0.0f32); w * h];
    for i in 0..w * h {
        let (r, g, b) = (rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]);
        let (yv, cb, cr) = rgb_to_ycc(r, g, b, c);
        yp[i] = yv;
        cbcr[i] = (cb, cr);
    }
    // Chroma: average the 2x2 block, then quantize (interleaved U,V).
    let quant = |v: f32| -> u8 {
        let s = if c.full_range { 255.0 } else { 224.0 };
        (v * s + 128.0 + 0.5).clamp(0.0, 255.0) as u8
    };
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let mut sb = 0.0f32;
            let mut sr = 0.0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (cb, cr) = cbcr[(by * 2 + dy) * w + (bx * 2 + dx)];
                    sb += cb;
                    sr += cr;
                }
            }
            let uidx = by * w + bx * 2;
            uv[uidx] = quant(sb / 4.0);
            uv[uidx + 1] = quant(sr / 4.0);
        }
    }
    (yp, uv)
}

/// CPU reference: decode NV12 -> full-frame RGB24 (nearest 2x2 chroma, matching
/// the kernel's siting).
fn nv12_to_rgb(yp: &[u8], uv: &[u8], w: usize, h: usize, c: ColorCoeffs) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let yv = yp[y * w + x] as i32;
            let (cx, cy) = (x >> 1, y >> 1);
            let uidx = cy * w + cx * 2;
            let u = uv[uidx] as i32;
            let v = uv[uidx + 1] as i32;
            let px = yuv_to_rgb_u8(yv, u, v, c);
            let o = (y * w + x) * 3;
            rgb[o] = px[0];
            rgb[o + 1] = px[1];
            rgb[o + 2] = px[2];
        }
    }
    rgb
}

/// CPU reference NCHW `[3,S,S]`: NV12->RGB -> resize_rgb -> /255 + normalize.
fn cpu_reference(yp: &[u8], uv: &[u8], w: usize, h: usize, c: ColorCoeffs) -> Vec<f32> {
    let rgb = nv12_to_rgb(yp, uv, w, h, c);
    let resized = resize_rgb(&rgb, w as u32, h as u32, S as u32, S as u32).expect("resize");
    let mut out = vec![0f32; 3 * S * S];
    for y in 0..S {
        for x in 0..S {
            let o = (y * S + x) * 3;
            for ch in 0..3 {
                let fv = resized[o + ch] as f32 / 255.0;
                out[ch * S * S + y * S + x] = (fv - MEAN[ch]) / STD[ch];
            }
        }
    }
    out
}

fn main() -> Result<()> {
    let color = ColorCoeffs::bt709_limited();
    // Even dims (NV12 requires 2x2 chroma blocks): upscale + downscale to S.
    let sizes: [(u32, u32); 3] = [(640, 480), (1280, 720), (480, 480)];

    // Build NV12 planes + keep them alive for the whole batch call.
    let planes: Vec<(Vec<u8>, Vec<u8>, u32, u32)> = sizes
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| {
            let rgb = synth_rgb(w, h, i as u64 * 13 + 1);
            let (yp, uv) = rgb_to_nv12(&rgb, w as usize, h as usize, color);
            (yp, uv, w, h)
        })
        .collect();

    let frames: Vec<Nv12Frame> = planes
        .iter()
        .map(|(yp, uv, w, h)| Nv12Frame {
            y: yp.as_slice(),
            y_stride: *w as usize,
            uv: uv.as_slice(),
            uv_stride: *w as usize,
            w: *w,
            h: *h,
        })
        .collect();

    // Warm + timed GPU run.
    for _ in 0..3 {
        let _ = preprocess_nv12_batch_gpu(&frames, S, MEAN, STD, color)?;
    }
    let iters = 50u32;
    let t = Instant::now();
    let mut batch = preprocess_nv12_batch_gpu(&frames, S, MEAN, STD, color)?;
    for _ in 1..iters {
        batch = preprocess_nv12_batch_gpu(&frames, S, MEAN, STD, color)?;
    }
    let gpu_ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;
    let gpu_host = batch.copy_to_host()?;

    // Compare each frame's plane against the CPU reference.
    let mut max_abs = 0.0f32;
    let mut sum_abs = 0.0f64;
    let mut count = 0u64;
    for (i, (yp, uv, w, h)) in planes.iter().enumerate() {
        let cpu = cpu_reference(yp, uv, *w as usize, *h as usize, color);
        let base = i * 3 * S * S;
        for k in 0..3 * S * S {
            let d = (gpu_host[base + k] - cpu[k]).abs();
            if d > max_abs {
                max_abs = d;
            }
            sum_abs += d as f64;
            count += 1;
        }
    }
    let mean_abs = sum_abs / count as f64;

    println!("frames            : {}", sizes.len());
    println!("GPU-resident      : {gpu_ms:.3} ms/batch");
    println!("max abs diff      : {max_abs:.6} (normalized units)");
    println!("mean abs diff     : {mean_abs:.6}");
    println!("tolerance         : {TOL:.6}");

    if max_abs <= TOL {
        println!("PARITY: PASS (max {max_abs:.6} <= tol {TOL:.6})");
        Ok(())
    } else {
        println!("PARITY: FAIL (max {max_abs:.6} > tol {TOL:.6})");
        bail!("nv12 preprocess parity exceeded tolerance");
    }
}
