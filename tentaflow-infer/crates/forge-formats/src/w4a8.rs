// ===== File: w4a8.rs — Q4_K/fp16 -> QServe W4A8 requant + weight interleave =====
// Produces the buffers QServe's `w4a8_per_group` `dense_kernel0` reads (kernels/
// cuda/w4a8_gemm.cu): int4 weight codes in the 8-D interleave, per-group int8
// secondary scale + (-zero)*s2, per-channel fp16 primary scale. Two-level QoQ
// quant: w ~= s1[row] * int8(s2[row,group] * (q4 - zero)). group_size = 128 (the
// kernel's compile-time G). This host packer is validated byte-exact against a
// CPU int4xint8 golden through the GPU kernel in forge-kernels' `cuda_w4a8` test
// (relL2 ~2e-4 = fp16 noise). Layout mirrors omniserve/.../w4a8_linear.py.
//
// The `zero` this module carries is the packed int8 zero-point `(-zero)*s2`
// (2's complement), a FULL integer — NOT a [0,15] nibble. Clamping it to a nibble
// silently collapses every group that does not straddle 0 (a large fraction of
// real weights), which is what made the earlier requant produce garbage.
//
// NOTE (quality): requantizing an already-Q4_K weight through W4A8 is a double
// quantization; the mandatory per-row stage-1 int8 leaves ~+25% PPL vs the native
// Q4_K path even after the clip search (docs/BENCH_COMPARISON.md). A from-fp16
// requant or weight-side rotation — not activation smoothing — would close it.

use half::f16;

/// The QServe group size the in-tree kernel is compiled for.
pub const W4A8_GROUP: usize = 128;

/// Packed W4A8 weight buffers, ready to upload for `dense_kernel0`.
pub struct W4A8Packed {
    /// int4 codes, 8-D interleave: `[N/32][K/32][32][16]` bytes (`N*K/2`).
    pub qweight: Vec<u8>,
    /// per-group int8 secondary scale, reordered `[K/G][N]` (`i8` as `u8`).
    pub s2_scales: Vec<u8>,
    /// per-group `(-zero)*s2` (2's-complement `i8`), reordered `[K/G][N]`.
    pub s2_zeros: Vec<u8>,
    /// per-channel fp16 primary scale `[N]` (LE f16 bits).
    pub s1_scales: Vec<u16>,
    pub n: usize,
    pub k: usize,
    pub group: usize,
}

/// Given a fixed stage-1 scale `s1`, quantize one row's `k` weights through the
/// two-level QServe scheme (per-channel int8 → per-group asymmetric int4) and
/// return the per-group `(s2, zero, q4)` plus the squared reconstruction error
/// against the original weights. The effective weight is `s1*s2*(q4-zero)`,
/// which the GEMM reproduces byte-for-byte (int8-wrapped s2*(q4-zero)).
fn quantize_row(
    w: &[f32],
    k: usize,
    group: usize,
    s1: f32,
    q4: &mut [u8],
    s2: &mut [u8],
    zero: &mut [u8],
) -> f64 {
    let kg = k / group;
    let mut err = 0.0f64;
    for gi in 0..kg {
        // Stage 1: clip to int8 with the trial per-channel scale.
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        let base = gi * group;
        let mut wi8 = [0i32; 256];
        for j in 0..group {
            let v = ((w[base + j] / s1).round() as i32).clamp(-127, 127);
            wi8[j] = v;
            lo = lo.min(v);
            hi = hi.max(v);
        }
        // Stage 2: asymmetric int4 over the int8 group. The zero-point `zv` is a
        // FULL integer (stored downstream as the int8 `(-zv)*s2`), NOT a nibble:
        // a group that does not straddle zero needs zv well outside [0,15], and
        // clamping it there is what crushed such groups. q4 stays a [0,15] nibble.
        let s2v = ((hi - lo + 14) / 15).clamp(1, 127);
        // zv places lo at nibble 0; keep (-zv)*s2 inside int8 so the packed
        // zero-point byte the kernel reads is exact (|lo| <= 127 => it fits).
        let zv = (-(lo as f32) / s2v as f32).round() as i32;
        let zpt = ((-zv) * s2v).clamp(-127, 127) as i8;
        s2[gi] = s2v as u8;
        zero[gi] = zpt as u8;
        for j in 0..group {
            let q = ((wi8[j] as f32 / s2v as f32).round() as i32 + zv).clamp(0, 15);
            q4[base + j] = q as u8;
            // Kernel-exact reconstruction: int8-wrapped (s2*q4 + zero_point).
            let wrec = (s2v * q + zpt as i32) as i8;
            let recon = s1 * wrec as f32;
            let d = (w[base + j] - recon) as f64;
            err += d * d;
        }
    }
    err
}

/// Natural-layout quant of one weight matrix `W[N][K]` (row-major fp32).
/// Returns `(q4[N*K], s2[N*KG], zero[N*KG], s1[N])` — the shared quant both the
/// packer and the reconstruction reference consume, so they never diverge.
///
/// The stage-1 per-channel scale is chosen by a small grid search over clip
/// ratios (QServe's "magic 119" clipping): a single outlier weight would make
/// `amax/127` crush every other weight in the row to a couple of int8 levels,
/// so trading a clamped outlier for finer resolution on the bulk cuts the
/// requant error from ~0.32 relL2 to a few percent.
fn quantize(w: &[f32], n: usize, k: usize, group: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<f32>) {
    assert_eq!(w.len(), n * k);
    assert_eq!(k % group, 0);
    assert!(group <= 256, "quantize_row staging bounds group to 256");
    let kg = k / group;
    let mut q4 = vec![0u8; n * k];
    let mut s2 = vec![0u8; n * kg];
    let mut zero = vec![0u8; n * kg];
    let mut s1 = vec![0.0f32; n];

    // The clip search is CPU-heavy (9× the quant per row over a whole 7B model),
    // so rows — which are independent — are split across the available cores.
    let nthreads = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .clamp(1, n.max(1));
    let rows_per = n.div_ceil(nthreads);
    std::thread::scope(|scope| {
        for ((((wc, q4c), s2c), zeroc), s1c) in w
            .chunks(rows_per * k)
            .zip(q4.chunks_mut(rows_per * k))
            .zip(s2.chunks_mut(rows_per * kg))
            .zip(zero.chunks_mut(rows_per * kg))
            .zip(s1.chunks_mut(rows_per))
        {
            scope.spawn(move || quantize_rows_window(wc, q4c, s2c, zeroc, s1c, k, kg, group));
        }
    });
    (q4, s2, zero, s1)
}

/// Quantize a contiguous window of rows into pre-sliced output buffers. Each
/// thread owns its scratch, so there is no shared state across the row split.
#[allow(clippy::too_many_arguments)]
fn quantize_rows_window(
    w: &[f32],
    q4: &mut [u8],
    s2: &mut [u8],
    zero: &mut [u8],
    s1: &mut [f32],
    k: usize,
    kg: usize,
    group: usize,
) {
    const CLIPS: [f32; 9] = [1.0, 0.95, 0.9, 0.85, 0.8, 0.75, 0.7, 0.65, 0.6];
    let mut best_q4 = vec![0u8; k];
    let mut best_s2 = vec![0u8; kg];
    let mut best_zero = vec![0u8; kg];
    let mut try_q4 = vec![0u8; k];
    let mut try_s2 = vec![0u8; kg];
    let mut try_zero = vec![0u8; kg];
    let rows = s1.len();
    for row in 0..rows {
        let wr = &w[row * k..row * k + k];
        let mut amax = 0.0f32;
        for &v in wr {
            amax = amax.max(v.abs());
        }
        if amax == 0.0 {
            s1[row] = 1.0;
            continue;
        }
        let mut best_err = f64::INFINITY;
        let mut best_s1 = 1.0f32;
        for &clip in &CLIPS {
            let s1f = f16::from_f32(amax * clip / 127.0).to_f32();
            if s1f <= 0.0 {
                continue;
            }
            let err = quantize_row(wr, k, group, s1f, &mut try_q4, &mut try_s2, &mut try_zero);
            if err < best_err {
                best_err = err;
                best_s1 = s1f;
                best_q4.copy_from_slice(&try_q4);
                best_s2.copy_from_slice(&try_s2);
                best_zero.copy_from_slice(&try_zero);
            }
        }
        s1[row] = best_s1;
        q4[row * k..row * k + k].copy_from_slice(&best_q4);
        s2[row * kg..row * kg + kg].copy_from_slice(&best_s2);
        zero[row * kg..row * kg + kg].copy_from_slice(&best_zero);
    }
}

/// Per-input-channel absolute-max of `W[N][K]` (fp32 row-major): `out[j] =
/// max_n |W[n][j]|`. The SmoothQuant weight-side migration signal.
pub fn col_absmax(w: &[f32], n: usize, k: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; k];
    for row in 0..n {
        let r = &w[row * k..row * k + k];
        for (j, &v) in r.iter().enumerate() {
            out[j] = out[j].max(v.abs());
        }
    }
    out
}

/// SmoothQuant per-input-channel migration scale from calibration activation
/// abs-max and weight abs-max: `s_j = max(|X_j|)^alpha / max(|W_j|)^(1-alpha)`.
/// Clamped to `[1/max_migrate, max_migrate]` so no channel migrates so hard
/// that the smoothed weight (×s) overflows int4 or the activation (×1/s)
/// overflows int8. `alpha` ~ 0.5 balances the two.
pub fn smoothing_scale(act_absmax: &[f32], w_absmax: &[f32], alpha: f32) -> Vec<f32> {
    const MAX_MIGRATE: f32 = 8.0;
    assert_eq!(act_absmax.len(), w_absmax.len());
    // alpha < 0 is the DEFAULT identity (no migration): s = 1 everywhere. On the
    // Q4_K→W4A8 requant path SmoothQuant measurably regresses quality (it inflates
    // the per-row weight range the mandatory stage-1 int8 must cover), so the
    // engine defaults here and only opts into migration on FORGE_W4A8_ALPHA>=0.
    if alpha < 0.0 {
        return vec![1.0; act_absmax.len()];
    }
    act_absmax
        .iter()
        .zip(w_absmax)
        .map(|(&a, &w)| {
            let a = a.max(1e-6);
            let w = w.max(1e-6);
            let s = a.powf(alpha) / w.powf(1.0 - alpha);
            s.clamp(1.0 / MAX_MIGRATE, MAX_MIGRATE)
        })
        .collect()
}

/// SmoothQuant-aware pack: multiply each input column `j` of `W` by `smooth[j]`
/// (migrating that channel's dynamic range into the weight) before the QServe
/// requant. The GEMM's activation quantizer applies the reciprocal `1/smooth`
/// per channel, so `W'·X' == W·X` up to quant noise, but the per-token int8
/// activation range is no longer dominated by a few outlier channels.
pub fn w4a8_pack_smoothed(
    w: &[f32],
    n: usize,
    k: usize,
    group: usize,
    smooth: &[f32],
) -> W4A8Packed {
    assert_eq!(smooth.len(), k, "smoothing vector must have K entries");
    let mut ws = vec![0.0f32; n * k];
    for row in 0..n {
        let src = &w[row * k..row * k + k];
        let dst = &mut ws[row * k..row * k + k];
        for j in 0..k {
            dst[j] = src[j] * smooth[j];
        }
    }
    w4a8_pack(&ws, n, k, group)
}

/// Requant + pack `W[N][K]` (fp32 row-major) to the QServe W4A8 layout.
pub fn w4a8_pack(w: &[f32], n: usize, k: usize, group: usize) -> W4A8Packed {
    assert_eq!(n % 32, 0, "W4A8 needs N % 32 == 0");
    assert_eq!(k % 32, 0, "W4A8 needs K % 32 == 0");
    let (q4, s2, zero, s1) = quantize(w, n, k, group);
    let kg = k / group;
    let (n32, k32) = (n / 32, k / 32);

    // Weight repack: reshape [N/32,2,2,8, K/32,2,4,4], permute to
    // [d0,d4,d3,d6,d5,d2,d7,d1], byte = (q[d1=1] << 4) | q[d1=0]. The flat index
    // over the leading 7 dims is the contiguous qweight byte index.
    let mut qweight = vec![0u8; n * k / 2];
    for d0 in 0..n32 {
        for d4 in 0..k32 {
            for d3 in 0..8 {
                for d6 in 0..4 {
                    for d5 in 0..2 {
                        for d2 in 0..2 {
                            for d7 in 0..4 {
                                let flat7 =
                                    ((((((d0 * k32 + d4) * 8 + d3) * 4 + d6) * 2 + d5) * 2 + d2)
                                        * 4)
                                        + d7;
                                let mut nibs = [0u8; 2];
                                for (d1, nib) in nibs.iter_mut().enumerate() {
                                    let oc = d0 * 32 + d1 * 16 + d2 * 8 + d3;
                                    let ic = d4 * 32 + d5 * 16 + d6 * 4 + d7;
                                    *nib = q4[oc * k + ic] & 0xF;
                                }
                                qweight[flat7] = (nibs[1] << 4) | nibs[0];
                            }
                        }
                    }
                }
            }
        }
    }

    // Scale/zero repack: [N,KG] -> transpose [KG,N]; within each 32-column block,
    // reorder j -> (j%8)*4 + (j//8). `zero` already holds the int8 zero-point
    // `(-zv)*s2` (2's complement), so it copies through directly.
    let mut s2_scales = vec![0u8; kg * n];
    let mut s2_zeros = vec![0u8; kg * n];
    for gi in 0..kg {
        for nb in 0..n32 {
            for j in 0..32 {
                let oc = nb * 32 + j;
                let newj = (j % 8) * 4 + (j / 8);
                let s2v = s2[oc * kg + gi] as i32;
                s2_scales[gi * n + nb * 32 + newj] = s2v as u8;
                s2_zeros[gi * n + nb * 32 + newj] = zero[oc * kg + gi];
            }
        }
    }

    let s1_scales = s1.iter().map(|&v| f16::from_f32(v).to_bits()).collect();
    W4A8Packed {
        qweight,
        s2_scales,
        s2_zeros,
        s1_scales,
        n,
        k,
        group,
    }
}

/// Effective per-element weight the GEMM applies: `s1[row] * int8_wrap(s2*(q4-zero))`.
/// Mirrors the kernel's bytewise int8 reconstruction exactly (used by tests to
/// build an independent CPU golden). Do NOT use in production (allocates `N*K`).
pub fn w4a8_reconstruct(w: &[f32], n: usize, k: usize, group: usize) -> Vec<f32> {
    let (q4, s2, zero, s1) = quantize(w, n, k, group);
    let kg = k / group;
    let mut recon = vec![0.0f32; n * k];
    for row in 0..n {
        for gi in 0..kg {
            let s2v = s2[row * kg + gi] as i32;
            let zpt = zero[row * kg + gi] as i8 as i32;
            for j in 0..group {
                let kk = gi * group + j;
                let wrec = (s2v * q4[row * k + kk] as i32 + zpt) as i8;
                recon[row * k + kk] = s1[row] * wrec as f32;
            }
        }
    }
    recon
}

#[cfg(test)]
mod tests {
    use super::*;

    // LCG pseudo-random gaussian-ish weights, optionally with one large outlier
    // per row (exercises the stage-1 clip search and the non-straddling groups
    // whose zero-point clamp used to crush the requant).
    fn gen(n: usize, k: usize, outlier: bool) -> Vec<f32> {
        let mut s: u64 = 0x1234567;
        let mut nrm = || {
            let mut u = 0.0f32;
            for _ in 0..6 {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                u += ((s >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
            }
            u / 6.0
        };
        let mut w = vec![0.0f32; n * k];
        for v in w.iter_mut() {
            *v = nrm() * 0.02;
        }
        if outlier {
            for row in 0..n {
                w[row * k + (row % k)] = 0.5;
            }
        }
        w
    }

    fn rel_l2(w: &[f32], r: &[f32]) -> f64 {
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for (a, b) in w.iter().zip(r) {
            num += ((*a - *b) as f64).powi(2);
            den += (*a as f64).powi(2);
        }
        (num / den).sqrt()
    }

    // The requant must be a proper 4-bit quantizer: a few-percent relL2 on
    // smooth weights, and — the regression guard for the zero-point-clamp bug —
    // a finer group must never be WORSE than a coarser one.
    #[test]
    fn requant_quality_and_group_monotonicity() {
        let (n, k) = (256usize, 4096usize);
        for outlier in [false, true] {
            let w = gen(n, k, outlier);
            let e128 = rel_l2(&w, &w4a8_reconstruct(&w, n, k, 128));
            let e32 = rel_l2(&w, &w4a8_reconstruct(&w, n, k, 32));
            assert!(e32 <= e128 + 1e-6, "G=32 ({e32}) worse than G=128 ({e128})");
            if !outlier {
                assert!(e128 < 0.05, "smooth-weight requant relL2 {e128} too high");
            }
        }
    }
}
