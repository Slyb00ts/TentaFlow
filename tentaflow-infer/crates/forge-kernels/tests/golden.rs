// ===== File: golden.rs — GPU kernel outputs vs CPU references (forge-formats) =====
// Every kernel artifact must reproduce the CPU reference math within f16
// tolerance. Skips cleanly when no CUDA device is present; on this machine
// (RTX 4090) the tests are expected to run.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::{Kernels, Nvfp4GgufQ8Projection};
use forge_types::MemKind;
use forge_types::{DType, QuantKind};
use half::f16;

const EPS: f32 = 1e-6;

/// Otwarcie urządzenia przez wspólny selektor backendu — te testy muszą
/// działać także na AMD (wcześniej wołały wprost CUDA i na innym sprzęcie
/// pomijały się w ciszy, raportując „ok" bez żadnego pokrycia).
fn device() -> Option<Arc<dyn Device>> {
    match forge_hal::gpu::open(
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
            eprintln!("brak urządzenia GPU dla golden tests: {e}");
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

fn upload_f32(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let bytes = unsafe {
        std::slice::from_raw_parts(vals.as_ptr() as *const u8, std::mem::size_of_val(vals))
    };
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

/// Silnik woła rmsnorm W MIEJSCU (normy Q/K/V pracują na buforze projekcji),
/// przy cols równym rozmiarowi bloku — Gemma 4 ma head_dim 256 i 512.
#[test]
fn rmsnorm_in_place_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for (rows, cols) in [(32usize, 256usize), (16, 512), (8, 128)] {
        let x: Vec<f32> = (0..rows * cols)
            .map(|i| f16::from_f32(fill(i)).to_f32())
            .collect();
        let w: Vec<f32> = (0..cols)
            .map(|i| f16::from_f32(1.0 + (i % 5) as f32 * 0.1).to_f32())
            .collect();

        let xb = upload_f16(dev.as_ref(), &x);
        let wb = upload_f16(dev.as_ref(), &w);
        kernels
            .rmsnorm_f16(&xb, &xb, &wb, rows, cols, EPS, &stream)
            .unwrap();
        dev.synchronize().unwrap();

        let got = download_f16(dev.as_ref(), &xb, rows * cols);
        let mut want = vec![0.0f32; rows * cols];
        for r in 0..rows {
            let row = &x[r * cols..(r + 1) * cols];
            let ss: f32 = row.iter().map(|v| v * v).sum::<f32>() / cols as f32;
            let inv = 1.0 / (ss + EPS).sqrt();
            for c in 0..cols {
                want[r * cols + c] = row[c] * inv * w[c];
            }
        }
        let err = max_abs_err(&got, &want);
        assert!(err < 0.01, "rows={rows} cols={cols} max_err={err}");
    }
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

/// Deterministic Q8_0 stream (34-byte blocks: f16 scale + 32 i8 codes).
fn build_q8_0(rows: usize, cols: usize) -> Vec<u8> {
    let mut wq = Vec::with_capacity(rows * cols / 32 * 34);
    for r in 0..rows {
        for b in 0..cols / 32 {
            let scale = f16::from_f32(0.01 + ((r + b) % 5) as f32 * 0.01);
            wq.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                wq.push((((r * 31 + b * 17 + k * 13) % 255) as i32 - 127) as i8 as u8);
            }
        }
    }
    wq
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
        .gemv_q4_k_out_f32(&y32, 0, &wb, &xb, 0, rows, cols, &stream)
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

    if let Err(e) = kernels.gemm_q4_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream) {
        if skip_if_absent(&e, "gemm_q4_k_f16") {
            return;
        }
        panic!("gemm_q4_k_f16: {e}");
    }
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
                "t {t} row {r}: got {} want {want}",
                got[t * rows + r]
            );
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
        .gemv_q6_k_out_f32(&y32, 0, &wb, &xb, 0, rows, cols, &stream)
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
    assert!(
        rel_l2 < 5e-3,
        "Q6_K MMQ relL2 {rel_l2:.3e} exceeds q8_1 tolerance"
    );
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

    if let Err(e) = kernels.gemm_q6_k_f16(&yb, &wb, &xb, rows, cols, n_tokens, &stream) {
        if skip_if_absent(&e, "gemm_q6_k_f16") {
            return;
        }
        panic!("gemm_q6_k_f16: {e}");
    }
    dev.synchronize().unwrap();

    // Kafel `dot4` kwantyzuje aktywacje do int8 przed mnożeniem, kafel WMMA
    // mnoży je w f16 — referencja musi iść tą samą drogą co dispatch.
    let xref = if kernels.has_artifact("gemm_q6_k_wmma_f16_bm32") {
        x.clone()
    } else {
        quantize_act_q8_1_host(&x, cols)
    };
    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * xref[t * cols + c])
                .sum();
            let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
            assert!(
                rel < 0.02,
                "t {t} row {r}: got {} want {want}",
                got[t * rows + r]
            );
        }
    }
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

    let w_f32 =
        forge_formats::nvfp4::dequantize_nvfp4(&packed, &scales, global_scale, rows, cols, 16)
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

fn e2m1_reference(code: u8) -> f32 {
    const MAGNITUDES: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
    let magnitude = MAGNITUDES[(code & 0x07) as usize];
    if code & 0x08 == 0 {
        magnitude
    } else {
        -magnitude
    }
}

fn make_nvfp4_gguf(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 64;
    let mut weights = vec![0u8; rows * blocks_per_row * 36];
    for row in 0..rows {
        for block in 0..blocks_per_row {
            let base = (row * blocks_per_row + block) * 36;
            for subblock in 0..4 {
                weights[base + subblock] = 0x20 + ((row + block * 3 + subblock * 5) % 25) as u8;
                for j in 0..8 {
                    let low = ((row * 7 + block * 11 + subblock * 3 + j * 5) % 16) as u8;
                    let high = ((row * 13 + block * 5 + subblock * 7 + j * 3 + 1) % 16) as u8;
                    weights[base + 4 + subblock * 8 + j] = low | (high << 4);
                }
            }
        }
    }
    weights
}

fn nvfp4_gguf_dot(weights: &[u8], row: usize, cols: usize, x: &[f32], output_scale: f32) -> f32 {
    let blocks_per_row = cols / 64;
    let row_base = row * blocks_per_row * 36;
    let mut sum = 0.0f32;
    for group in 0..cols / 16 {
        let block = group / 4;
        let subblock = group % 4;
        let block_base = row_base + block * 36;
        let scale_byte = weights[block_base + subblock];
        let scale = if scale_byte == 0x7f {
            0.0
        } else {
            forge_formats::nvfp4::f8e4m3_to_f32(scale_byte)
        };
        let packed_base = block_base + 4 + subblock * 8;
        let x_base = group * 16;
        let mut dot = 0.0f32;
        for j in 0..8 {
            let code = weights[packed_base + j];
            dot += e2m1_reference(code & 0x0f) * x[x_base + j];
            dot += e2m1_reference(code >> 4) * x[x_base + j + 8];
        }
        sum += scale * dot;
    }
    sum * output_scale
}

#[test]
fn gemv_nvfp4_gguf_matches_cpu_for_block_and_model_shape() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for (rows, cols) in [(7usize, 64usize), (5120usize, 5120usize)] {
        let mut weights = make_nvfp4_gguf(rows, cols);
        weights[0] = 0x7f;
        let x: Vec<f32> = (0..cols)
            .map(|i| f16::from_f32(fill(i) * 0.025).to_f32())
            .collect();
        let wb = dev
            .alloc(weights.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(&weights, &wb, 0).unwrap();
        let xb = upload_f16(dev.as_ref(), &x);
        let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);

        let output_scale = 0.3125;
        kernels
            .gemv_nvfp4_gguf_f16(&yb, &wb, &xb, rows, cols, output_scale, &stream)
            .unwrap();
        dev.synchronize().unwrap();

        let got = download_f16(dev.as_ref(), &yb, rows);
        for (row, &value) in got.iter().enumerate() {
            let want = nvfp4_gguf_dot(&weights, row, cols, &x, output_scale);
            let rel = (value - want).abs() / (want.abs() + 1.0);
            assert!(
                rel < 0.02,
                "kształt {rows}x{cols}, wiersz {row}: otrzymano {value}, oczekiwano {want}"
            );
        }
    }
}

#[test]
fn gemv_nvfp4_gguf_q8_1_matches_cpu_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for (rows, cols) in [(7usize, 64usize), (5120usize, 5120usize)] {
        let mut weights = make_nvfp4_gguf(rows, cols);
        weights[0] = 0x7f;
        let x: Vec<f32> = (0..cols)
            .map(|i| f16::from_f32(fill(i) * 0.025).to_f32())
            .collect();
        let weights_buffer = dev
            .alloc(weights.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(&weights, &weights_buffer, 0).unwrap();
        let input = upload_f16(dev.as_ref(), &x);
        let output = upload_f16(dev.as_ref(), &vec![0.0; rows]);
        let output_scale = 0.3125;
        kernels
            .gemv_nvfp4_gguf_q8_1_group_f16(
                &[Nvfp4GgufQ8Projection {
                    output: &output,
                    weights: &weights_buffer,
                    rows,
                    output_scale,
                }],
                &input,
                cols,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let got = download_f16(dev.as_ref(), &output, rows);
        let want: Vec<f32> = (0..rows)
            .map(|row| nvfp4_gguf_dot(&weights, row, cols, &x, output_scale))
            .collect();
        let error = got
            .iter()
            .zip(&want)
            .map(|(&actual, &reference)| (actual - reference).powi(2))
            .sum::<f32>();
        let norm = want.iter().map(|value| value.powi(2)).sum::<f32>();
        let relative_l2 = (error / norm.max(1e-12)).sqrt();
        assert!(
            relative_l2 < 0.02,
            "kształt {rows}x{cols}: względny błąd L2 {relative_l2:.4}"
        );
    }
}

#[test]
fn gemm_nvfp4_gguf_dispatch_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.artifacts().has("gemm_nvfp4_f16_bm64") {
        eprintln!(
            "pomijam nvfp4 gguf dispatch: brak kernela gemm_nvfp4_f16_bm64 dla tej architektury"
        );
        return;
    }
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (11usize, 576usize);
    let mut weights = make_nvfp4_gguf(rows, cols);
    weights[0] = 0x7f;
    let weights_buffer = dev
        .alloc(weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&weights, &weights_buffer, 0).unwrap();

    for n_tokens in [2usize, 3, 4, 5, 8, 9, 16, 17, 128, 129] {
        let input: Vec<f32> = (0..n_tokens * cols)
            .map(|i| f16::from_f32(fill(i + n_tokens) * 0.025).to_f32())
            .collect();
        let input_buffer = upload_f16(dev.as_ref(), &input);
        let output_buffer = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
        let output_scale = 0.625;
        kernels
            .gemm_nvfp4_gguf_f16(
                &output_buffer,
                &weights_buffer,
                &input_buffer,
                rows,
                cols,
                n_tokens,
                output_scale,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let got = download_f16(dev.as_ref(), &output_buffer, n_tokens * rows);
        for token in 0..n_tokens {
            let token_input = &input[token * cols..(token + 1) * cols];
            for row in 0..rows {
                let want = nvfp4_gguf_dot(&weights, row, cols, token_input, output_scale);
                let value = got[token * rows + row];
                let rel = (value - want).abs() / (want.abs() + 1.0);
                assert!(
                    rel < 0.02,
                    "T={n_tokens}, wiersz {row}: otrzymano {value}, oczekiwano {want}"
                );
            }
        }
    }
}

#[test]
fn gemm_nvfp4_gguf_rejects_invalid_buffers_and_scale() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let tiny = upload_f16(dev.as_ref(), &[0.0]);

    assert!(kernels
        .gather_f16_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 0, &stream)
        .is_err());
    assert!(kernels
        .gather_f16_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 1, &stream)
        .is_err());
    assert!(kernels
        .gemm_nvfp4_gguf_f16(&tiny, &tiny, &tiny, 2, 64, 3, 1.0, &stream)
        .is_err());
    assert!(kernels
        .gemm_nvfp4_gguf_f16(&tiny, &tiny, &tiny, 1, 64, 3, f32::NAN, &stream)
        .is_err());
    assert!(kernels
        .gemm_nvfp4_gguf_f16(&tiny, &tiny, &tiny, 1, 64, 1, 1.0, &stream)
        .is_err());
}

fn make_q8_0(rows: usize, cols: usize) -> Vec<u8> {
    let blocks_per_row = cols / 32;
    let mut weights = Vec::with_capacity(rows * blocks_per_row * 34);
    for row in 0..rows {
        for block in 0..blocks_per_row {
            let scale = f16::from_f32(0.003 + ((row + block) % 7) as f32 * 0.001);
            weights.extend_from_slice(&scale.to_le_bytes());
            for k in 0..32 {
                weights.push((((row * 17 + block * 13 + k * 7) % 63) as i32 - 31) as i8 as u8);
            }
        }
    }
    weights
}

fn fp32_to_ue4m3(mut value: f32) -> u8 {
    if value <= 0.0 {
        return 0;
    }
    value = value.min(448.0);
    let bits = value.to_bits();
    let fp32_exp = ((bits >> 23) & 0xff) as i32 - 127;
    let fp32_man = ((bits >> 20) & 7) as i32;
    let mut exponent = fp32_exp + 7;
    if exponent <= 0 {
        return (value.mul_add(512.0, 0.5) as i32).clamp(0, 7) as u8;
    }
    if exponent >= 15 {
        return 0x7e;
    }
    let mut mantissa = fp32_man + ((bits >> 19) & 1) as i32;
    if mantissa > 7 {
        mantissa = 0;
        exponent += 1;
        if exponent >= 15 {
            return 0x7e;
        }
    }
    ((exponent << 3) | mantissa) as u8
}

fn ue4m3(value: u8) -> f32 {
    if value == 0 || value == 0x7f {
        return 0.0;
    }
    let exponent = (value >> 3) & 0x0f;
    let mantissa = (value & 7) as f32;
    if exponent == 0 {
        mantissa / 512.0
    } else {
        (1.0 + mantissa / 8.0) * 2.0f32.powi(exponent as i32 - 7)
    }
}

fn best_e2m1(value: f32, scale: f32) -> u8 {
    const VALUES: [f32; 16] = [
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    ];
    let mut best = 0;
    let mut error = value.abs();
    for (index, candidate) in VALUES.iter().enumerate().skip(1) {
        let next = (candidate * scale - value).abs();
        if next < error {
            best = index as u8;
            error = next;
        }
    }
    best
}

fn q8_0_to_nvfp4_reference(source: &[u8], rows: usize, cols: usize) -> Vec<u8> {
    let blocks = rows * cols / 64;
    let mut output = vec![0u8; blocks * 36];
    for block in 0..blocks {
        for subblock in 0..4 {
            let q8_block = block * 2 + subblock / 2;
            let q8_base = q8_block * 34;
            let q8_scale = f16::from_le_bytes([source[q8_base], source[q8_base + 1]]).to_f32();
            let q8_offset = q8_base + 2 + (subblock % 2) * 16;
            let values: Vec<f32> = source[q8_offset..q8_offset + 16]
                .iter()
                .map(|value| (*value as i8) as f32 * q8_scale)
                .collect();
            let amax = values
                .iter()
                .fold(0.0f32, |acc, value| acc.max(value.abs()));
            let scale_code = fp32_to_ue4m3(amax / 6.0);
            let scale = ue4m3(scale_code);
            output[block * 36 + subblock] = scale_code;
            for index in 0..8 {
                let low = best_e2m1(values[index], scale);
                let high = best_e2m1(values[index + 8], scale);
                output[block * 36 + 4 + subblock * 8 + index] = low | (high << 4);
            }
        }
    }
    output
}

#[test]
fn gpu_pack_q8_0_nvfp4_i_gemv_f32_zgadza_sie_z_referencja() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (17usize, 128usize);
    let q8 = make_q8_0(rows, cols);
    let expected = q8_0_to_nvfp4_reference(&q8, rows, cols);
    let source = dev.alloc(q8.len(), MemKind::Device, Pool::Weights).unwrap();
    let packed = dev
        .alloc(expected.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&q8, &source, 0).unwrap();
    kernels
        .pack_q8_0_nvfp4_gguf(&packed, &source, rows, cols, &stream)
        .unwrap();

    let x: Vec<f32> = (0..cols)
        .map(|index| f16::from_f32(fill(index) * 0.07).to_f32())
        .collect();
    let x_dev = upload_f16(dev.as_ref(), &x);
    let logits = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    kernels
        .gemv_nvfp4_gguf_out_f32(&logits, &packed, &x_dev, rows, cols, 1.0, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    assert_eq!(download_u8(dev.as_ref(), &packed, expected.len()), expected);
    let weights = forge_formats::dequant::dequantize_to_f32(
        DType::F32,
        QuantKind::NVFP4Gguf,
        &expected,
        rows * cols,
    )
    .unwrap();
    let reference: Vec<f32> = (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| weights[row * cols + col] * x[col])
                .sum()
        })
        .collect();
    let got = download_f32(dev.as_ref(), &logits, rows);
    assert!(max_abs_err(&got, &reference) < 0.02);
    let top = |values: &[f32]| {
        values
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap()
            .0
    };
    assert_eq!(top(&got), top(&reference));
}

#[test]
fn gpu_nvfp4_logits_b2_sa_bitowo_zgodne_z_dwoma_b1() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (257usize, 128usize);
    let q8 = make_q8_0(rows, cols);
    let packed_bytes = rows * (cols / 64) * 36;
    let source = dev.alloc(q8.len(), MemKind::Device, Pool::Weights).unwrap();
    let packed = dev
        .alloc(packed_bytes, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&q8, &source, 0).unwrap();
    kernels
        .pack_q8_0_nvfp4_gguf(&packed, &source, rows, cols, &stream)
        .unwrap();
    let first: Vec<f32> = (0..cols).map(|index| fill(index) * 0.03125).collect();
    let second: Vec<f32> = (0..cols)
        .map(|index| fill(index + 11) * -0.046875)
        .collect();
    let first_dev = upload_f16(dev.as_ref(), &first);
    let second_dev = upload_f16(dev.as_ref(), &second);
    let mut joined = first.clone();
    joined.extend_from_slice(&second);
    let joined_dev = upload_f16(dev.as_ref(), &joined);
    let first_logits = dev
        .alloc(rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let second_logits = dev
        .alloc(rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let batch_logits = dev
        .alloc(2 * rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .gemv_nvfp4_gguf_out_f32(&first_logits, &packed, &first_dev, rows, cols, 1.0, &stream)
        .unwrap();
    kernels
        .gemv_nvfp4_gguf_out_f32(
            &second_logits,
            &packed,
            &second_dev,
            rows,
            cols,
            1.0,
            &stream,
        )
        .unwrap();
    kernels
        .gemm_nvfp4_gguf_out_f32_b2(
            &batch_logits,
            &packed,
            &joined_dev,
            rows,
            cols,
            1.0,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let mut expected = download_f32(dev.as_ref(), &first_logits, rows);
    expected.extend(download_f32(dev.as_ref(), &second_logits, rows));
    assert_eq!(
        download_f32(dev.as_ref(), &batch_logits, 2 * rows),
        expected
    );
}

#[test]
fn gpu_nvfp4_logits_b4_b8_b16_sa_bitowo_zgodne_z_b1() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (257usize, 128usize);
    let q8 = make_q8_0(rows, cols);
    let packed_bytes = rows * (cols / 64) * 36;
    let source = dev.alloc(q8.len(), MemKind::Device, Pool::Weights).unwrap();
    let packed = dev
        .alloc(packed_bytes, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&q8, &source, 0).unwrap();
    kernels
        .pack_q8_0_nvfp4_gguf(&packed, &source, rows, cols, &stream)
        .unwrap();

    for batch in [4usize, 8, 16] {
        let joined = (0..batch)
            .flat_map(|lane| {
                (0..cols).map(move |column| fill(column + lane * 11) * (lane as f32 + 1.0) * 0.01)
            })
            .collect::<Vec<_>>();
        let joined_dev = upload_f16(dev.as_ref(), &joined);
        let batch_logits = dev
            .alloc(batch * rows * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        kernels
            .gemm_nvfp4_gguf_out_f32_batch(
                &batch_logits,
                &packed,
                &joined_dev,
                rows,
                cols,
                batch,
                1.0,
                &stream,
            )
            .unwrap();
        let mut expected = Vec::with_capacity(batch * rows);
        for lane in 0..batch {
            let input = upload_f16(dev.as_ref(), &joined[lane * cols..(lane + 1) * cols]);
            let logits = dev
                .alloc(rows * 4, MemKind::Device, Pool::Activations)
                .unwrap();
            kernels
                .gemv_nvfp4_gguf_out_f32(&logits, &packed, &input, rows, cols, 1.0, &stream)
                .unwrap();
            dev.synchronize().unwrap();
            expected.extend(download_f32(dev.as_ref(), &logits, rows));
        }
        dev.synchronize().unwrap();
        assert_eq!(
            download_f32(dev.as_ref(), &batch_logits, batch * rows),
            expected,
            "B={batch}"
        );
    }
}

#[test]
fn gpu_q8_0_logits_b2_sa_bitowo_zgodne_z_dwoma_b1() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (257usize, 128usize);
    let weights = make_q8_0(rows, cols);
    let weights_dev = dev
        .alloc(weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&weights, &weights_dev, 0).unwrap();
    let first: Vec<f32> = (0..cols).map(|index| fill(index) * 0.03125).collect();
    let second: Vec<f32> = (0..cols)
        .map(|index| fill(index + 11) * -0.046875)
        .collect();
    let first_dev = upload_f16(dev.as_ref(), &first);
    let second_dev = upload_f16(dev.as_ref(), &second);
    let mut joined = first.clone();
    joined.extend_from_slice(&second);
    let joined_dev = upload_f16(dev.as_ref(), &joined);
    let first_logits = dev
        .alloc(rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let second_logits = dev
        .alloc(rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let batch_logits = dev
        .alloc(2 * rows * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .gemv_q8_0_out_f32(&first_logits, &weights_dev, &first_dev, rows, cols, &stream)
        .unwrap();
    kernels
        .gemv_q8_0_out_f32(
            &second_logits,
            &weights_dev,
            &second_dev,
            rows,
            cols,
            &stream,
        )
        .unwrap();
    kernels
        .gemm_q8_0_f16_exact_out_f32_at(
            &batch_logits,
            &weights_dev,
            0,
            &joined_dev,
            rows,
            cols,
            2,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let mut expected = download_f32(dev.as_ref(), &first_logits, rows);
    expected.extend(download_f32(dev.as_ref(), &second_logits, rows));
    assert_eq!(
        download_f32(dev.as_ref(), &batch_logits, 2 * rows),
        expected
    );
}

fn q8_0_dot(weights: &[u8], row: usize, cols: usize, x: &[f32]) -> f32 {
    let blocks_per_row = cols / 32;
    let row_base = row * blocks_per_row * 34;
    let mut sum = 0.0f32;
    for block in 0..blocks_per_row {
        let offset = row_base + block * 34;
        let scale = f16::from_le_bytes([weights[offset], weights[offset + 1]]).to_f32();
        let mut dot = 0.0f32;
        for k in 0..32 {
            dot += (weights[offset + 2 + k] as i8) as f32 * x[block * 32 + k];
        }
        sum += scale * dot;
    }
    sum
}

#[test]
fn mtp_gather_q8_0_row_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (vocab_size, hidden_size, token) = (5usize, 128usize, 3usize);
    let weights = make_q8_0(vocab_size, hidden_size);
    let weights_buffer = dev
        .alloc(weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&weights, &weights_buffer, 0).unwrap();
    let token_buffer = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&(token as i32).to_le_bytes(), &token_buffer, 0)
        .unwrap();
    let status = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
    let output = upload_f16(dev.as_ref(), &vec![0.0; hidden_size]);

    kernels
        .gather_q8_0_row_f16(
            &output,
            &weights_buffer,
            &token_buffer,
            &status,
            0,
            vocab_size,
            hidden_size,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &output, hidden_size);
    let row_base = token * (hidden_size / 32) * 34;
    for (element, &value) in got.iter().enumerate() {
        let block = element / 32;
        let offset = row_base + block * 34;
        let scale = f16::from_le_bytes([weights[offset], weights[offset + 1]]).to_f32();
        let want = scale * (weights[offset + 2 + element % 32] as i8) as f32;
        assert!(
            (value - want).abs() < 0.002,
            "element {element}: otrzymano {value}, oczekiwano {want}"
        );
    }
}

#[test]
fn mtp_gather_f16_row_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (vocab_size, hidden_size, token) = (5usize, 128usize, 3usize);
    let weights: Vec<f32> = (0..vocab_size * hidden_size)
        .map(|index| f16::from_f32(fill(index) * 0.05).to_f32())
        .collect();
    let weights_buffer = upload_f16(dev.as_ref(), &weights);
    let token_buffer = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&(token as i32).to_le_bytes(), &token_buffer, 0)
        .unwrap();
    let status = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
    let output = upload_f16(dev.as_ref(), &vec![0.0; hidden_size]);

    kernels
        .gather_f16_row_f16(
            &output,
            &weights_buffer,
            &token_buffer,
            &status,
            0,
            vocab_size,
            hidden_size,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    assert_eq!(
        download_f16(dev.as_ref(), &output, hidden_size),
        weights[token * hidden_size..(token + 1) * hidden_size]
    );
}

#[test]
fn mtp_gather_nvfp4_gguf_row_matches_cpu() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (vocab_size, hidden_size, token) = (5usize, 128usize, 3usize);
    let output_scale = 0.3125f32;
    let mut weights = make_nvfp4_gguf(vocab_size, hidden_size);
    let row_base = token * (hidden_size / 64) * 36;
    weights[row_base] = 0x7f;
    let weights_buffer = dev
        .alloc(weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&weights, &weights_buffer, 0).unwrap();
    let token_buffer = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&(token as i32).to_le_bytes(), &token_buffer, 0)
        .unwrap();
    let status = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
    dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
    let output = upload_f16(dev.as_ref(), &vec![0.0; hidden_size]);

    kernels
        .gather_nvfp4_gguf_row_f16(
            &output,
            &weights_buffer,
            &token_buffer,
            &status,
            0,
            vocab_size,
            hidden_size,
            output_scale,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &output, hidden_size);
    for (element, &value) in got.iter().enumerate() {
        let block = element / 64;
        let subblock = (element % 64) / 16;
        let within = element % 16;
        let block_base = row_base + block * 36;
        let scale_byte = weights[block_base + subblock];
        let scale = if scale_byte == 0x7f {
            0.0
        } else {
            forge_formats::nvfp4::f8e4m3_to_f32(scale_byte)
        };
        let packed = weights[block_base + 4 + subblock * 8 + within % 8];
        let code = if within < 8 {
            packed & 0x0f
        } else {
            packed >> 4
        };
        let want = e2m1_reference(code) * scale * output_scale;
        assert!(
            (value - want).abs() < 0.002,
            "element {element}: otrzymano {value}, oczekiwano {want}"
        );
    }
}

#[test]
fn mtp_gather_rejects_invalid_shapes_and_buffers() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let tiny = upload_f16(dev.as_ref(), &[0.0]);

    assert!(kernels
        .gather_q8_0_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 31, &stream)
        .is_err());
    assert!(kernels
        .gather_q8_0_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 32, &stream)
        .is_err());
    assert!(kernels
        .gather_nvfp4_gguf_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 63, 1.0, &stream)
        .is_err());
    assert!(kernels
        .gather_nvfp4_gguf_row_f16(&tiny, &tiny, &tiny, &tiny, 0, 1, 64, 1.0, &stream)
        .is_err());
}

#[test]
fn mtp_gather_guards_device_token_out_of_range() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (vocab_size, hidden_size) = (2usize, 64usize);
    let q8 = make_q8_0(vocab_size, hidden_size);
    let nvfp4 = make_nvfp4_gguf(vocab_size, hidden_size);
    let f16_weights: Vec<f32> = (0..vocab_size * hidden_size).map(fill).collect();
    let f16_buffer = upload_f16(dev.as_ref(), &f16_weights);
    let q8_buffer = dev.alloc(q8.len(), MemKind::Device, Pool::Weights).unwrap();
    let nvfp4_buffer = dev
        .alloc(nvfp4.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&q8, &q8_buffer, 0).unwrap();
    dev.write(&nvfp4, &nvfp4_buffer, 0).unwrap();

    for token in [-1i32, vocab_size as i32] {
        let token_buffer = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
        let status = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();
        dev.write(&token.to_le_bytes(), &token_buffer, 0).unwrap();
        dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
        let q8_output = upload_f16(dev.as_ref(), &vec![1.0; hidden_size]);
        kernels
            .gather_q8_0_row_f16(
                &q8_output,
                &q8_buffer,
                &token_buffer,
                &status,
                0,
                vocab_size,
                hidden_size,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        let mut status_bytes = [0u8; 4];
        dev.read(&status, 0, &mut status_bytes).unwrap();
        assert_eq!(i32::from_le_bytes(status_bytes), 1);
        assert!(download_f16(dev.as_ref(), &q8_output, hidden_size)
            .iter()
            .all(|&value| value == 0.0));

        dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
        let f16_output = upload_f16(dev.as_ref(), &vec![1.0; hidden_size]);
        kernels
            .gather_f16_row_f16(
                &f16_output,
                &f16_buffer,
                &token_buffer,
                &status,
                0,
                vocab_size,
                hidden_size,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        dev.read(&status, 0, &mut status_bytes).unwrap();
        assert_eq!(i32::from_le_bytes(status_bytes), 1);
        assert!(download_f16(dev.as_ref(), &f16_output, hidden_size)
            .iter()
            .all(|&value| value == 0.0));

        dev.write(&0i32.to_le_bytes(), &status, 0).unwrap();
        let nvfp4_output = upload_f16(dev.as_ref(), &vec![1.0; hidden_size]);
        kernels
            .gather_nvfp4_gguf_row_f16(
                &nvfp4_output,
                &nvfp4_buffer,
                &token_buffer,
                &status,
                0,
                vocab_size,
                hidden_size,
                1.0,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        dev.read(&status, 0, &mut status_bytes).unwrap();
        assert_eq!(i32::from_le_bytes(status_bytes), 1);
        assert!(download_f16(dev.as_ref(), &nvfp4_output, hidden_size)
            .iter()
            .all(|&value| value == 0.0));
    }
}

/// Rozwinięcie pętli w `rmsnorm_residual_f16` zachowuje kolejność akumulacji,
/// więc test sprawdza i wynik normy, i zaktualizowany strumień residuału —
/// obie ścieżki (rozwinięta i resztkowa) na kolumnach niepodzielnych przez
/// 256 * NORM_UNROLL.
#[test]
fn rmsnorm_residual_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for (rows, cols) in [(4usize, 4096usize), (3, 2048), (1, 2568), (2, 300)] {
        let residual: Vec<f32> = (0..rows * cols)
            .map(|i| f16::from_f32(fill(i)).to_f32())
            .collect();
        let x: Vec<f32> = (0..rows * cols)
            .map(|i| f16::from_f32(fill(i + 7) * 0.5).to_f32())
            .collect();
        let w: Vec<f32> = (0..cols)
            .map(|i| f16::from_f32(1.0 + (i % 5) as f32 * 0.1).to_f32())
            .collect();

        let rb = upload_f16(dev.as_ref(), &residual);
        let xb = upload_f16(dev.as_ref(), &x);
        let wb = upload_f16(dev.as_ref(), &w);
        let yb = upload_f16(dev.as_ref(), &vec![0.0; rows * cols]);
        kernels
            .rmsnorm_residual_f16(&yb, &rb, &xb, &wb, rows, cols, EPS, &stream)
            .unwrap();
        dev.synchronize().unwrap();

        let got = download_f16(dev.as_ref(), &yb, rows * cols);
        let got_residual = download_f16(dev.as_ref(), &rb, rows * cols);
        let mut want = vec![0.0f32; rows * cols];
        let mut want_residual = vec![0.0f32; rows * cols];
        for r in 0..rows {
            let sum: Vec<f32> = (0..cols)
                .map(|c| f16::from_f32(residual[r * cols + c] + x[r * cols + c]).to_f32())
                .collect();
            want_residual[r * cols..(r + 1) * cols].copy_from_slice(&sum);
            want[r * cols..(r + 1) * cols].copy_from_slice(&rmsnorm_reference(&sum, &w));
        }
        assert_eq!(
            got_residual, want_residual,
            "strumień residuału musi być dokładny dla {rows}x{cols}"
        );
        assert!(
            max_abs_err(&got, &want) < 0.01,
            "norma rozjechała się dla {rows}x{cols}"
        );
    }
}

fn rmsnorm_reference(values: &[f32], weight: &[f32]) -> Vec<f32> {
    let mean_sq = values.iter().map(|value| value * value).sum::<f32>() / values.len() as f32;
    let inv = 1.0 / (mean_sq + EPS).sqrt();
    values
        .iter()
        .zip(weight)
        .map(|(value, weight)| f16::from_f32(value * inv * weight).to_f32())
        .collect()
}

#[test]
fn mtp_prepare_matches_cpu_for_small_and_qwen_hidden() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for hidden_size in [128usize, 5120usize] {
        let embedding_row: Vec<f32> = (0..hidden_size)
            .map(|i| f16::from_f32(fill(i) * 0.05).to_f32())
            .collect();
        let hidden: Vec<f32> = (0..hidden_size)
            .map(|i| f16::from_f32(fill(i + 11) * 0.04).to_f32())
            .collect();
        let enorm: Vec<f32> = (0..hidden_size)
            .map(|i| f16::from_f32(0.8 + (i % 9) as f32 * 0.025).to_f32())
            .collect();
        let hnorm: Vec<f32> = (0..hidden_size)
            .map(|i| f16::from_f32(0.9 + (i % 7) as f32 * 0.02).to_f32())
            .collect();
        let projection = make_q8_0(hidden_size, 2 * hidden_size);

        let output_buffer = upload_f16(dev.as_ref(), &vec![0.0; hidden_size]);
        let embedding_buffer = upload_f16(dev.as_ref(), &embedding_row);
        let hidden_buffer = upload_f16(dev.as_ref(), &hidden);
        let enorm_buffer = upload_f16(dev.as_ref(), &enorm);
        let hnorm_buffer = upload_f16(dev.as_ref(), &hnorm);
        let projection_buffer = dev
            .alloc(projection.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        dev.write(&projection, &projection_buffer, 0).unwrap();

        kernels
            .mtp_prepare_f16(
                &output_buffer,
                &embedding_buffer,
                &hidden_buffer,
                &enorm_buffer,
                &hnorm_buffer,
                &projection_buffer,
                hidden_size,
                EPS,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let mut joined = rmsnorm_reference(&embedding_row, &enorm);
        joined.extend(rmsnorm_reference(&hidden, &hnorm));
        let got = download_f16(dev.as_ref(), &output_buffer, hidden_size);
        for (row, &value) in got.iter().enumerate() {
            let want = q8_0_dot(&projection, row, 2 * hidden_size, &joined);
            let rel = (value - want).abs() / (want.abs() + 1.0);
            assert!(
                rel < 0.02,
                "MTP H={hidden_size}, wiersz {row}: otrzymano {value}, oczekiwano {want}"
            );
        }
    }
}

#[test]
fn mtp_prepare_rejects_invalid_shapes_and_buffers() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let tiny = upload_f16(dev.as_ref(), &[0.0]);

    assert!(kernels
        .mtp_prepare_f16(&tiny, &tiny, &tiny, &tiny, &tiny, &tiny, 5136, EPS, &stream,)
        .is_err());
    assert!(kernels
        .mtp_prepare_f16(&tiny, &tiny, &tiny, &tiny, &tiny, &tiny, 16, 0.0, &stream)
        .is_err());
    assert!(kernels
        .mtp_prepare_f16(&tiny, &tiny, &tiny, &tiny, &tiny, &tiny, 16, EPS, &stream)
        .is_err());
}

#[test]
fn deltanet_batched_scan_matches_sequential_checkpoints_and_gpu_commit() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let n_v_heads = 4usize;
    let d_state = 128usize;
    let vector_elements = n_v_heads * d_state;
    let state_elements = n_v_heads * d_state * d_state;
    let state_bytes = state_elements * 4;
    let initial_state: Vec<f32> = (0..state_elements)
        .map(|index| fill(index + 17) * 0.0025)
        .collect();

    for n_steps in 2..=4 {
        let q: Vec<f32> = (0..n_steps * vector_elements)
            .map(|index| f16::from_f32(fill(index * 3 + 5) * 0.04).to_f32())
            .collect();
        let k: Vec<f32> = (0..n_steps * vector_elements)
            .map(|index| f16::from_f32(fill(index * 5 + 7) * 0.04).to_f32())
            .collect();
        let v: Vec<f32> = (0..n_steps * vector_elements)
            .map(|index| f16::from_f32(fill(index * 7 + 11) * 0.03).to_f32())
            .collect();
        let g: Vec<f32> = (0..n_steps * n_v_heads)
            .map(|index| -0.05 - (index % 7) as f32 * 0.025)
            .collect();
        let beta: Vec<f32> = (0..n_steps * n_v_heads)
            .map(|index| 0.2 + (index % 5) as f32 * 0.1)
            .collect();

        let sequential_state = upload_f32(dev.as_ref(), &initial_state);
        let sequential_checkpoints = dev
            .alloc(n_steps * state_bytes, MemKind::Device, Pool::Weights)
            .unwrap();
        let sequential_outputs = dev
            .alloc(
                n_steps * vector_elements * 2,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        for token in 0..n_steps {
            let vector_range = token * vector_elements..(token + 1) * vector_elements;
            let gate_range = token * n_v_heads..(token + 1) * n_v_heads;
            let q_token = upload_f16(dev.as_ref(), &q[vector_range.clone()]);
            let k_token = upload_f16(dev.as_ref(), &k[vector_range.clone()]);
            let v_token = upload_f16(dev.as_ref(), &v[vector_range]);
            let g_token = upload_f32(dev.as_ref(), &g[gate_range.clone()]);
            let beta_token = upload_f32(dev.as_ref(), &beta[gate_range]);
            let output_token = dev
                .alloc(vector_elements * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            kernels
                .deltanet_gated_step_f16(
                    &output_token,
                    &sequential_state,
                    &q_token,
                    &k_token,
                    &v_token,
                    &g_token,
                    &beta_token,
                    n_v_heads,
                    d_state,
                    &stream,
                )
                .unwrap();
            dev.copy(
                &sequential_state,
                0,
                &sequential_checkpoints,
                token * state_bytes,
                state_bytes,
                &stream,
            )
            .unwrap();
            dev.copy(
                &output_token,
                0,
                &sequential_outputs,
                token * vector_elements * 2,
                vector_elements * 2,
                &stream,
            )
            .unwrap();
        }

        let state_in = upload_f32(dev.as_ref(), &initial_state);
        let q_all = upload_f16(dev.as_ref(), &q);
        let k_all = upload_f16(dev.as_ref(), &k);
        let v_all = upload_f16(dev.as_ref(), &v);
        let g_all = upload_f32(dev.as_ref(), &g);
        let beta_all = upload_f32(dev.as_ref(), &beta);
        let checkpoint_byte_offset = n_steps * state_bytes;
        let checkpoints = dev
            .alloc(2 * n_steps * state_bytes, MemKind::Device, Pool::Weights)
            .unwrap();
        let outputs = dev
            .alloc(
                n_steps * vector_elements * 2,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        kernels
            .deltanet_gated_scan_f16_at(
                &outputs,
                &checkpoints,
                checkpoint_byte_offset,
                &state_in,
                &q_all,
                &k_all,
                &v_all,
                &g_all,
                &beta_all,
                n_steps,
                n_v_heads,
                d_state,
                &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let expected_states = download_f32(
            dev.as_ref(),
            &sequential_checkpoints,
            n_steps * state_elements,
        );
        let retained_states =
            download_f32(dev.as_ref(), &checkpoints, 2 * n_steps * state_elements);
        let actual_states = &retained_states[n_steps * state_elements..];
        let expected_outputs =
            download_f16(dev.as_ref(), &sequential_outputs, n_steps * vector_elements);
        let actual_outputs = download_f16(dev.as_ref(), &outputs, n_steps * vector_elements);
        assert_eq!(actual_states, expected_states, "T={n_steps}: checkpointy");
        assert_eq!(actual_outputs, expected_outputs, "T={n_steps}: wyjścia");
        assert_eq!(
            download_f32(dev.as_ref(), &state_in, state_elements),
            initial_state,
            "T={n_steps}: skan zmodyfikował stan wejściowy"
        );

        let accepted_index = dev
            .alloc(
                std::mem::size_of::<i32>(),
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        let accepted_cases = -1..=n_steps as i32 + 1;
        for accepted in accepted_cases {
            let committed = upload_f32(dev.as_ref(), &initial_state);
            dev.write(&accepted.to_le_bytes(), &accepted_index, 0)
                .unwrap();
            kernels
                .deltanet_commit_checkpoint_f32_at(
                    &committed,
                    &checkpoints,
                    checkpoint_byte_offset,
                    &accepted_index,
                    n_steps,
                    n_v_heads,
                    d_state,
                    &stream,
                )
                .unwrap();
            dev.synchronize().unwrap();
            let actual = download_f32(dev.as_ref(), &committed, state_elements);
            let expected = if (1..=n_steps as i32).contains(&accepted) {
                let checkpoint = accepted as usize - 1;
                &expected_states[checkpoint * state_elements..(checkpoint + 1) * state_elements]
            } else {
                &initial_state
            };
            assert_eq!(actual, expected, "T={n_steps}, accepted={accepted}");
        }
    }
}

#[test]
fn deltanet_batched_scan_rejects_invalid_shapes_and_buffers() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let tiny = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();

    for n_steps in [0, 1, 5, usize::MAX] {
        assert!(kernels
            .deltanet_gated_scan_f16(
                &tiny, &tiny, &tiny, &tiny, &tiny, &tiny, &tiny, &tiny, n_steps, 1, 128, &stream,
            )
            .is_err());
    }
    assert!(kernels
        .deltanet_gated_scan_f16(
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            4,
            usize::MAX,
            1024,
            &stream,
        )
        .is_err());
    assert!(kernels
        .deltanet_commit_checkpoint_f32(&tiny, &tiny, &tiny, 4, usize::MAX, 1024, &stream)
        .is_err());
    assert!(kernels
        .deltanet_gated_scan_f16_at(
            &tiny,
            &tiny,
            usize::MAX,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            &tiny,
            2,
            1,
            1,
            &stream,
        )
        .is_err());
    assert!(kernels
        .deltanet_commit_checkpoint_f32_at(&tiny, &tiny, usize::MAX, &tiny, 2, 1, 1, &stream,)
        .is_err());
}

#[test]
fn batched_nvfp4_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.artifacts().has("gemm_nvfp4_f16_bm64") {
        eprintln!("pomijam batched nvfp4: brak kernela gemm_nvfp4_f16_bm64 dla tej architektury");
        return;
    }
    let stream = dev.create_stream().unwrap();
    let (rows, cols) = (24usize, 256usize);
    let global_scale = 12.5f32;
    let packed: Vec<u8> = (0..rows * cols / 2)
        .map(|i| ((i * 41 + 7) % 256) as u8)
        .collect();
    let scales: Vec<u8> = (0..rows * cols / 16)
        .map(|i| (((i * 29 + 3) % 96) + 16) as u8)
        .collect();
    let weights =
        forge_formats::nvfp4::dequantize_nvfp4(&packed, &scales, global_scale, rows, cols, 16)
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
    if !kernels.artifacts().has("gemm_f16_bm64") {
        eprintln!("pomijam batched f16 logits: brak kernela gemm_f16_bm64 dla tej architektury");
        return;
    }
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
            .gemm_f16_out_f32_at(&y_buf, &weights_buf, 0, &x_buf, rows, cols, batch, &stream)
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
        .gemm_f16_out_f32_at(&y_buf, &weights_buf, 0, &x_buf, rows, cols, 0, &stream,)
        .is_err());
    assert!(kernels
        .gemm_f16_out_f32_at(&y_buf, &weights_buf, 0, &x_buf, rows, 4, 1, &stream,)
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

    // Ścieżka dzielona zapisuje częściowe wyniki do własnego bufora
    // (8 partycji po head_dim + 4 f32 na głowicę), więc test musi go dać.
    let parts = dev
        .alloc(
            n_seqs * n_q_heads * 8 * (head_dim + 4) * 4,
            MemKind::Device,
            Pool::Weights,
        )
        .unwrap();
    kernels
        .attn_decode_f16(
            &ob, &parts, &qb, &kb, &vb, &ptb, &slb, n_seqs, n_q_heads, n_kv_heads, head_dim,
            page_size, max_pages, scale, 0, &stream,
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
                let dot: f32 = (0..head_dim).map(|e| q[qb_off + e] * kc[kv_off + e]).sum();
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
    let tiny = dev.alloc(4, MemKind::Device, Pool::Activations).unwrap();

    assert!(kernels
        .attn_decode_split_gqa4_f16_hd128(
            &tiny, &tiny, 0, &tiny, 0, &tiny, 0, &tiny, &tiny, &tiny, &tiny, &tiny, 1, 4, 1, 1, 1,
            1, 1e-5, 10_000.0, 0.125, &stream,
        )
        .is_err());
    assert!(kernels
        .attn_decode_combine_gqa2_f16_hd128(&tiny, &tiny, usize::MAX, 4, 1, &stream,)
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
        .gemv_q4_k_dp4a_out_f32(&y32, 0, &wb, &xb, 0, rows, cols, &stream)
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

// Weight-stationary small-batch dp4a GEMV (T=2/4/8/16): every token's row
// dot must match the CPU dequant reference within the q8_1 activation
// tolerance, for both Q4_K (min term) and Q6_K.
#[test]
fn gemm_qk_dp4a_batch_matches_formats_dequant() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for (quant, q6) in [(QuantKind::Q4K, false), (QuantKind::Q6K, true)] {
        let (rows, cols) = (33usize, 512usize);
        let wq = if q6 {
            build_q6k(rows, cols)
        } else {
            build_q4k(rows, cols)
        };
        let w_f32 =
            forge_formats::dequant::dequantize_to_f32(DType::F32, quant, &wq, rows * cols).unwrap();
        let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
        dev.write(&wq, &wb, 0).unwrap();

        for n_tokens in [2usize, 4, 8, 16] {
            let x: Vec<f32> = (0..n_tokens * cols)
                .map(|i| f16::from_f32(fill(i) * 0.1).to_f32())
                .collect();
            let xb = upload_f16(dev.as_ref(), &x);
            let yb = upload_f16(dev.as_ref(), &vec![0.0; n_tokens * rows]);
            let routed = kernels
                .gemm_qk_dp4a_batch_at(&yb, &wb, 0, &xb, rows, cols, n_tokens, q6, &stream)
                .unwrap();
            if !routed {
                // Kernel istnieje dla fali 32; karta bez niego jest pomijana.
                eprintln!("pomijam batch dp4a dla T={n_tokens}: brak kernela");
                continue;
            }
            dev.synchronize().unwrap();

            // Aggregate relative L2 vs the CPU dequant reference — the same
            // metric the i8mma/MMQ goldens use; the q8_1 activation quant
            // (and the quantized-sum min term) is per-element lossy.
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
            assert!(
                rel_l2 < 5e-3,
                "{quant:?} T={n_tokens}: relL2 {rel_l2:.3e} exceeds q8_1 tolerance"
            );
        }
    }
}

// GPU GGUF→e4m3 pack must be BIT-identical to the CPU reference (row absmax
// / 448 scale + f32_to_f8e4m3 codes over the same dequantized values).
#[test]
fn pack_gguf_fp8_matches_cpu_pack() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    for quant in [QuantKind::Q4K, QuantKind::Q6K, QuantKind::Q8_0] {
        let (rows, cols) = (33usize, 512usize);
        let wq = match quant {
            QuantKind::Q4K => build_q4k(rows, cols),
            QuantKind::Q6K => build_q6k(rows, cols),
            _ => build_q8_0(rows, cols),
        };
        let w_f32 =
            forge_formats::dequant::dequantize_to_f32(DType::F32, quant, &wq, rows * cols).unwrap();
        // CPU reference pack.
        let mut want_codes = vec![0u8; rows * cols];
        let mut want_scales = vec![0f32; rows];
        for r in 0..rows {
            let row = &w_f32[r * cols..(r + 1) * cols];
            let absmax = row.iter().fold(0f32, |m, &x| m.max(x.abs()));
            if absmax == 0.0 {
                continue;
            }
            want_scales[r] = absmax / 448.0;
            let inv = 448.0 / absmax;
            for (c, &x) in row.iter().enumerate() {
                want_codes[r * cols + c] = forge_formats::nvfp4::f32_to_f8e4m3(x * inv);
            }
        }

        let wb = dev.alloc(wq.len(), MemKind::Device, Pool::Weights).unwrap();
        dev.write(&wq, &wb, 0).unwrap();
        let codes = dev
            .alloc(rows * cols, MemKind::Device, Pool::Weights)
            .unwrap();
        let scales = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
        kernels
            .pack_gguf_fp8(&codes, &scales, &wb, 0, rows, cols, quant, &stream)
            .unwrap();
        dev.synchronize().unwrap();

        let mut got_codes = vec![0u8; rows * cols];
        dev.read(&codes, 0, &mut got_codes).unwrap();
        let mut sb = vec![0u8; rows * 4];
        dev.read(&scales, 0, &mut sb).unwrap();
        let got_scales: Vec<f32> = sb
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for r in 0..rows {
            assert_eq!(
                got_scales[r].to_bits(),
                want_scales[r].to_bits(),
                "{quant:?} scale row {r}: got {} want {}",
                got_scales[r],
                want_scales[r]
            );
        }
        let bad = got_codes
            .iter()
            .zip(&want_codes)
            .filter(|(g, w)| g != w)
            .count();
        assert_eq!(
            bad, 0,
            "{quant:?}: {bad} e4m3 codes differ from the CPU pack"
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
                    let (doff, moff) = if quant == QuantKind::Q5K {
                        (0, 2)
                    } else {
                        (80, 82)
                    };
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
                | QuantKind::IQ1S => block[0..2].copy_from_slice(&d.to_le_bytes()),
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

/// Kernel nieskompilowany dla tej architektury (lista `unsupported.txt`) to
/// brak wsparcia backendu, nie błąd numeryczny — raportujemy i pomijamy.
/// Rozjazd wartości nadal jest błędem.
fn skip_if_absent(e: &forge_types::ForgeError, what: &str) -> bool {
    let msg = e.to_string();
    if msg.contains("kernel not loaded") || msg.contains("artifact") {
        eprintln!("pomijam {what}: {msg}");
        return true;
    }
    false
}

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

    if let Err(e) = gemv(&kernels, &yb, &wb, &xb, rows, cols, &stream) {
        if skip_if_absent(&e, "gemv") {
            return;
        }
        panic!("gemv: {e}");
    }
    dev.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &yb, rows);
    for r in 0..rows {
        let want: f32 = (0..cols).map(|c| w_f32[r * cols + c] * x[c]).sum();
        let rel = (got[r] - want).abs() / (want.abs() + 1.0);
        assert!(rel < 0.02, "{quant:?} row {r}: got {} want {want}", got[r]);
    }

    let y32 = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
    if let Err(e) = out32(&kernels, &y32, &wb, &xb, rows, cols, &stream) {
        if skip_if_absent(&e, "gemv out_f32") {
            return;
        }
        panic!("gemv out_f32: {e}");
    }
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
        assert!(
            rel < 0.02,
            "{quant:?} f32 row {r}: got {} want {want}",
            got32[r]
        );
    }
}

/// Odtwarza kwantyzacje aktywacji q8_1 (skala na 32 kolumny), ktora ścieżka
/// int8 wykonuje przed batchowym GEMM na kartach bez jednostki macierzowej.
fn quantize_act_q8_1_host(x: &[f32], cols: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; x.len()];
    for (t, row) in x.chunks_exact(cols).enumerate() {
        for b in 0..cols / 32 {
            let blk = &row[b * 32..(b + 1) * 32];
            let amax = blk.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            if amax == 0.0 {
                continue;
            }
            let d = amax / 127.0;
            for (i, v) in blk.iter().enumerate() {
                out[t * cols + b * 32 + i] = (v / d).round().clamp(-127.0, 127.0) * d;
            }
        }
    }
    out
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

    if let Err(e) = gemm(&kernels, &yb, &wb, &xb, rows, cols, n_tokens, &stream) {
        if skip_if_absent(&e, "gemm") {
            return;
        }
        panic!("gemm: {e}");
    }
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n_tokens * rows);
    for t in 0..n_tokens {
        for r in 0..rows {
            let want: f32 = (0..cols)
                .map(|c| w_f32[r * cols + c] * x[t * cols + c])
                .sum();
            let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
            // Referencja to dokładna dekwantyzacja wagi razy aktywacja f32.
            // Ścieżki batch bez jednostki macierzowej (AMD, `v_dot4_i32_i8`)
            // kwantyzują aktywację do int8 per grupa 32, co samo wnosi kilka
            // procent błędu — dlatego batch ma luźniejszy próg niż GEMV.
            assert!(
                rel < 0.05,
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
    let cases = [
        ("q", 1usize),
        ("gate", 4),
        ("up", 6),
        ("o", 8),
        ("down", 10),
    ];
    for (label, offset) in cases {
        let rows = 2usize;
        let qweight = dev
            .alloc(rows * cols, MemKind::Device, Pool::Weights)
            .unwrap();
        let scales = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
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
        let logits = dev.alloc(rows * 4, MemKind::Device, Pool::Weights).unwrap();
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
                let weight =
                    forge_formats::nvfp4::f8e4m3_to_f32(codes[row * cols + col]) * row_scales[row];
                expected_gemm +=
                    weight * forge_formats::nvfp4::f8e4m3_to_f32(x_codes[col]) * x_scale;
                expected_gemv += weight * x[col];
            }
            let gemm_rel = (gemm[row] - expected_gemm).abs() / (expected_gemm.abs() + 1.0);
            let gemv_rel = (gemv[row] - expected_gemv).abs() / (expected_gemv.abs() + 1.0);
            assert!(
                gemm_rel < 0.03,
                "{label} GEMM: {} != {expected_gemm}",
                gemm[row]
            );
            assert!(
                gemv_rel < 0.03,
                "{label} GEMV: {} != {expected_gemv}",
                gemv[row]
            );
        }
    }
}
