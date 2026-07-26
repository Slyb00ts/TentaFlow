// ===== File: gemma_attn.rs — golden tests uwagi dla geometrii Gemmy 4 =====
//
// Warstwy globalne mają head_dim 512 i jedną głowicę KV na 16 głowic Q, a
// warstwy okienne 256 z oknem 1024. Te kształty pojawiły się dopiero z Gemmą,
// więc mają tu własną referencję CPU.

use forge_hal::{DevBuffer, Device, Pool};
use forge_types::MemKind;
use forge_kernels::Kernels;
use forge_types::DType;
use half::f16;
use std::sync::Arc;

/// Test MUSI działać na realnym urządzeniu — brak GPU to błąd, nie pominięcie
/// (ciche `return` w starszych testach maskowało brak pokrycia na AMD).
fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open_default_pools(0).expect("GPU wymagane")
}

fn upload(dev: &dyn Device, v: &[f32]) -> DevBuffer {
    let h: Vec<f16> = v.iter().map(|&x| f16::from_f32(x)).collect();
    let bytes =
        unsafe { std::slice::from_raw_parts(h.as_ptr() as *const u8, std::mem::size_of_val(&h[..])) };
    let buf = dev.alloc(bytes.len(), MemKind::Device, Pool::Activations).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    (0..n)
        .map(|i| f16::from_bits(u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]])).to_f32())
        .collect()
}

/// Referencja: uwaga przyczynowa z opcjonalnym oknem, układ stron KV
/// `[page][head][pos_w_stronie][dim]`, Q `[token][head][dim]`.
#[allow(clippy::too_many_arguments)]
fn reference(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    n_tokens: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    head_dim: usize,
    page_size: usize,
    ctx_len: usize,
    scale: f32,
    window: usize,
) -> Vec<f32> {
    let mut out = vec![0.0f32; n_tokens * n_q_heads * head_dim];
    let per_kv = n_q_heads / n_kv_heads;
    let kv_at = |cache: &[f32], pos: usize, h: usize, d: usize| -> f32 {
        let page = pos / page_size;
        let slot = pos % page_size;
        cache[((page * n_kv_heads + h) * page_size + slot) * head_dim + d]
    };
    for t in 0..n_tokens {
        let abs = ctx_len - n_tokens + t;
        let lo = if window > 0 && abs + 1 > window {
            abs + 1 - window
        } else {
            0
        };
        for h in 0..n_q_heads {
            let kvh = h / per_kv;
            let mut scores = Vec::new();
            for pos in lo..=abs {
                let mut dot = 0.0f32;
                for d in 0..head_dim {
                    dot += q[(t * n_q_heads + h) * head_dim + d] * kv_at(k, pos, kvh, d);
                }
                scores.push(dot * scale);
            }
            let m = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let exps: Vec<f32> = scores.iter().map(|s| (s - m).exp()).collect();
            let sum: f32 = exps.iter().sum();
            for d in 0..head_dim {
                let mut acc = 0.0f32;
                for (i, pos) in (lo..=abs).enumerate() {
                    acc += exps[i] * kv_at(v, pos, kvh, d);
                }
                out[(t * n_q_heads + h) * head_dim + d] = acc / sum;
            }
        }
    }
    out
}

fn fill(i: usize) -> f32 {
    (((i * 2654435761) % 1000) as f32 / 500.0) - 1.0
}

#[test]
fn attn_prefill_matches_reference_for_gemma_shapes() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let page_size = 32usize;

    // (head_dim, n_q_heads, n_kv_heads, window) — globalna i okienna warstwa.
    for &(head_dim, n_q, n_kv, window) in &[(512usize, 16usize, 1usize, 0usize), (256, 16, 8, 1024), (256, 4, 2, 3)] {
        let n_tokens = 20usize;
        let pages = n_tokens.div_ceil(page_size);
        let ctx = pages * page_size;
        let q: Vec<f32> = (0..n_tokens * n_q * head_dim).map(fill).collect();
        let k: Vec<f32> = (0..ctx * n_kv * head_dim).map(|i| fill(i + 7)).collect();
        let v: Vec<f32> = (0..ctx * n_kv * head_dim).map(|i| fill(i + 13)).collect();
        let pt: Vec<i32> = (0..pages as i32).collect();

        let qb = upload(dev.as_ref(), &q);
        let kb = upload(dev.as_ref(), &k);
        let vb = upload(dev.as_ref(), &v);
        let ob = upload(dev.as_ref(), &vec![0.0; n_tokens * n_q * head_dim]);
        let ptb = {
            let bytes: Vec<u8> = pt.iter().flat_map(|p| p.to_le_bytes()).collect();
            let buf = dev
                .alloc(bytes.len(), MemKind::Device, Pool::Activations)
                .unwrap();
            dev.write(&bytes, &buf, 0).unwrap();
            buf
        };

        kernels
            .attn_prefill(
                &ob, &qb, &kb, &vb, &ptb, 0, n_tokens, n_q, n_kv, head_dim, page_size,
                DType::F16, 1.0, window, &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();

        let got = download(dev.as_ref(), &ob, n_tokens * n_q * head_dim);
        let want = reference(
            &q, &k, &v, n_tokens, n_q, n_kv, head_dim, page_size, n_tokens, 1.0, window,
        );
        let err = got
            .iter()
            .zip(&want)
            .map(|(g, w)| (g - w).abs())
            .fold(0.0f32, f32::max);
        assert!(
            err < 0.02,
            "hd={head_dim} n_q={n_q} n_kv={n_kv} window={window} max_err={err}"
        );
    }
}

/// Buduje wagę Q4_0 [rows, cols]: bloki 32 wartości (f16 skala + 16 bajtów nibbli).
fn build_q4_0(rows: usize, cols: usize) -> (Vec<u8>, Vec<f32>) {
    let mut raw = Vec::new();
    let mut deq = vec![0.0f32; rows * cols];
    for r in 0..rows {
        for b in 0..cols / 32 {
            let d = f16::from_f32(0.02 + ((r + b) % 7) as f32 * 0.005);
            raw.extend_from_slice(&d.to_bits().to_le_bytes());
            for j in 0..16 {
                let lo = ((r * 31 + b * 7 + j) % 16) as u8;
                let hi = ((r * 13 + b * 5 + j * 3) % 16) as u8;
                raw.push(lo | (hi << 4));
                deq[r * cols + b * 32 + j] = (lo as f32 - 8.0) * d.to_f32();
                deq[r * cols + b * 32 + 16 + j] = (hi as f32 - 8.0) * d.to_f32();
            }
        }
    }
    (raw, deq)
}

/// Kształty projekcji Gemmy 4 12B. Ścieżka int8 (`v_dot4_i32_i8`) była pokryta
/// tylko małymi kształtami, a projekcja `down` ma cols=15360.
#[test]
fn gemm_q4_0_matches_cpu_for_gemma_projections() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    // (rows_out, cols_in): attn_o, gate/up, down, q i k warstwy globalnej.
    for &(rows, cols) in &[
        (3840usize, 4096usize),
        (15360, 3840),
        (3840, 15360),
        (8192, 3840),
        (512, 3840),
    ] {
        let n_tokens = 11usize;
        let (raw, deq) = build_q4_0(rows, cols);
        let x: Vec<f32> = (0..n_tokens * cols).map(|i| fill(i) * 0.1).collect();

        let wb = dev.alloc(raw.len(), MemKind::Device, Pool::Weights).unwrap();
        dev.write(&raw, &wb, 0).unwrap();
        let xb = upload(dev.as_ref(), &x);
        let yb = upload(dev.as_ref(), &vec![0.0; n_tokens * rows]);

        kernels
            .gemm_q4_0_f16_at(&yb, &wb, 0, &xb, rows, cols, n_tokens, &stream)
            .unwrap();
        dev.synchronize().unwrap();
        let got = download(dev.as_ref(), &yb, n_tokens * rows);

        // Kilka wierszy na token wystarcza, a pełne 3840x11 na CPU byłoby wolne.
        for t in [0usize, 1, n_tokens - 1] {
            for r in [0usize, 1, 2, rows / 2, rows - 1] {
                let want: f32 = (0..cols)
                    .map(|c| deq[r * cols + c] * x[t * cols + c])
                    .sum();
                let rel = (got[t * rows + r] - want).abs() / (want.abs() + 1.0);
                assert!(
                    rel < 0.03,
                    "rows={rows} cols={cols} t={t} r={r}: got {} want {want}",
                    got[t * rows + r]
                );
            }
        }
    }
}
