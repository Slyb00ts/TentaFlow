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
