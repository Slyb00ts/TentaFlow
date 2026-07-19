// ===== File: w4a8.rs — Q4_K/fp16 -> QServe W4A8 requant + weight interleave =====
// Produces the buffers QServe's `w4a8_per_group` `dense_kernel0` reads (kernels/
// cuda/w4a8_gemm.cu): int4 weight codes in the 8-D interleave, per-group int8
// secondary scale + (-zero)*s2, per-channel fp16 primary scale. Two-level QoQ
// quant: w ~= s1[row] * s2[row,group] * (q4 - zero). group_size = 128 (the
// kernel's compile-time G). This host packer is validated byte-exact against a
// CPU int4xint8 golden through the GPU kernel in forge-kernels' `cuda_w4a8` test
// (relL2 ~2e-4 = fp16 noise). Layout mirrors omniserve/.../w4a8_linear.py.
//
// NOTE (quality): requantizing an already-Q4_K weight through W4A8 adds ~10%
// relL2 on top of Q4_K (docs/BENCH_COMPARISON.md Phase B). Fair-quality
// production would requant from the original fp16 weights.

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

/// Natural-layout quant of one weight matrix `W[N][K]` (row-major fp32).
/// Returns `(q4[N*K], s2[N*KG], zero[N*KG], s1[N])` — the shared quant both the
/// packer and the reconstruction reference consume, so they never diverge.
fn quantize(w: &[f32], n: usize, k: usize, group: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<f32>) {
    assert_eq!(w.len(), n * k);
    assert_eq!(k % group, 0);
    let kg = k / group;
    let mut q4 = vec![0u8; n * k];
    let mut s2 = vec![0u8; n * kg];
    let mut zero = vec![0u8; n * kg];
    let mut s1 = vec![0.0f32; n];
    let mut wi8 = vec![0i32; k];

    for row in 0..n {
        // Stage 1: per-channel fp16 scale -> int8.
        let mut amax = 0.0f32;
        for &v in &w[row * k..row * k + k] {
            amax = amax.max(v.abs());
        }
        let s1f = f16::from_f32(if amax > 0.0 { amax / 127.0 } else { 1.0 }).to_f32();
        s1[row] = s1f;
        for kk in 0..k {
            let v = (w[row * k + kk] / s1f).round() as i32;
            wi8[kk] = v.clamp(-127, 127);
        }
        // Stage 2: per-group int8 scale + int zero -> int4.
        for gi in 0..kg {
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for j in 0..group {
                let v = wi8[gi * group + j];
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let s2v = ((hi - lo + 14) / 15).clamp(1, 127);
            let zv = ((-(lo as f32) / s2v as f32).round() as i32).clamp(0, 15);
            s2[row * kg + gi] = s2v as u8;
            zero[row * kg + gi] = zv as u8;
            for j in 0..group {
                let v = wi8[gi * group + j];
                let q = ((v as f32 / s2v as f32).round() as i32 + zv).clamp(0, 15);
                q4[row * k + gi * group + j] = q as u8;
            }
        }
    }
    (q4, s2, zero, s1)
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
                                let flat7 = ((((((d0 * k32 + d4) * 8 + d3) * 4 + d6) * 2 + d5) * 2
                                    + d2)
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
    // reorder j -> (j%8)*4 + (j//8). zeros stored as (-zero)*s2 (2's complement).
    let mut s2_scales = vec![0u8; kg * n];
    let mut s2_zeros = vec![0u8; kg * n];
    for gi in 0..kg {
        for nb in 0..n32 {
            for j in 0..32 {
                let oc = nb * 32 + j;
                let newj = (j % 8) * 4 + (j / 8);
                let s2v = s2[oc * kg + gi] as i32;
                let zv = zero[oc * kg + gi] as i32;
                s2_scales[gi * n + nb * 32 + newj] = s2v as u8;
                s2_zeros[gi * n + nb * 32 + newj] = ((-zv) * s2v) as i8 as u8;
            }
        }
    }

    let s1_scales = s1.iter().map(|&v| f16::from_f32(v).to_bits()).collect();
    W4A8Packed { qweight, s2_scales, s2_zeros, s1_scales, n, k, group }
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
            let zv = zero[row * kg + gi] as i32;
            for j in 0..group {
                let kk = gi * group + j;
                let wrec = (s2v * (q4[row * k + kk] as i32 - zv)) as i8;
                recon[row * k + kk] = s1[row] * wrec as f32;
            }
        }
    }
    recon
}
