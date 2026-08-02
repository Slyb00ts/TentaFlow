// ===== File: cpu_ref.rs — host reference encoder, the oracle for every backend =====
//
// A straight f32 implementation of the Whisper audio encoder, deliberately
// written for clarity rather than speed. Its job is to be the thing a GPU
// backend is compared against: PLAN_NAPRAWY §5.1 keeps the CPU path as the
// oracle, and an oracle that shares code with the implementation it checks is
// worth nothing.
//
// Follows the reference formulation exactly, including the two places where a
// plausible-looking variant would be wrong:
//   * attention scales BOTH q and k by (head_dim)^-0.25 rather than dividing
//     the product once — the result is the same in exact arithmetic and is not
//     the same in f16;
//   * GELU is the exact erf form, not the tanh approximation.

use forge_hal::{DevBuffer, Device};
use forge_types::Result;
use half::f16;

use crate::weights::{Attention, LayerNormW, WhisperWeights};

/// Reads an f16 device buffer back to host f32.
fn host(device: &dyn Device, buf: &DevBuffer, count: usize) -> Result<Vec<f32>> {
    let mut bytes = vec![0u8; count * 2];
    device.read(buf, 0, &mut bytes)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
        .collect())
}

/// `x * (1 + erf(x / sqrt(2))) / 2`. The tanh approximation differs in the
/// third decimal place, which is enough to move a token.
fn gelu(x: f32) -> f32 {
    x * 0.5 * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2))
}

/// Abramowitz–Stegun 7.1.26: max absolute error 1.5e-7, below f32 resolution
/// for the magnitudes seen here.
fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

fn layer_norm(x: &mut [f32], rows: usize, cols: usize, w: &[f32], b: &[f32]) {
    const EPS: f32 = 1e-5;
    for r in 0..rows {
        let row = &mut x[r * cols..(r + 1) * cols];
        let mean = row.iter().sum::<f32>() / cols as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / cols as f32;
        let inv = 1.0 / (var + EPS).sqrt();
        for (c, v) in row.iter_mut().enumerate() {
            *v = (*v - mean) * inv * w[c] + b[c];
        }
    }
}

/// `out[rows, n] = x[rows, k] * w[n, k]^T + bias`, the row-major linear layer.
fn linear(x: &[f32], w: &[f32], b: Option<&[f32]>, rows: usize, k: usize, n: usize) -> Vec<f32> {
    let mut out = vec![0f32; rows * n];
    for r in 0..rows {
        for j in 0..n {
            let mut acc = b.map_or(0.0, |b| b[j]);
            for i in 0..k {
                acc += x[r * k + i] * w[j * k + i];
            }
            out[r * n + j] = acc;
        }
    }
    out
}

/// Conv1d over [time, in_ch] with kernel 3 and padding 1, weights [out, in, k].
fn conv1d_k3(
    x: &[f32],
    w: &[f32],
    b: &[f32],
    t_in: usize,
    in_ch: usize,
    out_ch: usize,
    stride: usize,
) -> Vec<f32> {
    let t_out = t_in.div_ceil(stride);
    let mut out = vec![0f32; t_out * out_ch];
    for (t_o, o_slot) in (0..t_out).enumerate().map(|(i, _)| (i, i)) {
        let center = t_o * stride;
        for oc in 0..out_ch {
            let mut acc = b[oc];
            for kk in 0..3 {
                let t = center as isize + kk as isize - 1;
                if t < 0 || t >= t_in as isize {
                    continue;
                }
                let t = t as usize;
                for ic in 0..in_ch {
                    acc += x[t * in_ch + ic] * w[(oc * in_ch + ic) * 3 + kk];
                }
            }
            out[o_slot * out_ch + oc] = acc;
        }
    }
    out
}

struct HostAttention {
    q_w: Vec<f32>,
    q_b: Vec<f32>,
    k_w: Vec<f32>,
    v_w: Vec<f32>,
    v_b: Vec<f32>,
    o_w: Vec<f32>,
    o_b: Vec<f32>,
}

fn read_attention(device: &dyn Device, a: &Attention, d: usize) -> Result<HostAttention> {
    Ok(HostAttention {
        q_w: host(device, &a.q_w, d * d)?,
        q_b: host(device, &a.q_b, d)?,
        k_w: host(device, &a.k_w, d * d)?,
        v_w: host(device, &a.v_w, d * d)?,
        v_b: host(device, &a.v_b, d)?,
        o_w: host(device, &a.o_w, d * d)?,
        o_b: host(device, &a.o_b, d)?,
    })
}

fn read_norm(device: &dyn Device, n: &LayerNormW, d: usize) -> Result<(Vec<f32>, Vec<f32>)> {
    Ok((host(device, &n.w, d)?, host(device, &n.b, d)?))
}

/// Self-attention without a mask, which is what the audio encoder uses.
fn self_attention(x: &[f32], a: &HostAttention, rows: usize, d: usize, heads: usize) -> Vec<f32> {
    let head_dim = d / heads;
    // Both operands carry the fourth root, exactly as the reference does.
    let scale = (head_dim as f32).powf(-0.25);

    let q = linear(x, &a.q_w, Some(&a.q_b), rows, d, d);
    let k = linear(x, &a.k_w, None, rows, d, d);
    let v = linear(x, &a.v_w, Some(&a.v_b), rows, d, d);

    let mut ctx = vec![0f32; rows * d];
    let mut scores = vec![0f32; rows];
    for h in 0..heads {
        let off = h * head_dim;
        for i in 0..rows {
            for (j, score) in scores.iter_mut().enumerate() {
                let mut acc = 0.0;
                for e in 0..head_dim {
                    acc += (q[i * d + off + e] * scale) * (k[j * d + off + e] * scale);
                }
                *score = acc;
            }
            let max = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0.0;
            for s in scores.iter_mut() {
                *s = (*s - max).exp();
                sum += *s;
            }
            for e in 0..head_dim {
                let mut acc = 0.0;
                for (j, s) in scores.iter().enumerate() {
                    acc += s * v[j * d + off + e];
                }
                ctx[i * d + off + e] = acc / sum;
            }
        }
    }
    linear(&ctx, &a.o_w, Some(&a.o_b), rows, d, d)
}

/// Runs the audio encoder on host, returning `[n_audio_ctx, d_model]` in f32.
///
/// `mel` is `[n_mels, n_audio_ctx * 2]` in row-major channel-major order, the
/// same layout the GPU path writes.
pub fn encode(device: &dyn Device, w: &WhisperWeights, mel: &[f32]) -> Result<Vec<f32>> {
    let cfg = &w.config;
    let d = cfg.d_model;
    let mels = cfg.num_mel_bins;
    let t_enc = cfg.max_source_positions;
    let t_in = t_enc * 2;
    let heads = cfg.encoder_attention_heads;
    let ffn = cfg.encoder_ffn_dim;

    assert_eq!(mel.len(), mels * t_in, "mel ma zły rozmiar");
    // The convolutions consume [time, channels].
    let mut x = vec![0f32; t_in * mels];
    for c in 0..mels {
        for t in 0..t_in {
            x[t * mels + c] = mel[c * t_in + t];
        }
    }

    let conv1_w = host(device, &w.conv1_w, d * mels * 3)?;
    let conv1_b = host(device, &w.conv1_b, d)?;
    let conv2_w = host(device, &w.conv2_w, d * d * 3)?;
    let conv2_b = host(device, &w.conv2_b, d)?;

    let mut h = conv1d_k3(&x, &conv1_w, &conv1_b, t_in, mels, d, 1);
    for v in h.iter_mut() {
        *v = gelu(*v);
    }
    let mut h = conv1d_k3(&h, &conv2_w, &conv2_b, t_in, d, d, 2);
    for v in h.iter_mut() {
        *v = gelu(*v);
    }
    assert_eq!(h.len(), t_enc * d);

    for (i, v) in h.iter_mut().enumerate() {
        *v += w.enc_pos_host[i];
    }

    for layer in &w.enc_layers {
        let (ln_w, ln_b) = read_norm(device, &layer.self_attn_ln, d)?;
        let mut y = h.clone();
        layer_norm(&mut y, t_enc, d, &ln_w, &ln_b);
        let attn = read_attention(device, &layer.self_attn, d)?;
        let a = self_attention(&y, &attn, t_enc, d, heads);
        for (dst, src) in h.iter_mut().zip(&a) {
            *dst += src;
        }

        let (ln_w, ln_b) = read_norm(device, &layer.final_ln, d)?;
        let mut y = h.clone();
        layer_norm(&mut y, t_enc, d, &ln_w, &ln_b);
        let fc1_w = host(device, &layer.fc1_w, ffn * d)?;
        let fc1_b = host(device, &layer.fc1_b, ffn)?;
        let fc2_w = host(device, &layer.fc2_w, d * ffn)?;
        let fc2_b = host(device, &layer.fc2_b, d)?;
        let mut mid = linear(&y, &fc1_w, Some(&fc1_b), t_enc, d, ffn);
        for v in mid.iter_mut() {
            *v = gelu(*v);
        }
        let back = linear(&mid, &fc2_w, Some(&fc2_b), t_enc, ffn, d);
        for (dst, src) in h.iter_mut().zip(&back) {
            *dst += src;
        }
    }

    let (ln_w, ln_b) = read_norm(device, &w.enc_ln, d)?;
    layer_norm(&mut h, t_enc, d, &ln_w, &ln_b);
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gelu_is_the_exact_form_not_the_tanh_approximation() {
        // Reference values from the erf definition.
        assert!((gelu(0.0) - 0.0).abs() < 1e-7);
        assert!((gelu(1.0) - 0.841_345).abs() < 1e-4, "gelu(1) = {}", gelu(1.0));
        assert!((gelu(-1.0) + 0.158_655).abs() < 1e-4);
        assert!((gelu(3.0) - 2.995_95).abs() < 1e-3);
    }

    #[test]
    fn layer_norm_zeroes_mean_and_unit_variance() {
        let mut x = vec![1.0, 2.0, 3.0, 4.0];
        let w = vec![1.0; 4];
        let b = vec![0.0; 4];
        layer_norm(&mut x, 1, 4, &w, &b);
        let mean: f32 = x.iter().sum::<f32>() / 4.0;
        assert!(mean.abs() < 1e-5);
        let var: f32 = x.iter().map(|v| v * v).sum::<f32>() / 4.0;
        assert!((var - 1.0).abs() < 1e-3, "wariancja {var}");
    }

    #[test]
    fn conv1d_pads_with_zeros_and_honours_stride() {
        // in_ch = 1, out_ch = 1, kernel [1, 2, 3] over x = [1, 2, 3, 4].
        let x = vec![1.0, 2.0, 3.0, 4.0];
        let w = vec![1.0, 2.0, 3.0];
        let b = vec![0.0];
        let y = conv1d_k3(&x, &w, &b, 4, 1, 1, 1);
        // pozycja 0: 0*1 + 1*2 + 2*3 = 8
        assert_eq!(y, vec![8.0, 14.0, 20.0, 11.0]);

        let y2 = conv1d_k3(&x, &w, &b, 4, 1, 1, 2);
        assert_eq!(y2, vec![8.0, 20.0]);
    }
}
