// ===== File: golden.rs — GPU kernel outputs vs CPU references (forge-formats) =====
// Every kernel artifact must reproduce the CPU reference math within f16
// tolerance. Skips cleanly when no CUDA device is present; on this machine
// (RTX 4090) the tests are expected to run.

use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_types::MemKind;
use forge_kernels::Kernels;
use forge_types::{DType, QuantKind};
use half::f16;

const EPS: f32 = 1e-6;

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 64 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping CUDA golden tests: {e}");
            None
        }
    }
}

fn fill(i: usize) -> f32 {
    ((i * 37 % 19) as f32 - 9.0) * 0.25
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes: &[u8] = bytemuck_cast(&host);
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

fn bytemuck_cast(host: &[f16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) }
}

fn max_abs_err(got: &[f32], want: &[f32]) -> f32 {
    got.iter()
        .zip(want)
        .map(|(g, w)| (g - w).abs())
        .fold(0.0, f32::max)
}

// f16 rounding of inputs is applied to the reference too, so tolerances only
// cover accumulation-order differences.
#[test]
fn rmsnorm_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (4usize, 1024usize);
    let x: Vec<f32> = (0..rows * cols)
        .map(|i| f16::from_f32(fill(i)).to_f32())
        .collect();
    let w: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(1.0 + (i % 5) as f32 * 0.1).to_f32())
        .collect();

    let xb = upload_f16(dev.as_ref(), &x);
    let wb = upload_f16(dev.as_ref(), &w);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows * cols]);
    kernels
        .rmsnorm_f16(&yb, &xb, &wb, rows, cols, EPS, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows * cols);
    let mut want = vec![0.0f32; rows * cols];
    for r in 0..rows {
        let row = &x[r * cols..(r + 1) * cols];
        let ss: f32 = row.iter().map(|v| v * v).sum::<f32>() / cols as f32;
        let inv = 1.0 / (ss + EPS).sqrt();
        for c in 0..cols {
            want[r * cols + c] = row[c] * inv * w[c];
        }
    }
    assert!(max_abs_err(&got, &want) < 0.01);
}

#[test]
fn gemv_q8_0_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (32usize, 512usize);
    let blocks_per_row = cols / 32;
    // Hand-built Q8_0 stream: f16 scale + 32 i8 per block.
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 34);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let scale = f16::from_f32(0.015 + ((r + b) % 7) as f32 * 0.005);
            wq.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                wq.push((((r * 31 + b * 17 + k * 13) % 255) as i32 - 127) as i8 as u8);
            }
        }
    }
    // The CPU dequant from forge-formats is the golden source of truth.
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q8_0, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemv_q8_0_f16(&yb, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got[r]);
    }
}

/// Deterministic Q4_K stream (144-byte superblocks) exercising both
/// get_scale_min_k4 branches including the high 2 bits of every scale byte.
fn build_q4k(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 256;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 144);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let d = f16::from_f32(0.008 + ((r + b) % 7) as f32 * 0.004);
            let dmin = f16::from_f32(0.005 + ((r + 2 * b) % 5) as f32 * 0.003);
            wq.extend_from_slice(&d.to_le_bytes());
            wq.extend_from_slice(&dmin.to_le_bytes());
            for i in 0..12 {
                wq.push(((r * 53 + b * 19 + i * 41 + 7) % 256) as u8);
            }
            for i in 0..128 {
                wq.push(((r * 31 + b * 17 + i * 13) % 256) as u8);
            }
        }
    }
    wq
}

#[test]
fn gemv_q4_k_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (33usize, 512usize);
    let wq = build_q4k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q4K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemv_q4_k_f16(&yb, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got[r]);
    }

    // f32-logit variant over the same data.
    let y32 = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    kernels
        .gemv_q4_k_out_f32(&y32, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let mut bytes = vec![0u8; rows * 4];
    dev.read(&y32, 0, &mut bytes).unwrap();
    let got32: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got32[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got32[r]);
    }
}

#[test]
fn gemm_q4_k_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // Row/token tails on purpose (rows < 64, tokens not a tile multiple).
    let (rows, cols, n_tokens) = (24usize, 512usize, 40usize);
    let wq = build_q4k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q4K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..n_tokens * cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemm_q4_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
            assert!(rel < 0.02, "t {t} row {r}: got {} want {want}", got[t * rows + r]);
        }
    }
}

/// Deterministic Q6_K stream (210-byte superblocks: ql[128], qh[64],
/// 16 int8 scales, d f16) exercising every qh shift and both nibble halves.
fn build_q6k(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 256;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 210);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            for i in 0..208 {
                wq.push(((r * 37 + b * 23 + i * 11 + 5) % 256) as u8);
            }
            let d = f16::from_f32(0.006 + ((r + b) % 7) as f32 * 0.003);
            wq.extend_from_slice(&d.to_le_bytes());
        }
    }
    wq
}

#[test]
fn gemv_q6_k_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (33usize, 512usize);
    let wq = build_q6k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q6K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemv_q6_k_f16(&yb, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got[r]);
    }

    // f32-logit variant over the same data.
    let y32 = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    kernels
        .gemv_q6_k_out_f32(&y32, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let mut bytes = vec![0u8; rows * 4];
    dev.read(&y32, 0, &mut bytes).unwrap();
    let got32: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got32[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got32[r]);
    }
}

// Prefill-sized batch (n_tokens >= 64) routes Q6_K through the vendored llama.cpp
// MMQ path (q8_1 D4 activation + native codes, f16-direct write-back). Compares
// against the CPU formats dequant of the SAME bytes; tolerance covers the q8_1
// activation quant. rows a multiple of 128 hits the _nc entry.
#[test]
fn gemm_q6_k_mmq_prefill_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols, n_tokens) = (256usize, 512usize, 128usize);
    let wq = build_q6k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q6K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..n_tokens * cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemm_q6_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    // q8_1 activation quant is per-element lossy; use the aggregate relative-L2
    // metric (same as scratch/mmq_probe/q6k_harness.cu) rather than a per-element
    // bound. Weight quant cancels (same bytes both sides).
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let diff = (got[t * rows + r] - want) as f64;
            num += diff * diff;
            den += (want as f64) * (want as f64);
        }
    }
    let rel_l2 = (num / den).sqrt();
    assert!(rel_l2 < 5e-3, "Q6_K MMQ relL2 {rel_l2:.3e} exceeds q8_1 tolerance");
}

#[test]
fn gemm_q6_k_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // Row/token tails on purpose (rows < 64, tokens not a tile multiple).
    let (rows, cols, n_tokens) = (24usize, 512usize, 40usize);
    let wq = build_q6k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q6K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..n_tokens * cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemm_q6_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
            assert!(rel < 0.02, "t {t} row {r}: got {} want {want}", got[t * rows + r]);
        }
    }
}

// Prefill-sized batch on a COMMITTED native (N,K,MPAD) shape (rows=1024,
// cols=4096, n_tokens=128 → gemm_q6k_i8_native_1024_4096_m128) routes Q6_K
// through the native-GGUF-layout int8 multistage GEMM. Compares against the CPU
// formats dequant of the SAME bytes with the aggregate relative-L2 metric (the
// q8_1 activation quant is per-element lossy; the double-mma per-16-scale flush is
// otherwise bit-exact).
#[test]
fn gemm_q6_k_native_prefill_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols, n_tokens) = (1024usize, 4096usize, 128usize);
    let wq = build_q6k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q6K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..n_tokens * cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemm_q6_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let diff = (got[t * rows + r] - want) as f64;
            num += diff * diff;
            den += (want as f64) * (want as f64);
        }
    }
    let rel_l2 = (num / den).sqrt();
    assert!(rel_l2 < 5e-3, "Q6_K native relL2 {rel_l2:.3e} exceeds q8_1 tolerance");
}

#[test]
fn gemv_nvfp4_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (24usize, 256usize);
    let global_scale = 12.5f32;
    // Deterministic packed nibbles + FP8 scales covering positive/negative
    // codes and a range of exponents.
    let packed: Vec<u8> = (0..rows * cols / 2)
        .map(|i| ((i * 41 + 7) % 256) as u8)
        .collect();
    let scales: Vec<u8> = (0..rows * cols / 16)
        .map(|i| (((i * 29 + 3) % 96) + 16) as u8) // exponents 2..13, positive
        .collect();

    let w_f32 = forge_formats::nvfp4::dequantize_nvfp4(
        &packed,
        &scales,
        global_scale,
        rows,
        cols,
        16,
    )
    .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let pb = dev
        .alloc(packed.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&packed, &pb, 0).unwrap();
    let sb = dev
        .alloc(scales.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&scales, &sb, 0).unwrap();

    kernels
        .gemv_nvfp4_f16(&yb, &pb, &sb, &xb, rows, cols, 1.0 / global_scale, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "row {r}: got {} want {want}", got[r]);
    }
}

#[test]
fn batched_nvfp4_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (24usize, 256usize);
    let global_scale = 12.5f32;
    let packed: Vec<u8> = (0..rows * cols / 2)
        .map(|i| ((i * 41 + 7) % 256) as u8)
        .collect();
    let scales: Vec<u8> = (0..rows * cols / 16)
        .map(|i| (((i * 29 + 3) % 96) + 16) as u8)
        .collect();
    let weights = forge_formats::nvfp4::dequantize_nvfp4(
        &packed,
        &scales,
        global_scale,
        rows,
        cols,
        16,
    )
    .unwrap();
    let packed_buf = dev
        .alloc(packed.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    let scales_buf = dev
        .alloc(scales.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&packed, &packed_buf, 0).unwrap();
    dev.write(&scales, &scales_buf, 0).unwrap();

    // Rozmiary obejmują B1, pełne buckety i ich niepełne warianty.
    for batch in [1usize, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
        let x: Vec<f32> = (0..batch * cols)
            .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
            .collect();
        let x_buf = upload_f16(dev.as_ref(), &x);
        let y_buf = upload_f16(dev.as_ref(), &vec![0.0; batch * rows]);
        kernels
            .gemm_nvfp4_f16(
                &y_buf,
                &packed_buf,
                &scales_buf,
                &x_buf,
                rows,
                cols,
                batch,
                1.0 / global_scale,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        let got = download_f16(dev.as_ref(), &y_buf, batch * rows);
        for token in 0..batch {
            for row in 0..rows {
                let want: f32 = (0..cols)
                    .map(|col| weights[row * cols + col] * x[token * cols + col])
                    .sum();
                let rel = (got[token * rows + row] - want).abs() / (want.abs() + 1.0);
                assert!(
                    rel < 0.02,
                    "batch {batch}, token {token}, row {row}: got {} want {want}",
                    got[token * rows + row]
                );
            }
        }
    }
    let x_buf = upload_f16(dev.as_ref(), &vec![0.0; cols]);
    let y_buf = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    assert!(kernels
        .gemm_nvfp4_f16(
            &y_buf,
            &packed_buf,
            &scales_buf,
            &x_buf,
            rows,
            cols,
            0,
            1.0 / global_scale,
            &stream,
        )
        .is_err());
    assert!(kernels
        .gemm_nvfp4_f16(
            &y_buf,
            &packed_buf,
            &scales_buf,
            &x_buf,
            rows,
            0,
            1,
            1.0 / global_scale,
            &stream,
        )
        .is_err());
    assert!(kernels
        .gemm_nvfp4_f16(
            &y_buf,
            &packed_buf,
            &scales_buf,
            &x_buf,
            rows,
            8,
            1,
            1.0 / global_scale,
            &stream,
        )
        .is_err());
}

#[test]
fn batched_f16_logits_match_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (24usize, 256usize);
    let weights: Vec<f32> = (0..rows * cols)
        .map(|i| f16::from_f32(fill(i) * 0.05).to_f32())
        .collect();
    let weights_buf = upload_f16(dev.as_ref(), &weights);

    // Rozmiary obejmują B1 oraz pełne i niepełne buckety B4/B8.
    for batch in [1usize, 2, 3, 4, 5, 7, 8, 17, 31, 33] {
        let x: Vec<f32> = (0..batch * cols)
            .map(|i| f16::from_f32(fill(i + 11) * 0.1).to_f32())
            .collect();
        let x_buf = upload_f16(dev.as_ref(), &x);
        let y_buf = dev
            .alloc(batch * rows * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        kernels
            .gemm_f16_out_f32_at(
                &y_buf,
                &weights_buf,
                0,
                &x_buf,
                rows,
                cols,
                batch,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let mut bytes = vec![0u8; batch * rows * 4];
        dev.read(&y_buf, 0, &mut bytes).unwrap();
        let got: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for token in 0..batch {
            for row in 0..rows {
                let want: f32 = (0..cols)
                    .map(|col| weights[row * cols + col] * x[token * cols + col])
                    .sum();
                let rel = (got[token * rows + row] - want).abs() / (want.abs() + 1.0);
                assert!(
                    rel < 1e-4,
                    "batch {batch}, token {token}, row {row}: got {} want {want}",
                    got[token * rows + row]
                );
            }
        }
    }
    let x_buf = upload_f16(dev.as_ref(), &vec![0.0; cols]);
    let y_buf = dev
        .alloc(rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    assert!(kernels
        .gemm_f16_out_f32_at(
            &y_buf,
            &weights_buf,
            0,
            &x_buf,
            rows,
            cols,
            0,
            &stream,
        )
        .is_err());
    assert!(kernels
        .gemm_f16_out_f32_at(
            &y_buf,
            &weights_buf,
            0,
            &x_buf,
            rows,
            4,
            1,
            &stream,
        )
        .is_err());
}

#[test]
fn attn_decode_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (n_seqs, n_q_heads, n_kv_heads, head_dim) = (2usize, 4usize, 2usize, 128usize);
    let (page_size, max_pages, n_pages) = (16usize, 4usize, 4usize);
    let seq_lens = [5i32, 23i32];
    let scale = 1.0 / (head_dim as f32).sqrt();

    // seq0 → page 3; seq1 → pages 1,0 (scattered on purpose).
    let mut pt = vec![-1i32; n_seqs * max_pages];
    pt[0] = 3;
    pt[max_pages] = 1;
    pt[max_pages + 1] = 0;
    let page_of = |s: usize, pos: usize| -> usize {
        if s == 0 {
            3
        } else if pos < page_size {
            1
        } else {
            0
        }
    };

    let q: Vec<f32> = (0..n_seqs * n_q_heads * head_dim)
        .map(|i| f16::from_f32(fill(i) * 0.2).to_f32())
        .collect();
    let kv_elems = n_pages * n_kv_heads * page_size * head_dim;
    let kc: Vec<f32> = (0..kv_elems)
        .map(|i| f16::from_f32(fill(i + 3) * 0.2).to_f32())
        .collect();
    let vc: Vec<f32> = (0..kv_elems)
        .map(|i| f16::from_f32(fill(i + 11) * 0.2).to_f32())
        .collect();

    let qb = upload_f16(dev.as_ref(), &q);
    let ob = upload_f16(dev.as_ref(), &vec![0.0; n_seqs * n_q_heads * head_dim]);
    let kb = upload_f16(dev.as_ref(), &kc);
    let vb = upload_f16(dev.as_ref(), &vc);
    let ptb = dev
        .alloc(pt.len() * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytemuck::cast_slice(&pt), &ptb, 0).unwrap();
    let slb = dev.alloc(8, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&seq_lens), &slb, 0).unwrap();

    kernels
        .attn_decode_f16(
            &ob, &qb, &kb, &vb, &ptb, &slb, n_seqs, n_q_heads, n_kv_heads, head_dim, page_size,
            max_pages, scale, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &ob, n_seqs * n_q_heads * head_dim);
    for (s, &sl) in seq_lens.iter().enumerate() {
        let ctx_len = sl as usize;
        for h in 0..n_q_heads {
            let kvh = h / (n_q_heads / n_kv_heads);
            let qb_off = (s * n_q_heads + h) * head_dim;
            let mut scores = Vec::with_capacity(ctx_len);
            for pos in 0..ctx_len {
                let page = page_of(s, pos);
                let kv_off = ((page * n_kv_heads + kvh) * page_size + pos % page_size) * head_dim;
                let dot: f32 = (0..head_dim)
                    .map(|e| q[qb_off + e] * kc[kv_off + e])
                    .sum();
                scores.push(dot * scale);
            }
            let m = scores.iter().cloned().fold(f32::MIN, f32::max);
            let denom: f32 = scores.iter().map(|s| (s - m).exp()).sum();
            for e in 0..head_dim {
                let num: f32 = (0..ctx_len)
                    .map(|pos| {
                        let page = page_of(s, pos);
                        let kv_off =
                            ((page * n_kv_heads + kvh) * page_size + pos % page_size) * head_dim;
                        (scores[pos] - m).exp() * vc[kv_off + e]
                    })
                    .sum();
                let want = num / denom;
                let g = got[qb_off + e];
                assert!(
                    (g - want).abs() < 0.01,
                    "seq {s} head {h} elem {e}: got {g} want {want}"
                );
            }
        }
    }
}

#[test]
fn gqa_decode_capability_odpowiada_zaladowanym_artefaktom() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();

    assert_eq!(
        kernels.supports_attn_decode_gqa4_f16_hd128(),
        dev.caps().fp8_native,
        "wbudowany split GQA sm_89 ma być dostępny wyłącznie tam, gdzie loader go załadował"
    );
}

#[test]
fn gqa_decode_odrzuca_zbyt_male_bufory_i_overflow() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let tiny = dev
        .alloc(4, MemKind::Device, Pool::Activations)
        .unwrap();

    assert!(kernels
        .attn_decode_split_gqa4_f16_hd128(
            &tiny,
            &tiny,
            0,
            &tiny,
            0,
            &tiny,
            0,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            1,
            4,
            1,
            1,
            1,
            1,
            1e-5,
            10_000.0,
            0.125,
            &stream,
        )
        .is_err());
    assert!(kernels
        .attn_decode_combine_gqa2_f16_hd128(
            &tiny,
            &tiny,
            usize::MAX,
            4,
            1,
            &stream,
        )
        .is_err());
}

// The dp4a GEMVs quantize activations to q8_1 (int8) before the dot, so
// their tolerance vs the CPU dequant reference is wider than the f16-x
// kernels': it covers the documented activation-quantization rounding, not
// just accumulation order.
#[test]
fn gemv_q8_0_dp4a_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (33usize, 512usize);
    let blocks_per_row = cols / 32;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * 34);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let scale = f16::from_f32(0.015 + ((r + b) % 7) as f32 * 0.005);
            wq.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                wq.push((((r * 31 + b * 17 + k * 13) % 255) as i32 - 127) as i8 as u8);
            }
        }
    }
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q8_0, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemv_q8_0_dp4a_f16(&yb, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.03, "row {r}: got {} want {want}", got[r]);
    }
}

#[test]
fn gemv_q4_k_dp4a_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (33usize, 512usize);
    let wq = build_q4k(rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, QuantKind::Q4K, &wq, rows * cols)
            .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    kernels
        .gemv_q4_k_dp4a_f16(&yb, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.03, "row {r}: got {} want {want}", got[r]);
    }

    // f32-logit variant over the same data must round to the f16 output.
    let y32 = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    kernels
        .gemv_q4_k_dp4a_out_f32(&y32, &wb, &xb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let mut bytes = vec![0u8; rows * 4];
    dev.read(&y32, 0, &mut bytes).unwrap();
    let got32: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for r in 0..rows {
        assert_eq!(
            f16::from_f32(got32[r]).to_f32(),
            got[r],
            "row {r}: f32 and f16 dp4a outputs diverge"
        );
    }
}

/// Deterministic quant streams for the extended formats: block headers get
/// plausible small scales, everything else pseudo-random bytes — every scale
/// branch / high-bit path gets exercised.
fn build_quant(quant: QuantKind, rows: usize, cols: usize) -> Vec<u8> {
    let bb = quant.block_bytes();
    let be = quant.block_elems();
    let blocks_per_row = cols / be;
    let mut wq = Vec::with_capacity(rows * blocks_per_row * bb);
    for r in 0..rows {
        for b in 0..blocks_per_row {
            let d = f16::from_f32(0.01 + ((r + b) % 7) as f32 * 0.005);
            let dmin = f16::from_f32(0.006 + ((r + 2 * b) % 5) as f32 * 0.004);
            let mut block: Vec<u8> = (0..bb)
                .map(|i| ((r * 31 + b * 17 + i * 13 + 5) % 256) as u8)
                .collect();
            // Overwrite the f16 fields with sane magnitudes so the reference
            // dot products stay in comparable range.
            match quant {
                QuantKind::Q5K | QuantKind::Q2K => {
                    let (doff, moff) = if quant == QuantKind::Q5K { (0, 2) } else { (80, 82) };
                    block[doff..doff + 2].copy_from_slice(&d.to_le_bytes());
                    block[moff..moff + 2].copy_from_slice(&dmin.to_le_bytes());
                }
                QuantKind::Q3K => block[108..110].copy_from_slice(&d.to_le_bytes()),
                QuantKind::Q4_0
                | QuantKind::Q5_0
                | QuantKind::IQ4NL
                | QuantKind::IQ4XS
                | QuantKind::IQ2XS
                | QuantKind::IQ2S
                | QuantKind::IQ3S
                | QuantKind::IQ2XXS
                | QuantKind::IQ3XXS
                | QuantKind::IQ1S => {
                    block[0..2].copy_from_slice(&d.to_le_bytes())
                }
                // E8M0 scale byte: keep exponents small (2^-9 .. 2^-3).
                QuantKind::MXFP4 => block[0] = 118 + ((r + b) % 7) as u8,
                // IQ1_M packs d in the top nibbles of the 4 scale words;
                // force them to reassemble a small sane f16 (0x2400).
                QuantKind::IQ1M => {
                    block[49] &= 0x0F;
                    block[51] &= 0x0F;
                    block[53] = (block[53] & 0x0F) | 0x40;
                    block[55] = (block[55] & 0x0F) | 0x20;
                }
                QuantKind::Q4_1 | QuantKind::Q5_1 => {
                    block[0..2].copy_from_slice(&d.to_le_bytes());
                    block[2..4].copy_from_slice(&dmin.to_le_bytes());
                }
                _ => unreachable!("unexpected format in build_quant"),
            }
            wq.extend_from_slice(&block);
        }
    }
    wq
}

type GemvFn = fn(
    &Kernels,
    &DevBuffer,
    &DevBuffer,
    &DevBuffer,
    usize,
    usize,
    &forge_hal::Stream,
) -> forge_types::Result<()>;

type GemmFn = fn(
    &Kernels,
    &DevBuffer,
    &DevBuffer,
    &DevBuffer,
    usize,
    usize,
    usize,
    &forge_hal::Stream,
) -> forge_types::Result<()>;

fn gemv_case(quant: QuantKind, gemv: GemvFn, out32: GemvFn) {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (33usize, 512usize);
    let wq = build_quant(quant, rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, quant, &wq, rows * cols).unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    gemv(&kernels, &yb, &wb, &xb, rows, cols, &stream).unwrap();
    dev.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "{quant:?} row {r}: got {} want {want}", got[r]);
    }

    let y32 = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    out32(&kernels, &y32, &wb, &xb, rows, cols, &stream).unwrap();
    dev.synchronize().unwrap();
    let mut bytes = vec![0u8; rows * 4];
    dev.read(&y32, 0, &mut bytes).unwrap();
    let got32: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got32[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "{quant:?} f32 row {r}: got {} want {want}", got32[r]);
    }
}

fn gemm_case(quant: QuantKind, gemm: GemmFn) {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // Row/token tails on purpose (rows < 64, tokens not a tile multiple).
    let (rows, cols, n_tokens) = (24usize, 512usize, 40usize);
    let wq = build_quant(quant, rows, cols);
    let w_f32 =
        forge_formats::dequant::dequantize_to_f32(DType::F32, quant, &wq, rows * cols).unwrap();

    let x: Vec<f32> = (0..n_tokens * cols)
        .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
        .collect();
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
    let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(&wq, &wb, 0).unwrap();

    gemm(&kernels, &yb, &wb, &xb, rows, cols, n_tokens, &stream).unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
            assert!(
                rel < 0.02,
                "{quant:?} t {t} row {r}: got {} want {want}",
                got[t * rows + r]
            );
        }
    }
}

macro_rules! quant_golden {
    ($gemv_test:ident, $gemm_test:ident, $quant:expr, $gemv:ident, $out32:ident, $gemm:ident) => {
        #[test]
        fn $gemv_test() {
            gemv_case($quant, Kernels::$gemv, Kernels::$out32);
        }

        #[test]
        fn $gemm_test() {
            gemm_case($quant, Kernels::$gemm);
        }
    };
}

quant_golden!(
    gemv_q5_k_matches_formats_dequant,
    gemm_q5_k_matches_formats_dequant,
    QuantKind::Q5K,
    gemv_q5_k_f16,
    gemv_q5_k_out_f32,
    gemm_q5_k_f16
);
quant_golden!(
    gemv_q3_k_matches_formats_dequant,
    gemm_q3_k_matches_formats_dequant,
    QuantKind::Q3K,
    gemv_q3_k_f16,
    gemv_q3_k_out_f32,
    gemm_q3_k_f16
);
quant_golden!(
    gemv_q2_k_matches_formats_dequant,
    gemm_q2_k_matches_formats_dequant,
    QuantKind::Q2K,
    gemv_q2_k_f16,
    gemv_q2_k_out_f32,
    gemm_q2_k_f16
);
quant_golden!(
    gemv_q4_0_matches_formats_dequant,
    gemm_q4_0_matches_formats_dequant,
    QuantKind::Q4_0,
    gemv_q4_0_f16,
    gemv_q4_0_out_f32,
    gemm_q4_0_f16
);
quant_golden!(
    gemv_q4_1_matches_formats_dequant,
    gemm_q4_1_matches_formats_dequant,
    QuantKind::Q4_1,
    gemv_q4_1_f16,
    gemv_q4_1_out_f32,
    gemm_q4_1_f16
);
quant_golden!(
    gemv_q5_0_matches_formats_dequant,
    gemm_q5_0_matches_formats_dequant,
    QuantKind::Q5_0,
    gemv_q5_0_f16,
    gemv_q5_0_out_f32,
    gemm_q5_0_f16
);
quant_golden!(
    gemv_q5_1_matches_formats_dequant,
    gemm_q5_1_matches_formats_dequant,
    QuantKind::Q5_1,
    gemv_q5_1_f16,
    gemv_q5_1_out_f32,
    gemm_q5_1_f16
);

quant_golden!(
    gemv_iq4_nl_matches_formats_dequant,
    gemm_iq4_nl_matches_formats_dequant,
    QuantKind::IQ4NL,
    gemv_iq4_nl_f16,
    gemv_iq4_nl_out_f32,
    gemm_iq4_nl_f16
);
quant_golden!(
    gemv_iq4_xs_matches_formats_dequant,
    gemm_iq4_xs_matches_formats_dequant,
    QuantKind::IQ4XS,
    gemv_iq4_xs_f16,
    gemv_iq4_xs_out_f32,
    gemm_iq4_xs_f16
);
quant_golden!(
    gemv_mxfp4_gguf_matches_formats_dequant,
    gemm_mxfp4_gguf_matches_formats_dequant,
    QuantKind::MXFP4,
    gemv_mxfp4_f16,
    gemv_mxfp4_out_f32,
    gemm_mxfp4_f16
);

quant_golden!(
    gemv_iq2_xs_matches_formats_dequant,
    gemm_iq2_xs_matches_formats_dequant,
    QuantKind::IQ2XS,
    gemv_iq2_xs_f16,
    gemv_iq2_xs_out_f32,
    gemm_iq2_xs_f16
);
quant_golden!(
    gemv_iq2_s_matches_formats_dequant,
    gemm_iq2_s_matches_formats_dequant,
    QuantKind::IQ2S,
    gemv_iq2_s_f16,
    gemv_iq2_s_out_f32,
    gemm_iq2_s_f16
);
quant_golden!(
    gemv_iq3_s_matches_formats_dequant,
    gemm_iq3_s_matches_formats_dequant,
    QuantKind::IQ3S,
    gemv_iq3_s_f16,
    gemv_iq3_s_out_f32,
    gemm_iq3_s_f16
);

quant_golden!(
    gemv_iq2_xxs_matches_formats_dequant,
    gemm_iq2_xxs_matches_formats_dequant,
    QuantKind::IQ2XXS,
    gemv_iq2_xxs_f16,
    gemv_iq2_xxs_out_f32,
    gemm_iq2_xxs_f16
);
quant_golden!(
    gemv_iq3_xxs_matches_formats_dequant,
    gemm_iq3_xxs_matches_formats_dequant,
    QuantKind::IQ3XXS,
    gemv_iq3_xxs_f16,
    gemv_iq3_xxs_out_f32,
    gemm_iq3_xxs_f16
);
quant_golden!(
    gemv_iq1_s_matches_formats_dequant,
    gemm_iq1_s_matches_formats_dequant,
    QuantKind::IQ1S,
    gemv_iq1_s_f16,
    gemv_iq1_s_out_f32,
    gemm_iq1_s_f16
);
quant_golden!(
    gemv_iq1_m_matches_formats_dequant,
    gemm_iq1_m_matches_formats_dequant,
    QuantKind::IQ1M,
    gemv_iq1_m_f16,
    gemv_iq1_m_out_f32,
    gemm_iq1_m_f16
);

fn download_u8(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    dev.read(buf, 0, &mut out).unwrap();
    out
}

fn download_f32(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let bytes = download_u8(dev, buf, n * 4);
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[test]
fn gpu_pack_nvfp4_wspolpracuje_z_fp8_gemm_i_gemv() {
    let Some(dev) = device() else { return };
    if !dev.caps().fp8_native {
        return;
    }
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let cols = 512usize;
    let source_rows = 12usize;
    let groups = cols / 16;
    let mut packed = vec![0u8; source_rows * cols / 2];
    let mut source_scales = vec![0x38u8; source_rows * groups];
    for row in 0..source_rows {
        for byte in 0..cols / 2 {
            let lo = ((row * 3 + byte * 5) % 15 + 1) as u8;
            let hi = ((row * 7 + byte * 3) % 15 + 1) as u8;
            packed[row * cols / 2 + byte] = lo | (hi << 4);
        }
        for group in 0..groups {
            source_scales[row * groups + group] = [0x30, 0x38, 0x40][(row + group) % 3];
        }
    }
    let packed_dev = dev
        .alloc(packed.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    let source_scales_dev = dev
        .alloc(source_scales.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&packed, &packed_dev, 0).unwrap();
    dev.write(&source_scales, &source_scales_dev, 0).unwrap();
    let x: Vec<f32> = (0..cols)
        .map(|i| f16::from_f32(fill(i) * 0.05).to_f32())
        .collect();
    let x_dev = upload_f16(dev.as_ref(), &x);

    // Offset Q w sklejonej macierzy, dwie połówki gate/up oraz samodzielne O/down.
    let cases = [("q", 1usize), ("gate", 4), ("up", 6), ("o", 8), ("down", 10)];
    for (label, offset) in cases {
        let rows = 2usize;
        let qweight = dev
            .alloc(rows * cols, MemKind::Device, Pool::Weights)
            .unwrap();
        let scales = dev
            .alloc(rows * 4, MemKind::Device, Pool::Weights)
            .unwrap();
        kernels
            .pack_nvfp4_fp8(
                &qweight,
                &scales,
                &packed_dev,
                &source_scales_dev,
                cols,
                offset,
                rows,
                1.0,
                &stream,
            )
            .unwrap();
        let y = upload_f16(dev.as_ref(), &vec![0.0; rows]);
        kernels
            .gemm_fp8(&y, &qweight, &scales, &x_dev, rows, cols, 1, &stream)
            .unwrap();
        let logits = dev
            .alloc(rows * 4, MemKind::Device, Pool::Weights)
            .unwrap();
        kernels
            .gemv_fp8_out_f32(&logits, &qweight, &scales, &x_dev, rows, cols, &stream)
            .unwrap();
        dev.synchronize().unwrap();

        let codes = download_u8(dev.as_ref(), &qweight, rows * cols);
        let row_scales = download_f32(dev.as_ref(), &scales, rows);
        let gemm = download_f16(dev.as_ref(), &y, rows);
        let gemv = download_f32(dev.as_ref(), &logits, rows);
        let absmax = x.iter().fold(0.0f32, |m, value| m.max(value.abs()));
        let x_scale = absmax / 448.0;
        let x_codes: Vec<u8> = x
            .iter()
            .map(|value| forge_formats::nvfp4::f32_to_f8e4m3(value * 448.0 / absmax))
            .collect();
        for row in 0..rows {
            let mut expected_gemm = 0.0f32;
            let mut expected_gemv = 0.0f32;
            for col in 0..cols {
                let weight = forge_formats::nvfp4::f8e4m3_to_f32(codes[row * cols + col])
                    * row_scales[row];
                expected_gemm += weight
                    * forge_formats::nvfp4::f8e4m3_to_f32(x_codes[col])
                    * x_scale;
                expected_gemv += weight * x[col];
            }
            let gemm_rel = (gemm[row] - expected_gemm).abs() / (expected_gemm.abs() + 1.0);
            let gemv_rel = (gemv[row] - expected_gemv).abs() / (expected_gemv.abs() + 1.0);
            assert!(gemm_rel < 0.03, "{label} GEMM: {} != {expected_gemm}", gemm[row]);
            assert!(gemv_rel < 0.03, "{label} GEMV: {} != {expected_gemv}", gemv[row]);
        }
    }
}
