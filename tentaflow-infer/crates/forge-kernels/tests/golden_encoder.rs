// ===== File: golden_encoder.rs — GPU vs CPU references for the encoder kernel set =====
// Covers the kernels the Whisper engine depends on: layernorm (+fused
// residual), gelu, conv1d_k3, gemv_f16_bias (incl. the `_at` offset variants),
// attn_full (bidirectional and causal with q_offset) and gather_rows. Skips
// cleanly without a CUDA device.

use std::sync::Arc;

use forge_hal::{PoolSizes, gpu};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const EPS: f32 = 1e-5;

fn device() -> Option<Arc<dyn Device>> {
    match gpu::open(
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
    ((i * 37 % 19) as f32 - 9.0) * 0.05
}

fn upload_f16(dev: &dyn Device, vals: &[f32]) -> DevBuffer {
    let host: Vec<f16> = vals.iter().map(|&v| f16::from_f32(v)).collect();
    let bytes: Vec<u8> = bytemuck::cast_slice(&host).to_vec();
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(&bytes, &buf, 0).unwrap();
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

fn max_abs_err(got: &[f32], want: &[f32]) -> f32 {
    got.iter()
        .zip(want)
        .map(|(g, w)| (g - w).abs())
        .fold(0.0, f32::max)
}

fn quantize(vals: &[f32]) -> Vec<f32> {
    vals.iter().map(|&v| f16::from_f32(v).to_f32()).collect()
}

fn gelu_ref(v: f32) -> f32 {
    // The kernel computes exact-erf GELU; std lacks erf, and at f16 test
    // tolerances (1e-2) the tanh approximation is within 3e-4 of exact GELU
    // on [-8, 8], so it serves as the reference.
    let t = 0.797_884_56_f64 * (f64::from(v) + 0.044_715 * f64::from(v).powi(3));
    (0.5 * f64::from(v) * (1.0 + t.tanh())) as f32
}

fn layernorm_ref(x: &[f32], w: &[f32], b: &[f32], rows: usize, cols: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; rows * cols];
    for r in 0..rows {
        let row = &x[r * cols..(r + 1) * cols];
        let mean = row.iter().sum::<f32>() / cols as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / cols as f32;
        let inv = 1.0 / (var + EPS).sqrt();
        for c in 0..cols {
            out[r * cols + c] = (row[c] - mean) * inv * w[c] + b[c];
        }
    }
    out
}

#[test]
fn layernorm_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (5usize, 512usize);
    let x = quantize(&(0..rows * cols).map(fill).collect::<Vec<_>>());
    let w = quantize(
        &(0..cols)
            .map(|i| 0.8 + (i % 7) as f32 * 0.05)
            .collect::<Vec<_>>(),
    );
    let b = quantize(&(0..cols).map(|i| fill(i) * 0.2).collect::<Vec<_>>());

    let xb = upload_f16(dev.as_ref(), &x);
    let wb = upload_f16(dev.as_ref(), &w);
    let bb = upload_f16(dev.as_ref(), &b);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows * cols]);
    kernels
        .layernorm_f16(&yb, &xb, &wb, &bb, rows, cols, EPS, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, rows * cols);
    let want = layernorm_ref(&x, &w, &b, rows, cols);
    assert!(max_abs_err(&got, &want) < 0.01);
}

#[test]
fn layernorm_residual_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (3usize, 512usize);
    let res = quantize(&(0..rows * cols).map(fill).collect::<Vec<_>>());
    let x = quantize(
        &(0..rows * cols)
            .map(|i| fill(i + 11) * 0.5)
            .collect::<Vec<_>>(),
    );
    let w = quantize(
        &(0..cols)
            .map(|i| 1.0 + (i % 3) as f32 * 0.1)
            .collect::<Vec<_>>(),
    );
    let b = quantize(&(0..cols).map(|i| fill(i) * 0.1).collect::<Vec<_>>());

    let rb = upload_f16(dev.as_ref(), &res);
    let xb = upload_f16(dev.as_ref(), &x);
    let wb = upload_f16(dev.as_ref(), &w);
    let bb = upload_f16(dev.as_ref(), &b);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows * cols]);
    kernels
        .layernorm_residual_f16(&yb, &rb, &xb, &wb, &bb, rows, cols, EPS, &stream)
        .unwrap();
    dev.synchronize().unwrap();

    // residual is updated in place (f16-rounded), then normalized.
    let sum = quantize(&res.iter().zip(&x).map(|(r, v)| r + v).collect::<Vec<_>>());
    let want = layernorm_ref(&sum, &w, &b, rows, cols);
    let got = download_f16(dev.as_ref(), &yb, rows * cols);
    assert!(max_abs_err(&got, &want) < 0.01);
    let got_res = download_f16(dev.as_ref(), &rb, rows * cols);
    assert!(max_abs_err(&got_res, &sum) < 0.01);
}

#[test]
fn gelu_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let n = 4096usize;
    let x = quantize(&(0..n).map(|i| fill(i) * 4.0).collect::<Vec<_>>());
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; n]);
    kernels.gelu_f16(&yb, &xb, n, &stream).unwrap();
    dev.synchronize().unwrap();

    let got = download_f16(dev.as_ref(), &yb, n);
    let want: Vec<f32> = x.iter().map(|&v| gelu_ref(v)).collect();
    assert!(max_abs_err(&got, &want) < 0.01);
}

#[allow(clippy::too_many_arguments)]
fn conv1d_ref(
    x: &[f32],
    w: &[f32],
    b: &[f32],
    in_ch: usize,
    out_ch: usize,
    in_t: usize,
    out_t: usize,
    stride: usize,
    apply_gelu: bool,
) -> Vec<f32> {
    let mut out = vec![0.0f32; out_ch * out_t];
    for oc in 0..out_ch {
        for t in 0..out_t {
            let center = (t * stride) as isize;
            let mut acc = b[oc];
            for ic in 0..in_ch {
                for k in 0..3usize {
                    let src = center + k as isize - 1;
                    if src >= 0 && (src as usize) < in_t {
                        acc += w[(oc * in_ch + ic) * 3 + k] * x[ic * in_t + src as usize];
                    }
                }
            }
            out[oc * out_t + t] = if apply_gelu { gelu_ref(acc) } else { acc };
        }
    }
    out
}

#[test]
fn conv1d_k3_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (in_ch, out_ch, in_t) = (8usize, 6usize, 40usize);
    let x = quantize(&(0..in_ch * in_t).map(fill).collect::<Vec<_>>());
    let w = quantize(
        &(0..out_ch * in_ch * 3)
            .map(|i| fill(i + 5) * 0.3)
            .collect::<Vec<_>>(),
    );
    let b = quantize(&(0..out_ch).map(|i| fill(i) * 0.2).collect::<Vec<_>>());

    let xb = upload_f16(dev.as_ref(), &x);
    let wb = upload_f16(dev.as_ref(), &w);
    let bb = upload_f16(dev.as_ref(), &b);

    for (stride, out_t, apply_gelu) in [(1usize, in_t, false), (2usize, in_t / 2, true)] {
        let yb = upload_f16(dev.as_ref(), &vec![0.0; out_ch * out_t]);
        kernels
            .conv1d_k3_f16(
                &yb, &xb, &wb, &bb, in_ch, out_ch, in_t, out_t, stride, apply_gelu, &stream,
            )
            .unwrap();
        dev.synchronize().unwrap();
        let got = download_f16(dev.as_ref(), &yb, out_ch * out_t);
        let want = conv1d_ref(&x, &w, &b, in_ch, out_ch, in_t, out_t, stride, apply_gelu);
        assert!(
            max_abs_err(&got, &want) < 0.02,
            "conv stride {stride} gelu {apply_gelu}"
        );
    }
}

#[test]
fn gemv_bias_and_offset_variants_match_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (rows, cols) = (64usize, 128usize);
    let w = quantize(&(0..rows * cols).map(|i| fill(i) * 0.2).collect::<Vec<_>>());
    let x = quantize(&(0..cols).map(|i| fill(i + 3)).collect::<Vec<_>>());
    let b = quantize(&(0..rows).map(|i| fill(i) * 0.5).collect::<Vec<_>>());

    let mut want = vec![0.0f32; rows];
    for r in 0..rows {
        want[r] = b[r]
            + w[r * cols..(r + 1) * cols]
                .iter()
                .zip(&x)
                .map(|(a, c)| a * c)
                .sum::<f32>();
    }

    let wb = upload_f16(dev.as_ref(), &w);
    let bb = upload_f16(dev.as_ref(), &b);
    let xb = upload_f16(dev.as_ref(), &x);
    let yb = upload_f16(dev.as_ref(), &vec![0.0; rows]);
    kernels
        .gemv_f16_bias(&yb, &wb, &xb, &bb, rows, cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    assert!(max_abs_err(&download_f16(dev.as_ref(), &yb, rows), &want) < 0.02);

    // Offset variants: x embedded at position 2 of a 4-slot buffer, y written
    // to slot 1 of a 3-slot output.
    let mut x_big = vec![0.0f32; cols * 4];
    x_big[cols * 2..cols * 3].copy_from_slice(&x);
    let xbig = upload_f16(dev.as_ref(), &x_big);
    let ybig = upload_f16(dev.as_ref(), &vec![0.0; rows * 3]);
    kernels
        .gemv_f16_bias_at(
            &ybig,
            rows * 2,
            &wb,
            &xbig,
            cols * 2 * 2,
            &bb,
            rows,
            cols,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &ybig, rows * 3);
    assert!(max_abs_err(&got[rows..2 * rows], &want) < 0.02);
    assert!(got[..rows].iter().all(|&v| v == 0.0));

    let want_nobias: Vec<f32> = want.iter().zip(&b).map(|(v, bi)| v - bi).collect();
    let ybig2 = upload_f16(dev.as_ref(), &vec![0.0; rows * 3]);
    kernels
        .gemv_f16_at(
            &ybig2,
            rows * 2,
            &wb,
            &xbig,
            cols * 2 * 2,
            rows,
            cols,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let got2 = download_f16(dev.as_ref(), &ybig2, rows * 3);
    assert!(max_abs_err(&got2[rows..2 * rows], &want_nobias) < 0.02);
}

#[allow(clippy::too_many_arguments)]
fn attn_full_ref(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    n_q: usize,
    heads: usize,
    head_dim: usize,
    n_kv: usize,
    causal: bool,
    q_offset: usize,
    scale: f32,
) -> Vec<f32> {
    let mut out = vec![0.0f32; n_q * heads * head_dim];
    for t in 0..n_q {
        let limit = if causal {
            (q_offset + t + 1).min(n_kv)
        } else {
            n_kv
        };
        for h in 0..heads {
            let qv = &q[(t * heads + h) * head_dim..(t * heads + h + 1) * head_dim];
            let mut scores = vec![0.0f32; limit];
            for (p, s) in scores.iter_mut().enumerate() {
                let kv = &k[(p * heads + h) * head_dim..(p * heads + h + 1) * head_dim];
                *s = qv.iter().zip(kv).map(|(a, b)| a * b).sum::<f32>() * scale;
            }
            let m = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let exp: Vec<f32> = scores.iter().map(|s| (s - m).exp()).collect();
            let denom: f32 = exp.iter().sum();
            let o = &mut out[(t * heads + h) * head_dim..(t * heads + h + 1) * head_dim];
            for (p, &e) in exp.iter().enumerate() {
                let vv = &v[(p * heads + h) * head_dim..(p * heads + h + 1) * head_dim];
                for (oi, &vi) in o.iter_mut().zip(vv) {
                    *oi += e / denom * vi;
                }
            }
        }
    }
    out
}

#[test]
fn attn_full_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (heads, head_dim, n_kv) = (4usize, 64usize, 37usize);
    let scale = 1.0 / (head_dim as f32).sqrt();
    let k = quantize(&(0..n_kv * heads * head_dim).map(fill).collect::<Vec<_>>());
    let v = quantize(
        &(0..n_kv * heads * head_dim)
            .map(|i| fill(i + 7))
            .collect::<Vec<_>>(),
    );
    let kb = upload_f16(dev.as_ref(), &k);
    let vb = upload_f16(dev.as_ref(), &v);

    // Bidirectional, multiple query rows (encoder case).
    let n_q = n_kv;
    let q = quantize(
        &(0..n_q * heads * head_dim)
            .map(|i| fill(i + 3))
            .collect::<Vec<_>>(),
    );
    let qb = upload_f16(dev.as_ref(), &q);
    let ob = upload_f16(dev.as_ref(), &vec![0.0; n_q * heads * head_dim]);
    kernels
        .attn_full_f16(
            &ob, &qb, &kb, &vb, n_q, heads, heads, head_dim, n_kv, false, 0, scale, &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &ob, n_q * heads * head_dim);
    let want = attn_full_ref(&q, &k, &v, n_q, heads, head_dim, n_kv, false, 0, scale);
    assert!(max_abs_err(&got, &want) < 0.02, "bidirectional");

    // Single-query causal with q_offset (decode case): only the first
    // q_offset+1 keys are admitted.
    let q1 = quantize(
        &(0..heads * head_dim)
            .map(|i| fill(i + 9))
            .collect::<Vec<_>>(),
    );
    let q1b = upload_f16(dev.as_ref(), &q1);
    let o1b = upload_f16(dev.as_ref(), &vec![0.0; heads * head_dim]);
    let q_offset = 20usize;
    kernels
        .attn_full_f16(
            &o1b,
            &q1b,
            &kb,
            &vb,
            1,
            heads,
            heads,
            head_dim,
            q_offset + 1,
            true,
            q_offset,
            scale,
            &stream,
        )
        .unwrap();
    dev.synchronize().unwrap();
    let got1 = download_f16(dev.as_ref(), &o1b, heads * head_dim);
    let want1 = attn_full_ref(
        &q1,
        &k,
        &v,
        1,
        heads,
        head_dim,
        q_offset + 1,
        true,
        q_offset,
        scale,
    );
    assert!(max_abs_err(&got1, &want1) < 0.02, "causal decode");
}

#[test]
fn gather_rows_matches_reference() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();

    let (n_rows, cols) = (10usize, 512usize);
    let table = quantize(&(0..n_rows * cols).map(fill).collect::<Vec<_>>());
    let tb = upload_f16(dev.as_ref(), &table);
    let ids: Vec<i32> = vec![7, 0, 3];
    let idb = dev
        .alloc(ids.len() * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    dev.write(bytemuck::cast_slice(&ids), &idb, 0).unwrap();
    let ob = upload_f16(dev.as_ref(), &vec![0.0; ids.len() * cols]);
    kernels
        .gather_rows_f16(&ob, &tb, &idb, ids.len(), cols, &stream)
        .unwrap();
    dev.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &ob, ids.len() * cols);
    for (i, &id) in ids.iter().enumerate() {
        let want = &table[id as usize * cols..(id as usize + 1) * cols];
        assert!(max_abs_err(&got[i * cols..(i + 1) * cols], want) < 1e-6);
    }
}
