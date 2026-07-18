// ===== File: rotkv.rs — rotational low-bit KV kernels vs CPU reference attention =====
// Drives the rot4/rot3 launchers (kv_pack_rot_from_cache + attn_decode_rot)
// through the real Rust launch path: packs a paged f16 K/V region into the
// rotational store and runs decode attention over it, comparing the output to
// an f64 softmax-attention reference over the same K/V. Locks the Rust↔PTX
// wiring and the rotational math end to end. Skips cleanly with no CUDA device.

use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const HD: usize = 128;
const PAGE: usize = 32;
const NPAGES: usize = 8;
const CTX: usize = 200; // tokens (fits NPAGES*PAGE = 256)

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
            eprintln!("skipping rotkv tests: {e}");
            None
        }
    }
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev.alloc(bytes.len(), MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

// Deterministic pseudo-random with heavy outliers (the regime rotation fixes).
fn synth(seed: u64, i: usize, outlier: bool) -> f32 {
    let mut s = seed.wrapping_add(i as u64).wrapping_mul(6364136223846793005).wrapping_add(1);
    s ^= s >> 33;
    let u = (s >> 11) as f64 / (1u64 << 53) as f64;
    let v = (u - 0.5) * 4.0;
    if outlier && s.is_multiple_of(17) {
        (v * 8.0) as f32
    } else {
        v as f32
    }
}

fn run_bits(kernels: &Kernels, dev: &dyn Device, bits: u8) -> f64 {
    let stream = dev.create_stream().unwrap();
    let slots = NPAGES * PAGE; // n_kv_heads == 1
    let mut k = vec![0f32; slots * HD];
    let mut v = vec![0f32; slots * HD];
    for t in 0..CTX {
        for i in 0..HD {
            k[t * HD + i] = synth(1, t * HD + i, true);
            v[t * HD + i] = synth(2, t * HD + i, false);
        }
    }
    let q: Vec<f32> = (0..HD).map(|i| synth(3, i, false) * 0.5).collect();

    let k_cache = upload_f16(dev, &k);
    let v_cache = upload_f16(dev, &v);
    let q_buf = upload_f16(dev, &q);
    let out = dev.alloc(HD * 2, MemKind::Device, Pool::Weights).unwrap();

    let page_table: Vec<i32> = (0..NPAGES as i32).collect();
    let pt = dev.alloc(NPAGES * 4, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&page_table), &pt, 0).unwrap();
    let seq_lens = dev.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    dev.write(bytemuck::cast_slice(&[CTX as i32]), &seq_lens, 0).unwrap();

    let pb = HD * bits as usize / 8;
    let k_packed = dev.alloc(slots * pb, MemKind::Device, Pool::Weights).unwrap();
    let v_packed = dev.alloc(slots * pb, MemKind::Device, Pool::Weights).unwrap();
    let k_scale = dev.alloc(slots * 2, MemKind::Device, Pool::Weights).unwrap();
    let v_scale = dev.alloc(slots * 2, MemKind::Device, Pool::Weights).unwrap();

    kernels
        .kv_pack_rot_from_cache(
            &k_packed, &v_packed, &k_scale, &v_scale, &k_cache, &v_cache, &pt, 0, CTX, 1, PAGE, HD,
            bits, &stream,
        )
        .unwrap();
    let scale = 1.0 / (HD as f32).sqrt();
    kernels
        .attn_decode_rot(
            &out, &q_buf, 0, &k_packed, &v_packed, &k_scale, &v_scale, &pt, &seq_lens, 1, 1, 1, HD,
            PAGE, NPAGES, bits, scale, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();

    let mut ob = vec![0u8; HD * 2];
    dev.read(&out, 0, &mut ob).unwrap();
    let got: Vec<f64> = ob
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32() as f64)
        .collect();

    // f64 reference softmax attention over the same (f16-rounded) K/V.
    let qf: Vec<f64> = q.iter().map(|&x| f16::from_f32(x).to_f32() as f64).collect();
    let mut scores = vec![0f64; CTX];
    let mut m = f64::MIN;
    for (t, sc) in scores.iter_mut().enumerate() {
        let mut d = 0f64;
        for i in 0..HD {
            d += qf[i] * f16::from_f32(k[t * HD + i]).to_f32() as f64;
        }
        d *= scale as f64;
        *sc = d;
        if d > m {
            m = d;
        }
    }
    let mut l = 0f64;
    for sc in scores.iter_mut() {
        *sc = (*sc - m).exp();
        l += *sc;
    }
    let mut refv = vec![0f64; HD];
    for (t, &p) in scores.iter().enumerate() {
        let w = p / l;
        for (i, r) in refv.iter_mut().enumerate() {
            *r += w * f16::from_f32(v[t * HD + i]).to_f32() as f64;
        }
    }
    let dot: f64 = got.iter().zip(&refv).map(|(a, b)| a * b).sum();
    let ng: f64 = got.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nr: f64 = refv.iter().map(|x| x * x).sum::<f64>().sqrt();
    dot / (ng * nr)
}

#[test]
fn rot4_decode_attention_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let cos = run_bits(&kernels, dev.as_ref(), 4);
    println!("rot4 decode-attention cosine vs reference: {cos:.4}");
    assert!(cos > 0.95, "rot4 attention cosine too low: {cos}");
}

#[test]
fn rot3_decode_attention_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let cos = run_bits(&kernels, dev.as_ref(), 3);
    println!("rot3 decode-attention cosine vs reference: {cos:.4}");
    assert!(cos > 0.85, "rot3 attention cosine too low: {cos}");
}
