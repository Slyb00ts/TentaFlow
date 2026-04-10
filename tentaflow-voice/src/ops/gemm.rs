// =============================================================================
// Plik: ops/gemm.rs
// Opis: Wysokowydajne GEMM (general matrix multiply) dla f32 — fundament
//       WeSpeaker/Silero forward pass. Konwencja row-major:
//           C[M, N] += A[M, K] * B[K, N]   (alpha=1, beta=1)
//
// Strategie wydajnosciowe:
//  1. Contiguous SIMD loads — `<[f32; 8]>::try_from(&slice[..8])` kompiluje
//     sie do pojedynczej 256-bitowej instrukcji vmovups (AVX) lub 2x ldp
//     (NEON). ZADNYCH scalar gather'ow.
//  2. FMA — f32x8::mul_add() mapuje sie na vfmaddps (x86) / fmla (ARM).
//  3. Register blocking — microkernel 1x32 trzyma 4 ymm accumulatory w
//     rejestrach przez cala petle K, arithmetic intensity ~1 FMA per 1 B load.
//  4. Rayon parallelism po wierszach M — embarrassingly parallel, kazdy watek
//     pisze do disjoint slice C.
//  5. target-cpu=native (w .cargo/config.toml) wlacza AVX2+FMA/AVX-512/NEON.
//     Bez tego wide::f32x8 lecialoby jako 2x SSE2 = 4x wolniej.
// =============================================================================

use rayon::prelude::*;
use wide::f32x8;

/// Ladowanie 8 kolejnych f32 do f32x8 — jedna vmovups/ldp instrukcja.
/// Bounds check jest eliminowany przez kompilator jesli caller zapewnia
/// `src.len() >= 8`.
#[inline(always)]
fn load8(src: &[f32]) -> f32x8 {
    let arr: [f32; 8] = src[..8].try_into().expect("load8: slice len < 8");
    f32x8::from(arr)
}

/// Zapis f32x8 do 8 kolejnych f32 — jedna vmovups/stp.
#[inline(always)]
fn store8(dst: &mut [f32], v: f32x8) {
    let arr = v.to_array();
    dst[..8].copy_from_slice(&arr);
}

/// GEMM: C = A * B + bias (per-row), gdzie
///   A: [M, K] row-major
///   B: [K, N] row-major
///   C: [M, N] row-major (output, overwritten)
///   bias: opcjonalnie [M] — dodawane do kazdej kolumny wiersza i
///
/// Rownoleglizowane po wierszach M. Dla malych M (< RAYON_THRESHOLD) uzywa
/// single-thread zeby uniknac overhead'u.
pub fn gemm(
    a: &[f32],
    b: &[f32],
    c: &mut [f32],
    m: usize,
    n: usize,
    k: usize,
    bias: Option<&[f32]>,
) {
    debug_assert_eq!(a.len(), m * k, "gemm: A shape mismatch");
    debug_assert_eq!(b.len(), k * n, "gemm: B shape mismatch");
    debug_assert_eq!(c.len(), m * n, "gemm: C shape mismatch");
    if let Some(b_) = bias {
        debug_assert_eq!(b_.len(), m, "gemm: bias shape mismatch");
    }

    // Prog poniżej ktorego nie uzywamy rayona — overhead spawn > computation
    const RAYON_THRESHOLD: usize = 32;

    if m < RAYON_THRESHOLD {
        // Single thread — kolejno wiersze
        for m_idx in 0..m {
            let c_row = &mut c[m_idx * n..(m_idx + 1) * n];
            let a_row = &a[m_idx * k..(m_idx + 1) * k];
            let bias_val = bias.map(|bs| bs[m_idx]).unwrap_or(0.0);
            gemm_one_row(a_row, b, c_row, n, k, bias_val);
        }
    } else {
        // Multi-thread — kazdy watek bierze jeden wiersz (rayon balansuje sam).
        // par_chunks_mut(n) daje niezalezne slice'y wierszy C — brak contention.
        c.par_chunks_mut(n)
            .enumerate()
            .for_each(|(m_idx, c_row)| {
                let a_row = &a[m_idx * k..(m_idx + 1) * k];
                let bias_val = bias.map(|bs| bs[m_idx]).unwrap_or(0.0);
                gemm_one_row(a_row, b, c_row, n, k, bias_val);
            });
    }
}

/// Microkernel: oblicza jeden wiersz C (N kolumn) = a_row * B + bias.
///
/// Strategia: przetwarzamy 32 kolumny naraz trzymajac 4 ymm accumulatory w
/// rejestrach przez cala petle K. Arithmetic intensity = 4 FMAs per iteracja
/// K, tylko 4 f32x8 loads z B (cache-friendly sekwencyjnie) + 1 scalar z A.
#[inline]
fn gemm_one_row(a_row: &[f32], b: &[f32], c_row: &mut [f32], n: usize, k: usize, bias_val: f32) {
    let bias_v = f32x8::splat(bias_val);
    let n32 = n - (n % 32); // grupa 32 kolumn
    let n8 = n - (n % 8); // grupa 8 kolumn (tail 1)

    // Glowny loop: 32 kolumny naraz
    let mut j = 0;
    while j < n32 {
        let mut acc0 = bias_v;
        let mut acc1 = bias_v;
        let mut acc2 = bias_v;
        let mut acc3 = bias_v;

        for kk in 0..k {
            let a_val = a_row[kk];
            let a_s = f32x8::splat(a_val);
            let b_base = kk * n + j;

            // 4x contiguous 256-bit loads → 4 ymm
            let b0 = load8(&b[b_base..b_base + 8]);
            let b1 = load8(&b[b_base + 8..b_base + 16]);
            let b2 = load8(&b[b_base + 16..b_base + 24]);
            let b3 = load8(&b[b_base + 24..b_base + 32]);

            // 4 FMAs — vfmaddps (x86) / fmla (ARM)
            acc0 = a_s.mul_add(b0, acc0);
            acc1 = a_s.mul_add(b1, acc1);
            acc2 = a_s.mul_add(b2, acc2);
            acc3 = a_s.mul_add(b3, acc3);
        }

        store8(&mut c_row[j..j + 8], acc0);
        store8(&mut c_row[j + 8..j + 16], acc1);
        store8(&mut c_row[j + 16..j + 24], acc2);
        store8(&mut c_row[j + 24..j + 32], acc3);

        j += 32;
    }

    // Tail 1: pozostalosc 8-kolumnowa
    while j < n8 {
        let mut acc = bias_v;
        for kk in 0..k {
            let a_s = f32x8::splat(a_row[kk]);
            let b_vec = load8(&b[kk * n + j..kk * n + j + 8]);
            acc = a_s.mul_add(b_vec, acc);
        }
        store8(&mut c_row[j..j + 8], acc);
        j += 8;
    }

    // Tail 2: pozostalosc skalarna
    while j < n {
        let mut sum = bias_val;
        for kk in 0..k {
            sum += a_row[kk] * b[kk * n + j];
        }
        c_row[j] = sum;
        j += 1;
    }
}

/// Akumulujaca wersja: C += A * B (alpha=1, beta=1 bez bias init).
/// Uzywana w Conv1D k>1 gdy akumulujemy po roznych kernel positions.
pub fn gemm_accumulate(
    a: &[f32],
    b: &[f32],
    c: &mut [f32],
    m: usize,
    n: usize,
    k: usize,
) {
    debug_assert_eq!(a.len(), m * k);
    debug_assert_eq!(b.len(), k * n);
    debug_assert_eq!(c.len(), m * n);

    const RAYON_THRESHOLD: usize = 32;

    if m < RAYON_THRESHOLD {
        for m_idx in 0..m {
            let a_row = &a[m_idx * k..(m_idx + 1) * k];
            let c_row = &mut c[m_idx * n..(m_idx + 1) * n];
            gemm_one_row_accumulate(a_row, b, c_row, n, k);
        }
    } else {
        c.par_chunks_mut(n)
            .enumerate()
            .for_each(|(m_idx, c_row)| {
                let a_row = &a[m_idx * k..(m_idx + 1) * k];
                gemm_one_row_accumulate(a_row, b, c_row, n, k);
            });
    }
}

#[inline]
fn gemm_one_row_accumulate(a_row: &[f32], b: &[f32], c_row: &mut [f32], n: usize, k: usize) {
    let n32 = n - (n % 32);
    let n8 = n - (n % 8);

    let mut j = 0;
    while j < n32 {
        // Load istniejace akumulatory z C
        let mut acc0 = load8(&c_row[j..j + 8]);
        let mut acc1 = load8(&c_row[j + 8..j + 16]);
        let mut acc2 = load8(&c_row[j + 16..j + 24]);
        let mut acc3 = load8(&c_row[j + 24..j + 32]);

        for kk in 0..k {
            let a_s = f32x8::splat(a_row[kk]);
            let b_base = kk * n + j;
            let b0 = load8(&b[b_base..b_base + 8]);
            let b1 = load8(&b[b_base + 8..b_base + 16]);
            let b2 = load8(&b[b_base + 16..b_base + 24]);
            let b3 = load8(&b[b_base + 24..b_base + 32]);

            acc0 = a_s.mul_add(b0, acc0);
            acc1 = a_s.mul_add(b1, acc1);
            acc2 = a_s.mul_add(b2, acc2);
            acc3 = a_s.mul_add(b3, acc3);
        }

        store8(&mut c_row[j..j + 8], acc0);
        store8(&mut c_row[j + 8..j + 16], acc1);
        store8(&mut c_row[j + 16..j + 24], acc2);
        store8(&mut c_row[j + 24..j + 32], acc3);

        j += 32;
    }

    while j < n8 {
        let mut acc = load8(&c_row[j..j + 8]);
        for kk in 0..k {
            let a_s = f32x8::splat(a_row[kk]);
            let b_vec = load8(&b[kk * n + j..kk * n + j + 8]);
            acc = a_s.mul_add(b_vec, acc);
        }
        store8(&mut c_row[j..j + 8], acc);
        j += 8;
    }

    while j < n {
        let mut sum = c_row[j];
        for kk in 0..k {
            sum += a_row[kk] * b[kk * n + j];
        }
        c_row[j] = sum;
        j += 1;
    }
}

/// Matrix-vector: y[M] = W[M, K] * x[K] + bias[M]
/// Dla linear layers na koniec pipeline'u (SE blocks, final embedding).
pub fn gemv(weights: &[f32], x: &[f32], bias: Option<&[f32]>, m: usize, k: usize, y: &mut [f32]) {
    debug_assert_eq!(weights.len(), m * k);
    debug_assert_eq!(x.len(), k);
    debug_assert_eq!(y.len(), m);

    let k8 = k - (k % 8);

    // Rownolegle po wierszach gdy m > 32
    const RAYON_THRESHOLD: usize = 32;
    if m < RAYON_THRESHOLD {
        for m_idx in 0..m {
            let row = &weights[m_idx * k..(m_idx + 1) * k];
            let bias_val = bias.map(|b| b[m_idx]).unwrap_or(0.0);
            y[m_idx] = dot_product(row, x, k8, k) + bias_val;
        }
    } else {
        y.par_iter_mut().enumerate().for_each(|(m_idx, y_val)| {
            let row = &weights[m_idx * k..(m_idx + 1) * k];
            let bias_val = bias.map(|b| b[m_idx]).unwrap_or(0.0);
            *y_val = dot_product(row, x, k8, k) + bias_val;
        });
    }
}

#[inline]
fn dot_product(row: &[f32], x: &[f32], k8: usize, k: usize) -> f32 {
    let mut acc0 = f32x8::splat(0.0);
    let mut acc1 = f32x8::splat(0.0);
    let mut acc2 = f32x8::splat(0.0);
    let mut acc3 = f32x8::splat(0.0);

    // 4-way unrolling dla maskowania latencji FMA
    let k32 = k - (k % 32);
    let mut i = 0;
    while i < k32 {
        let r0 = load8(&row[i..i + 8]);
        let r1 = load8(&row[i + 8..i + 16]);
        let r2 = load8(&row[i + 16..i + 24]);
        let r3 = load8(&row[i + 24..i + 32]);

        let x0 = load8(&x[i..i + 8]);
        let x1 = load8(&x[i + 8..i + 16]);
        let x2 = load8(&x[i + 16..i + 24]);
        let x3 = load8(&x[i + 24..i + 32]);

        acc0 = r0.mul_add(x0, acc0);
        acc1 = r1.mul_add(x1, acc1);
        acc2 = r2.mul_add(x2, acc2);
        acc3 = r3.mul_add(x3, acc3);

        i += 32;
    }

    // Drobniejsze bloki 8
    while i < k8 {
        let r = load8(&row[i..i + 8]);
        let x_v = load8(&x[i..i + 8]);
        acc0 = r.mul_add(x_v, acc0);
        i += 8;
    }

    // Horizontal reduction
    let combined = (acc0 + acc1) + (acc2 + acc3);
    let lanes = combined.to_array();
    let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                + lanes[4] + lanes[5] + lanes[6] + lanes[7];

    // Tail skalarny
    while i < k {
        sum += row[i] * x[i];
        i += 1;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_gemm(a: &[f32], b: &[f32], m: usize, n: usize, k: usize) -> Vec<f32> {
        let mut c = vec![0.0_f32; m * n];
        for i in 0..m {
            for j in 0..n {
                let mut sum = 0.0;
                for kk in 0..k {
                    sum += a[i * k + kk] * b[kk * n + j];
                }
                c[i * n + j] = sum;
            }
        }
        c
    }

    #[test]
    fn gemm_matches_naive_small() {
        let m = 5;
        let n = 37;
        let k = 11;
        let a: Vec<f32> = (0..m * k).map(|i| (i as f32 * 0.013 - 0.7)).collect();
        let b: Vec<f32> = (0..k * n).map(|i| (i as f32 * 0.021 + 0.3)).collect();
        let expected = naive_gemm(&a, &b, m, n, k);

        let mut c = vec![0.0_f32; m * n];
        gemm(&a, &b, &mut c, m, n, k, None);

        for i in 0..m * n {
            let diff = (c[i] - expected[i]).abs();
            assert!(diff < 1e-3, "idx {}: got {}, expected {}, diff {}", i, c[i], expected[i], diff);
        }
    }

    #[test]
    fn gemm_matches_naive_tile_aligned() {
        // 32-column tile aligned, 128-row tile > RAYON_THRESHOLD
        let m = 64;
        let n = 96;
        let k = 48;
        let a: Vec<f32> = (0..m * k).map(|i| ((i * 37 % 113) as f32 - 50.0) * 0.01).collect();
        let b: Vec<f32> = (0..k * n).map(|i| ((i * 53 % 97) as f32 - 40.0) * 0.02).collect();
        let expected = naive_gemm(&a, &b, m, n, k);

        let mut c = vec![0.0_f32; m * n];
        gemm(&a, &b, &mut c, m, n, k, None);

        for i in 0..m * n {
            let diff = (c[i] - expected[i]).abs();
            assert!(diff < 1e-2, "idx {}: got {}, expected {}", i, c[i], expected[i]);
        }
    }

    #[test]
    fn gemm_with_bias() {
        let m = 4;
        let n = 40;
        let k = 8;
        let a: Vec<f32> = (0..m * k).map(|i| (i as f32) * 0.1).collect();
        let b: Vec<f32> = vec![1.0; k * n];
        let bias: Vec<f32> = vec![100.0, 200.0, 300.0, 400.0];

        let mut c = vec![0.0_f32; m * n];
        gemm(&a, &b, &mut c, m, n, k, Some(&bias));

        // Row i sum = sum_k a[i, k] * 1 + bias[i]
        for i in 0..m {
            let row_sum: f32 = (0..k).map(|kk| a[i * k + kk]).sum::<f32>() + bias[i];
            for j in 0..n {
                let diff = (c[i * n + j] - row_sum).abs();
                assert!(diff < 1e-3);
            }
        }
    }

    #[test]
    fn gemm_accumulate_adds_to_c() {
        let m = 8;
        let n = 40;
        let k = 16;
        let a: Vec<f32> = (0..m * k).map(|i| i as f32 * 0.01).collect();
        let b: Vec<f32> = (0..k * n).map(|i| i as f32 * 0.02).collect();

        // Najpierw naive dwa razy
        let delta = naive_gemm(&a, &b, m, n, k);
        let expected: Vec<f32> = delta.iter().map(|&v| v * 2.0).collect();

        let mut c = delta.clone();
        gemm_accumulate(&a, &b, &mut c, m, n, k);

        for i in 0..m * n {
            let diff = (c[i] - expected[i]).abs();
            assert!(diff < 1e-2);
        }
    }

    #[test]
    fn gemv_matches_naive() {
        let m = 192;
        let k = 3072;
        let weights: Vec<f32> = (0..m * k).map(|i| ((i as f32) * 0.0001).sin()).collect();
        let x: Vec<f32> = (0..k).map(|i| ((i as f32) * 0.0003).cos()).collect();
        let bias: Vec<f32> = (0..m).map(|i| i as f32 * 0.01).collect();

        let mut y = vec![0.0_f32; m];
        gemv(&weights, &x, Some(&bias), m, k, &mut y);

        for i in 0..m {
            let row = &weights[i * k..(i + 1) * k];
            let expected: f32 = row.iter().zip(x.iter()).map(|(w, xv)| w * xv).sum::<f32>() + bias[i];
            let diff = (y[i] - expected).abs();
            assert!(diff < 1e-2, "idx {}: got {}, expected {}", i, y[i], expected);
        }
    }
}
