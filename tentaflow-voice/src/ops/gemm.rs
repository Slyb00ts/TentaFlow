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

/// Runtime CPU feature detection — sprawdzane raz, wynik cache'owany.
/// Pozwala na jeden binarny artefakt ktory na CPU z AVX-512 leci 16-wide,
/// a na starszych (np. AVX2 only) spada na 8-wide f32x8.
#[cfg(target_arch = "x86_64")]
fn has_avx512f() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let has = is_x86_feature_detected!("avx512f");
        tracing::info!(avx512f = has, "GEMM backend selected");
        has
    })
}

#[cfg(not(target_arch = "x86_64"))]
#[allow(dead_code)] // Cross-platform stub: callers gate AVX-512 paths
                    // behind this on x86_64; on arm64/etc the result is
                    // always false and cargo would otherwise flag the
                    // gating call sites as dead.
fn has_avx512f() -> bool {
    false
}

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

// =============================================================================
// AVX-512 microkernel — 16-wide f32, 2x wider than AVX2.
// Kompilowane zawsze na x86_64 (funkcje maja #[target_feature(enable=...)]
// wiec LLVM emituje AVX-512 niezaleznie od global target features). Runtime
// dispatch ponizej wybiera AVX-512 tylko jesli CPU go ma.
// =============================================================================
#[cfg(target_arch = "x86_64")]
mod avx512 {
    use std::arch::x86_64::*;

    /// Microkernel: oblicza jeden wiersz C (n kolumn) = a_row * B + bias.
    /// Przetwarza 64 kolumny naraz trzymajac 4 zmm accumulatory w rejestrach
    /// przez cala petle K. 4 FMA per K step × 16 lanes = 64 FLOPs/cycle
    /// per rdzen przy IPC 1 (AVX-512 FMA ma throughput 1 cycle na Zen4).
    ///
    /// # Safety
    /// Wymaga CPU z AVX-512F. Caller gwarantuje `a_row.len() >= k`,
    /// `b.len() >= k*n`, `c_row.len() >= n`.
    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_one_row_avx512(
        a_row: &[f32],
        b: &[f32],
        c_row: &mut [f32],
        n: usize,
        k: usize,
        bias_val: f32,
    ) {
        let bias_v = _mm512_set1_ps(bias_val);
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let mut j = 0;
        while j < n64 {
            let mut acc0 = bias_v;
            let mut acc1 = bias_v;
            let mut acc2 = bias_v;
            let mut acc3 = bias_v;

            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_base = kk * n + j;

                // 4x contiguous 512-bit loads (4x 64B cache lines)
                let b0 = _mm512_loadu_ps(b.as_ptr().add(b_base));
                let b1 = _mm512_loadu_ps(b.as_ptr().add(b_base + 16));
                let b2 = _mm512_loadu_ps(b.as_ptr().add(b_base + 32));
                let b3 = _mm512_loadu_ps(b.as_ptr().add(b_base + 48));

                // 4x vfmadd231ps — 64 FMAs = 128 FLOPs per K iteration
                acc0 = _mm512_fmadd_ps(a_s, b0, acc0);
                acc1 = _mm512_fmadd_ps(a_s, b1, acc1);
                acc2 = _mm512_fmadd_ps(a_s, b2, acc2);
                acc3 = _mm512_fmadd_ps(a_s, b3, acc3);
            }

            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc0);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 16), acc1);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 32), acc2);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 48), acc3);

            j += 64;
        }

        // Tail 16: pojedynczy zmm
        while j < n16 {
            let mut acc = bias_v;
            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_vec = _mm512_loadu_ps(b.as_ptr().add(kk * n + j));
                acc = _mm512_fmadd_ps(a_s, b_vec, acc);
            }
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc);
            j += 16;
        }

        // Tail skalarny
        while j < n {
            let mut sum = bias_val;
            for kk in 0..k {
                sum += *a_row.get_unchecked(kk) * *b.get_unchecked(kk * n + j);
            }
            *c_row.get_unchecked_mut(j) = sum;
            j += 1;
        }
    }

    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_one_row_accumulate_avx512(
        a_row: &[f32],
        b: &[f32],
        c_row: &mut [f32],
        n: usize,
        k: usize,
    ) {
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let mut j = 0;
        while j < n64 {
            let mut acc0 = _mm512_loadu_ps(c_row.as_ptr().add(j));
            let mut acc1 = _mm512_loadu_ps(c_row.as_ptr().add(j + 16));
            let mut acc2 = _mm512_loadu_ps(c_row.as_ptr().add(j + 32));
            let mut acc3 = _mm512_loadu_ps(c_row.as_ptr().add(j + 48));

            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_base = kk * n + j;
                let b0 = _mm512_loadu_ps(b.as_ptr().add(b_base));
                let b1 = _mm512_loadu_ps(b.as_ptr().add(b_base + 16));
                let b2 = _mm512_loadu_ps(b.as_ptr().add(b_base + 32));
                let b3 = _mm512_loadu_ps(b.as_ptr().add(b_base + 48));

                acc0 = _mm512_fmadd_ps(a_s, b0, acc0);
                acc1 = _mm512_fmadd_ps(a_s, b1, acc1);
                acc2 = _mm512_fmadd_ps(a_s, b2, acc2);
                acc3 = _mm512_fmadd_ps(a_s, b3, acc3);
            }

            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc0);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 16), acc1);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 32), acc2);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 48), acc3);

            j += 64;
        }

        while j < n16 {
            let mut acc = _mm512_loadu_ps(c_row.as_ptr().add(j));
            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_vec = _mm512_loadu_ps(b.as_ptr().add(kk * n + j));
                acc = _mm512_fmadd_ps(a_s, b_vec, acc);
            }
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc);
            j += 16;
        }

        while j < n {
            let mut sum = *c_row.get_unchecked(j);
            for kk in 0..k {
                sum += *a_row.get_unchecked(kk) * *b.get_unchecked(kk * n + j);
            }
            *c_row.get_unchecked_mut(j) = sum;
            j += 1;
        }
    }

    /// 4-row strided accumulate AVX-512 — jak gemm_4row_tile_accumulate ale z
    /// b_row_stride != n. Uzywane w Conv1D k>1 (layer1 k=5, Res2 k=3).
    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_4row_tile_accumulate_strided_avx512(
        a_rows: *const f32,
        b: *const f32,
        b_row_stride: usize,
        c_rows: *mut f32,
        c_row_stride: usize,
        n: usize,
        k: usize,
    ) {
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let a0 = a_rows;
        let a1 = a_rows.add(k);
        let a2 = a_rows.add(2 * k);
        let a3 = a_rows.add(3 * k);
        let c0 = c_rows;
        let c1 = c_rows.add(c_row_stride);
        let c2 = c_rows.add(2 * c_row_stride);
        let c3 = c_rows.add(3 * c_row_stride);

        let mut j = 0;
        while j < n64 {
            let mut acc00 = _mm512_loadu_ps(c0.add(j));
            let mut acc01 = _mm512_loadu_ps(c0.add(j + 16));
            let mut acc02 = _mm512_loadu_ps(c0.add(j + 32));
            let mut acc03 = _mm512_loadu_ps(c0.add(j + 48));
            let mut acc10 = _mm512_loadu_ps(c1.add(j));
            let mut acc11 = _mm512_loadu_ps(c1.add(j + 16));
            let mut acc12 = _mm512_loadu_ps(c1.add(j + 32));
            let mut acc13 = _mm512_loadu_ps(c1.add(j + 48));
            let mut acc20 = _mm512_loadu_ps(c2.add(j));
            let mut acc21 = _mm512_loadu_ps(c2.add(j + 16));
            let mut acc22 = _mm512_loadu_ps(c2.add(j + 32));
            let mut acc23 = _mm512_loadu_ps(c2.add(j + 48));
            let mut acc30 = _mm512_loadu_ps(c3.add(j));
            let mut acc31 = _mm512_loadu_ps(c3.add(j + 16));
            let mut acc32 = _mm512_loadu_ps(c3.add(j + 32));
            let mut acc33 = _mm512_loadu_ps(c3.add(j + 48));

            for kk in 0..k {
                let b_base = b.add(kk * b_row_stride + j);
                let b0 = _mm512_loadu_ps(b_base);
                let b1 = _mm512_loadu_ps(b_base.add(16));
                let b2 = _mm512_loadu_ps(b_base.add(32));
                let b3 = _mm512_loadu_ps(b_base.add(48));

                let a0s = _mm512_set1_ps(*a0.add(kk));
                acc00 = _mm512_fmadd_ps(a0s, b0, acc00);
                acc01 = _mm512_fmadd_ps(a0s, b1, acc01);
                acc02 = _mm512_fmadd_ps(a0s, b2, acc02);
                acc03 = _mm512_fmadd_ps(a0s, b3, acc03);

                let a1s = _mm512_set1_ps(*a1.add(kk));
                acc10 = _mm512_fmadd_ps(a1s, b0, acc10);
                acc11 = _mm512_fmadd_ps(a1s, b1, acc11);
                acc12 = _mm512_fmadd_ps(a1s, b2, acc12);
                acc13 = _mm512_fmadd_ps(a1s, b3, acc13);

                let a2s = _mm512_set1_ps(*a2.add(kk));
                acc20 = _mm512_fmadd_ps(a2s, b0, acc20);
                acc21 = _mm512_fmadd_ps(a2s, b1, acc21);
                acc22 = _mm512_fmadd_ps(a2s, b2, acc22);
                acc23 = _mm512_fmadd_ps(a2s, b3, acc23);

                let a3s = _mm512_set1_ps(*a3.add(kk));
                acc30 = _mm512_fmadd_ps(a3s, b0, acc30);
                acc31 = _mm512_fmadd_ps(a3s, b1, acc31);
                acc32 = _mm512_fmadd_ps(a3s, b2, acc32);
                acc33 = _mm512_fmadd_ps(a3s, b3, acc33);
            }

            _mm512_storeu_ps(c0.add(j), acc00);
            _mm512_storeu_ps(c0.add(j + 16), acc01);
            _mm512_storeu_ps(c0.add(j + 32), acc02);
            _mm512_storeu_ps(c0.add(j + 48), acc03);
            _mm512_storeu_ps(c1.add(j), acc10);
            _mm512_storeu_ps(c1.add(j + 16), acc11);
            _mm512_storeu_ps(c1.add(j + 32), acc12);
            _mm512_storeu_ps(c1.add(j + 48), acc13);
            _mm512_storeu_ps(c2.add(j), acc20);
            _mm512_storeu_ps(c2.add(j + 16), acc21);
            _mm512_storeu_ps(c2.add(j + 32), acc22);
            _mm512_storeu_ps(c2.add(j + 48), acc23);
            _mm512_storeu_ps(c3.add(j), acc30);
            _mm512_storeu_ps(c3.add(j + 16), acc31);
            _mm512_storeu_ps(c3.add(j + 32), acc32);
            _mm512_storeu_ps(c3.add(j + 48), acc33);

            j += 64;
        }

        while j < n16 {
            let mut a0acc = _mm512_loadu_ps(c0.add(j));
            let mut a1acc = _mm512_loadu_ps(c1.add(j));
            let mut a2acc = _mm512_loadu_ps(c2.add(j));
            let mut a3acc = _mm512_loadu_ps(c3.add(j));
            for kk in 0..k {
                let b_v = _mm512_loadu_ps(b.add(kk * b_row_stride + j));
                a0acc = _mm512_fmadd_ps(_mm512_set1_ps(*a0.add(kk)), b_v, a0acc);
                a1acc = _mm512_fmadd_ps(_mm512_set1_ps(*a1.add(kk)), b_v, a1acc);
                a2acc = _mm512_fmadd_ps(_mm512_set1_ps(*a2.add(kk)), b_v, a2acc);
                a3acc = _mm512_fmadd_ps(_mm512_set1_ps(*a3.add(kk)), b_v, a3acc);
            }
            _mm512_storeu_ps(c0.add(j), a0acc);
            _mm512_storeu_ps(c1.add(j), a1acc);
            _mm512_storeu_ps(c2.add(j), a2acc);
            _mm512_storeu_ps(c3.add(j), a3acc);
            j += 16;
        }

        while j < n {
            let mut s0 = *c0.add(j);
            let mut s1 = *c1.add(j);
            let mut s2 = *c2.add(j);
            let mut s3 = *c3.add(j);
            for kk in 0..k {
                let b_val = *b.add(kk * b_row_stride + j);
                s0 += *a0.add(kk) * b_val;
                s1 += *a1.add(kk) * b_val;
                s2 += *a2.add(kk) * b_val;
                s3 += *a3.add(kk) * b_val;
            }
            *c0.add(j) = s0;
            *c1.add(j) = s1;
            *c2.add(j) = s2;
            *c3.add(j) = s3;
            j += 1;
        }
    }

    /// Strided accumulate AVX-512 — czyta B z row stride != n (dla conv1d k>1)
    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_one_row_accumulate_strided_avx512(
        a_row: &[f32],
        b: &[f32],
        b_row_stride: usize,
        c_row: &mut [f32],
        n: usize,
        k: usize,
    ) {
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let mut j = 0;
        while j < n64 {
            let mut acc0 = _mm512_loadu_ps(c_row.as_ptr().add(j));
            let mut acc1 = _mm512_loadu_ps(c_row.as_ptr().add(j + 16));
            let mut acc2 = _mm512_loadu_ps(c_row.as_ptr().add(j + 32));
            let mut acc3 = _mm512_loadu_ps(c_row.as_ptr().add(j + 48));

            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_base = kk * b_row_stride + j;
                let b0 = _mm512_loadu_ps(b.as_ptr().add(b_base));
                let b1 = _mm512_loadu_ps(b.as_ptr().add(b_base + 16));
                let b2 = _mm512_loadu_ps(b.as_ptr().add(b_base + 32));
                let b3 = _mm512_loadu_ps(b.as_ptr().add(b_base + 48));

                acc0 = _mm512_fmadd_ps(a_s, b0, acc0);
                acc1 = _mm512_fmadd_ps(a_s, b1, acc1);
                acc2 = _mm512_fmadd_ps(a_s, b2, acc2);
                acc3 = _mm512_fmadd_ps(a_s, b3, acc3);
            }

            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc0);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 16), acc1);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 32), acc2);
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j + 48), acc3);

            j += 64;
        }

        while j < n16 {
            let mut acc = _mm512_loadu_ps(c_row.as_ptr().add(j));
            for kk in 0..k {
                let a_s = _mm512_set1_ps(*a_row.get_unchecked(kk));
                let b_vec = _mm512_loadu_ps(b.as_ptr().add(kk * b_row_stride + j));
                acc = _mm512_fmadd_ps(a_s, b_vec, acc);
            }
            _mm512_storeu_ps(c_row.as_mut_ptr().add(j), acc);
            j += 16;
        }

        while j < n {
            let mut sum = *c_row.get_unchecked(j);
            for kk in 0..k {
                sum += *a_row.get_unchecked(kk) * *b.get_unchecked(kk * b_row_stride + j);
            }
            *c_row.get_unchecked_mut(j) = sum;
            j += 1;
        }
    }

    /// Microkernel 4-row × 64-col AVX-512. Oblicza 4 wiersze C (kazdy 64 kolumny)
    /// rownoczesnie, trzymajac 16 zmm akumulatorow w rejestrach przez cala petle K.
    /// Kazdy krok K wykonuje 4 loady B + 4 broadcast'y A + 16 FMA = 256 FLOPs.
    /// Arithmetic intensity 4 FMAs per 1 B load — 4x lepsze niz microkernel 1xN,
    /// eliminuje wielokrotne re-czytanie B z L3.
    ///
    /// # Safety
    /// Wymaga AVX-512F. Caller gwarantuje `a_rows` ma >=4 wiersze po k elementow,
    /// `c_rows` ma >=4 wiersze po n elementow z row_stride=n, a N jest wielokrotnoscia 64.
    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_4row_tile_avx512(
        a_rows: *const f32, // 4 kolejne wiersze A, kazdy dlugosci k (stride=k)
        b: *const f32,      // [K, N] row major
        c_rows: *mut f32,   // 4 kolejne wiersze C, stride=n
        n: usize,
        k: usize,
        bias: [f32; 4],
    ) {
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let a0 = a_rows;
        let a1 = a_rows.add(k);
        let a2 = a_rows.add(2 * k);
        let a3 = a_rows.add(3 * k);

        let c0 = c_rows;
        let c1 = c_rows.add(n);
        let c2 = c_rows.add(2 * n);
        let c3 = c_rows.add(3 * n);

        let bias0 = _mm512_set1_ps(bias[0]);
        let bias1 = _mm512_set1_ps(bias[1]);
        let bias2 = _mm512_set1_ps(bias[2]);
        let bias3 = _mm512_set1_ps(bias[3]);

        // Main loop: 64 columns at a time = 4 zmm per row
        let mut j = 0;
        while j < n64 {
            // 16 zmm accumulators for 4 rows × 4 zmm each
            let mut acc00 = bias0; let mut acc01 = bias0; let mut acc02 = bias0; let mut acc03 = bias0;
            let mut acc10 = bias1; let mut acc11 = bias1; let mut acc12 = bias1; let mut acc13 = bias1;
            let mut acc20 = bias2; let mut acc21 = bias2; let mut acc22 = bias2; let mut acc23 = bias2;
            let mut acc30 = bias3; let mut acc31 = bias3; let mut acc32 = bias3; let mut acc33 = bias3;

            for kk in 0..k {
                let b_base = b.add(kk * n + j);
                let b0 = _mm512_loadu_ps(b_base);
                let b1 = _mm512_loadu_ps(b_base.add(16));
                let b2 = _mm512_loadu_ps(b_base.add(32));
                let b3 = _mm512_loadu_ps(b_base.add(48));

                let a0s = _mm512_set1_ps(*a0.add(kk));
                acc00 = _mm512_fmadd_ps(a0s, b0, acc00);
                acc01 = _mm512_fmadd_ps(a0s, b1, acc01);
                acc02 = _mm512_fmadd_ps(a0s, b2, acc02);
                acc03 = _mm512_fmadd_ps(a0s, b3, acc03);

                let a1s = _mm512_set1_ps(*a1.add(kk));
                acc10 = _mm512_fmadd_ps(a1s, b0, acc10);
                acc11 = _mm512_fmadd_ps(a1s, b1, acc11);
                acc12 = _mm512_fmadd_ps(a1s, b2, acc12);
                acc13 = _mm512_fmadd_ps(a1s, b3, acc13);

                let a2s = _mm512_set1_ps(*a2.add(kk));
                acc20 = _mm512_fmadd_ps(a2s, b0, acc20);
                acc21 = _mm512_fmadd_ps(a2s, b1, acc21);
                acc22 = _mm512_fmadd_ps(a2s, b2, acc22);
                acc23 = _mm512_fmadd_ps(a2s, b3, acc23);

                let a3s = _mm512_set1_ps(*a3.add(kk));
                acc30 = _mm512_fmadd_ps(a3s, b0, acc30);
                acc31 = _mm512_fmadd_ps(a3s, b1, acc31);
                acc32 = _mm512_fmadd_ps(a3s, b2, acc32);
                acc33 = _mm512_fmadd_ps(a3s, b3, acc33);
            }

            _mm512_storeu_ps(c0.add(j), acc00);
            _mm512_storeu_ps(c0.add(j + 16), acc01);
            _mm512_storeu_ps(c0.add(j + 32), acc02);
            _mm512_storeu_ps(c0.add(j + 48), acc03);
            _mm512_storeu_ps(c1.add(j), acc10);
            _mm512_storeu_ps(c1.add(j + 16), acc11);
            _mm512_storeu_ps(c1.add(j + 32), acc12);
            _mm512_storeu_ps(c1.add(j + 48), acc13);
            _mm512_storeu_ps(c2.add(j), acc20);
            _mm512_storeu_ps(c2.add(j + 16), acc21);
            _mm512_storeu_ps(c2.add(j + 32), acc22);
            _mm512_storeu_ps(c2.add(j + 48), acc23);
            _mm512_storeu_ps(c3.add(j), acc30);
            _mm512_storeu_ps(c3.add(j + 16), acc31);
            _mm512_storeu_ps(c3.add(j + 32), acc32);
            _mm512_storeu_ps(c3.add(j + 48), acc33);

            j += 64;
        }

        // Tail 16: single zmm per row
        while j < n16 {
            let mut a0acc = bias0;
            let mut a1acc = bias1;
            let mut a2acc = bias2;
            let mut a3acc = bias3;
            for kk in 0..k {
                let b_v = _mm512_loadu_ps(b.add(kk * n + j));
                a0acc = _mm512_fmadd_ps(_mm512_set1_ps(*a0.add(kk)), b_v, a0acc);
                a1acc = _mm512_fmadd_ps(_mm512_set1_ps(*a1.add(kk)), b_v, a1acc);
                a2acc = _mm512_fmadd_ps(_mm512_set1_ps(*a2.add(kk)), b_v, a2acc);
                a3acc = _mm512_fmadd_ps(_mm512_set1_ps(*a3.add(kk)), b_v, a3acc);
            }
            _mm512_storeu_ps(c0.add(j), a0acc);
            _mm512_storeu_ps(c1.add(j), a1acc);
            _mm512_storeu_ps(c2.add(j), a2acc);
            _mm512_storeu_ps(c3.add(j), a3acc);
            j += 16;
        }

        // Tail skalarny
        while j < n {
            let mut s0 = bias[0];
            let mut s1 = bias[1];
            let mut s2 = bias[2];
            let mut s3 = bias[3];
            for kk in 0..k {
                let b_val = *b.add(kk * n + j);
                s0 += *a0.add(kk) * b_val;
                s1 += *a1.add(kk) * b_val;
                s2 += *a2.add(kk) * b_val;
                s3 += *a3.add(kk) * b_val;
            }
            *c0.add(j) = s0;
            *c1.add(j) = s1;
            *c2.add(j) = s2;
            *c3.add(j) = s3;
            j += 1;
        }
    }

    /// Accumulate wersja 4-row × 64-col AVX-512: C += A * B (bez bias init).
    #[target_feature(enable = "avx512f")]
    pub unsafe fn gemm_4row_tile_accumulate_avx512(
        a_rows: *const f32,
        b: *const f32,
        c_rows: *mut f32,
        n: usize,
        k: usize,
    ) {
        let n64 = n - (n % 64);
        let n16 = n - (n % 16);

        let a0 = a_rows;
        let a1 = a_rows.add(k);
        let a2 = a_rows.add(2 * k);
        let a3 = a_rows.add(3 * k);
        let c0 = c_rows;
        let c1 = c_rows.add(n);
        let c2 = c_rows.add(2 * n);
        let c3 = c_rows.add(3 * n);

        let mut j = 0;
        while j < n64 {
            let mut acc00 = _mm512_loadu_ps(c0.add(j));
            let mut acc01 = _mm512_loadu_ps(c0.add(j + 16));
            let mut acc02 = _mm512_loadu_ps(c0.add(j + 32));
            let mut acc03 = _mm512_loadu_ps(c0.add(j + 48));
            let mut acc10 = _mm512_loadu_ps(c1.add(j));
            let mut acc11 = _mm512_loadu_ps(c1.add(j + 16));
            let mut acc12 = _mm512_loadu_ps(c1.add(j + 32));
            let mut acc13 = _mm512_loadu_ps(c1.add(j + 48));
            let mut acc20 = _mm512_loadu_ps(c2.add(j));
            let mut acc21 = _mm512_loadu_ps(c2.add(j + 16));
            let mut acc22 = _mm512_loadu_ps(c2.add(j + 32));
            let mut acc23 = _mm512_loadu_ps(c2.add(j + 48));
            let mut acc30 = _mm512_loadu_ps(c3.add(j));
            let mut acc31 = _mm512_loadu_ps(c3.add(j + 16));
            let mut acc32 = _mm512_loadu_ps(c3.add(j + 32));
            let mut acc33 = _mm512_loadu_ps(c3.add(j + 48));

            for kk in 0..k {
                let b_base = b.add(kk * n + j);
                let b0 = _mm512_loadu_ps(b_base);
                let b1 = _mm512_loadu_ps(b_base.add(16));
                let b2 = _mm512_loadu_ps(b_base.add(32));
                let b3 = _mm512_loadu_ps(b_base.add(48));

                let a0s = _mm512_set1_ps(*a0.add(kk));
                acc00 = _mm512_fmadd_ps(a0s, b0, acc00);
                acc01 = _mm512_fmadd_ps(a0s, b1, acc01);
                acc02 = _mm512_fmadd_ps(a0s, b2, acc02);
                acc03 = _mm512_fmadd_ps(a0s, b3, acc03);

                let a1s = _mm512_set1_ps(*a1.add(kk));
                acc10 = _mm512_fmadd_ps(a1s, b0, acc10);
                acc11 = _mm512_fmadd_ps(a1s, b1, acc11);
                acc12 = _mm512_fmadd_ps(a1s, b2, acc12);
                acc13 = _mm512_fmadd_ps(a1s, b3, acc13);

                let a2s = _mm512_set1_ps(*a2.add(kk));
                acc20 = _mm512_fmadd_ps(a2s, b0, acc20);
                acc21 = _mm512_fmadd_ps(a2s, b1, acc21);
                acc22 = _mm512_fmadd_ps(a2s, b2, acc22);
                acc23 = _mm512_fmadd_ps(a2s, b3, acc23);

                let a3s = _mm512_set1_ps(*a3.add(kk));
                acc30 = _mm512_fmadd_ps(a3s, b0, acc30);
                acc31 = _mm512_fmadd_ps(a3s, b1, acc31);
                acc32 = _mm512_fmadd_ps(a3s, b2, acc32);
                acc33 = _mm512_fmadd_ps(a3s, b3, acc33);
            }

            _mm512_storeu_ps(c0.add(j), acc00);
            _mm512_storeu_ps(c0.add(j + 16), acc01);
            _mm512_storeu_ps(c0.add(j + 32), acc02);
            _mm512_storeu_ps(c0.add(j + 48), acc03);
            _mm512_storeu_ps(c1.add(j), acc10);
            _mm512_storeu_ps(c1.add(j + 16), acc11);
            _mm512_storeu_ps(c1.add(j + 32), acc12);
            _mm512_storeu_ps(c1.add(j + 48), acc13);
            _mm512_storeu_ps(c2.add(j), acc20);
            _mm512_storeu_ps(c2.add(j + 16), acc21);
            _mm512_storeu_ps(c2.add(j + 32), acc22);
            _mm512_storeu_ps(c2.add(j + 48), acc23);
            _mm512_storeu_ps(c3.add(j), acc30);
            _mm512_storeu_ps(c3.add(j + 16), acc31);
            _mm512_storeu_ps(c3.add(j + 32), acc32);
            _mm512_storeu_ps(c3.add(j + 48), acc33);

            j += 64;
        }

        while j < n16 {
            let mut a0acc = _mm512_loadu_ps(c0.add(j));
            let mut a1acc = _mm512_loadu_ps(c1.add(j));
            let mut a2acc = _mm512_loadu_ps(c2.add(j));
            let mut a3acc = _mm512_loadu_ps(c3.add(j));
            for kk in 0..k {
                let b_v = _mm512_loadu_ps(b.add(kk * n + j));
                a0acc = _mm512_fmadd_ps(_mm512_set1_ps(*a0.add(kk)), b_v, a0acc);
                a1acc = _mm512_fmadd_ps(_mm512_set1_ps(*a1.add(kk)), b_v, a1acc);
                a2acc = _mm512_fmadd_ps(_mm512_set1_ps(*a2.add(kk)), b_v, a2acc);
                a3acc = _mm512_fmadd_ps(_mm512_set1_ps(*a3.add(kk)), b_v, a3acc);
            }
            _mm512_storeu_ps(c0.add(j), a0acc);
            _mm512_storeu_ps(c1.add(j), a1acc);
            _mm512_storeu_ps(c2.add(j), a2acc);
            _mm512_storeu_ps(c3.add(j), a3acc);
            j += 16;
        }

        while j < n {
            let mut s0 = *c0.add(j);
            let mut s1 = *c1.add(j);
            let mut s2 = *c2.add(j);
            let mut s3 = *c3.add(j);
            for kk in 0..k {
                let b_val = *b.add(kk * n + j);
                s0 += *a0.add(kk) * b_val;
                s1 += *a1.add(kk) * b_val;
                s2 += *a2.add(kk) * b_val;
                s3 += *a3.add(kk) * b_val;
            }
            *c0.add(j) = s0;
            *c1.add(j) = s1;
            *c2.add(j) = s2;
            *c3.add(j) = s3;
            j += 1;
        }
    }

    /// Dot product 4-way unrolled AVX-512 — hiding FMA latency z 4 niezaleznymi
    /// akumulatorami. Zwraca sum(row[i] * x[i] for i in 0..k).
    #[target_feature(enable = "avx512f")]
    pub unsafe fn dot_product_avx512(row: &[f32], x: &[f32], k: usize) -> f32 {
        let mut acc0 = _mm512_setzero_ps();
        let mut acc1 = _mm512_setzero_ps();
        let mut acc2 = _mm512_setzero_ps();
        let mut acc3 = _mm512_setzero_ps();

        let k64 = k - (k % 64);
        let k16 = k - (k % 16);

        let mut i = 0;
        while i < k64 {
            let r0 = _mm512_loadu_ps(row.as_ptr().add(i));
            let r1 = _mm512_loadu_ps(row.as_ptr().add(i + 16));
            let r2 = _mm512_loadu_ps(row.as_ptr().add(i + 32));
            let r3 = _mm512_loadu_ps(row.as_ptr().add(i + 48));

            let x0 = _mm512_loadu_ps(x.as_ptr().add(i));
            let x1 = _mm512_loadu_ps(x.as_ptr().add(i + 16));
            let x2 = _mm512_loadu_ps(x.as_ptr().add(i + 32));
            let x3 = _mm512_loadu_ps(x.as_ptr().add(i + 48));

            acc0 = _mm512_fmadd_ps(r0, x0, acc0);
            acc1 = _mm512_fmadd_ps(r1, x1, acc1);
            acc2 = _mm512_fmadd_ps(r2, x2, acc2);
            acc3 = _mm512_fmadd_ps(r3, x3, acc3);

            i += 64;
        }

        while i < k16 {
            let r = _mm512_loadu_ps(row.as_ptr().add(i));
            let x_v = _mm512_loadu_ps(x.as_ptr().add(i));
            acc0 = _mm512_fmadd_ps(r, x_v, acc0);
            i += 16;
        }

        let combined = _mm512_add_ps(_mm512_add_ps(acc0, acc1), _mm512_add_ps(acc2, acc3));
        let mut sum = _mm512_reduce_add_ps(combined);

        while i < k {
            sum += *row.get_unchecked(i) * *x.get_unchecked(i);
            i += 1;
        }
        sum
    }
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
    // Rozmiar bloku wierszy dla register blocking 4xN (AVX-512)
    // Dead on non-x86_64 (AVX-512 path is cfg-gated to `false` upstream).
    #[allow(dead_code)]
    const MR: usize = 4;

    // Fast path: M>=4, AVX-512F, uzywamy 4-row microkernela.
    // Arithmetic intensity 4 FMAs per 1 B load = 4x mniejsze re-czytanie B z L3.
    #[cfg(target_arch = "x86_64")]
    if m >= MR && has_avx512f() {
        let m_blocks = m / MR;
        let m_tail = m % MR;

        // Rownolegle po blokach 4-wierszowych — par_chunks_mut(MR*n) daje
        // disjoint slice'y po (MR wierszy × n kolumn) dla kazdego watka.
        let blocks_area = m_blocks * MR * n;
        c[..blocks_area]
            .par_chunks_mut(MR * n)
            .enumerate()
            .for_each(|(block_idx, c_tile)| {
                let a_tile_start = block_idx * MR * k;
                let bias_vals = match bias {
                    Some(bs) => {
                        let base = block_idx * MR;
                        [bs[base], bs[base + 1], bs[base + 2], bs[base + 3]]
                    }
                    None => [0.0; MR],
                };
                unsafe {
                    avx512::gemm_4row_tile_avx512(
                        a.as_ptr().add(a_tile_start),
                        b.as_ptr(),
                        c_tile.as_mut_ptr(),
                        n,
                        k,
                        bias_vals,
                    );
                }
            });

        // Tail rows (m % 4) — single-row microkernel
        if m_tail > 0 {
            let tail_start = m_blocks * MR;
            for i in 0..m_tail {
                let m_idx = tail_start + i;
                let c_row = &mut c[m_idx * n..(m_idx + 1) * n];
                let a_row = &a[m_idx * k..(m_idx + 1) * k];
                let bias_val = bias.map(|bs| bs[m_idx]).unwrap_or(0.0);
                gemm_one_row(a_row, b, c_row, n, k, bias_val);
            }
        }
        return;
    }

    if m < RAYON_THRESHOLD {
        for m_idx in 0..m {
            let c_row = &mut c[m_idx * n..(m_idx + 1) * n];
            let a_row = &a[m_idx * k..(m_idx + 1) * k];
            let bias_val = bias.map(|bs| bs[m_idx]).unwrap_or(0.0);
            gemm_one_row(a_row, b, c_row, n, k, bias_val);
        }
    } else {
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
/// Dispatch:
///  - AVX-512 (zmm, 16-wide): 64 kolumny naraz, 4 zmm akumulatory
///  - AVX2 (ymm, 8-wide):     32 kolumny naraz, 4 ymm akumulatory
/// Oba trzymaja akumulatory w rejestrach przez cala petle K.
#[inline]
fn gemm_one_row(a_row: &[f32], b: &[f32], c_row: &mut [f32], n: usize, k: usize, bias_val: f32) {
    #[cfg(target_arch = "x86_64")]
    if has_avx512f() {
        unsafe { avx512::gemm_one_row_avx512(a_row, b, c_row, n, k, bias_val); }
        return;
    }
    gemm_one_row_f32x8(a_row, b, c_row, n, k, bias_val);
}

/// Portable AVX2/NEON microkernel (wide::f32x8 8-wide). Dziala wszedzie gdzie
/// LLVM dostarcza 256-bit SIMD — aarch64 NEON (2x 128-bit), x86_64 AVX/AVX2,
/// wasm32 SIMD128 (2x 128-bit).
#[inline]
fn gemm_one_row_f32x8(a_row: &[f32], b: &[f32], c_row: &mut [f32], n: usize, k: usize, bias_val: f32) {
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
    // Dead on non-x86_64 (AVX-512 path is cfg-gated to `false` upstream).
    #[allow(dead_code)]
    const MR: usize = 4;

    #[cfg(target_arch = "x86_64")]
    if m >= MR && has_avx512f() {
        let m_blocks = m / MR;
        let m_tail = m % MR;

        let blocks_area = m_blocks * MR * n;
        c[..blocks_area]
            .par_chunks_mut(MR * n)
            .enumerate()
            .for_each(|(block_idx, c_tile)| {
                let a_tile_start = block_idx * MR * k;
                unsafe {
                    avx512::gemm_4row_tile_accumulate_avx512(
                        a.as_ptr().add(a_tile_start),
                        b.as_ptr(),
                        c_tile.as_mut_ptr(),
                        n,
                        k,
                    );
                }
            });

        if m_tail > 0 {
            let tail_start = m_blocks * MR;
            for i in 0..m_tail {
                let m_idx = tail_start + i;
                let a_row = &a[m_idx * k..(m_idx + 1) * k];
                let c_row = &mut c[m_idx * n..(m_idx + 1) * n];
                gemm_one_row_accumulate(a_row, b, c_row, n, k);
            }
        }
        return;
    }

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
    #[cfg(target_arch = "x86_64")]
    if has_avx512f() {
        unsafe { avx512::gemm_one_row_accumulate_avx512(a_row, b, c_row, n, k); }
        return;
    }
    gemm_one_row_accumulate_f32x8(a_row, b, c_row, n, k);
}

#[inline]
fn gemm_one_row_accumulate_f32x8(a_row: &[f32], b: &[f32], c_row: &mut [f32], n: usize, k: usize) {
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

/// GEMM accumulate z stride'ami — C[:, :n] += A * B[:, :n], gdzie B ma row
/// stride `b_row_stride` (zwykle >= n) a C ma row stride `c_row_stride`.
///
/// Kluczowe zastosowanie: Conv1D k>1 bez alokacji. Zamiast pakowac input
/// slice do contiguous temp, podajemy input bezposrednio z row_stride=in_length
/// i uzywamy tylko valid_n kolumn zaczynajac od t_in_start.
pub fn gemm_accumulate_strided(
    a: &[f32],
    b: &[f32],
    b_row_stride: usize,
    c: &mut [f32],
    c_row_stride: usize,
    m: usize,
    n: usize,
    k: usize,
) {
    debug_assert!(b_row_stride >= n);
    debug_assert!(c_row_stride >= n);
    debug_assert_eq!(a.len(), m * k);

    // Dead on non-x86_64 (AVX-512 path is cfg-gated to `false` upstream).
    #[allow(dead_code)]
    const MR: usize = 4;

    // Fast path: 4-row AVX-512 strided kernel. Single-threaded bo zwykle m
    // jest male (64 Res2, 512 layer1) a rayon overhead > benefit.
    #[cfg(target_arch = "x86_64")]
    if m >= MR && has_avx512f() {
        let m_blocks = m / MR;
        let m_tail = m % MR;
        for block_idx in 0..m_blocks {
            let a_tile_start = block_idx * MR * k;
            let c_tile_start = block_idx * MR * c_row_stride;
            unsafe {
                avx512::gemm_4row_tile_accumulate_strided_avx512(
                    a.as_ptr().add(a_tile_start),
                    b.as_ptr(),
                    b_row_stride,
                    c.as_mut_ptr().add(c_tile_start),
                    c_row_stride,
                    n,
                    k,
                );
            }
        }
        if m_tail > 0 {
            let tail_start = m_blocks * MR;
            for i in 0..m_tail {
                let m_idx = tail_start + i;
                let a_row = &a[m_idx * k..(m_idx + 1) * k];
                let c_start = m_idx * c_row_stride;
                let c_row = &mut c[c_start..c_start + n];
                gemm_one_row_accumulate_strided(a_row, b, b_row_stride, c_row, n, k);
            }
        }
        return;
    }

    // Fallback: single-row microkernel (AVX2/NEON path)
    for m_idx in 0..m {
        let a_row = &a[m_idx * k..(m_idx + 1) * k];
        let c_start = m_idx * c_row_stride;
        let c_row = &mut c[c_start..c_start + n];
        gemm_one_row_accumulate_strided(a_row, b, b_row_stride, c_row, n, k);
    }
}

#[inline]
fn gemm_one_row_accumulate_strided(
    a_row: &[f32],
    b: &[f32],
    b_row_stride: usize,
    c_row: &mut [f32],
    n: usize,
    k: usize,
) {
    #[cfg(target_arch = "x86_64")]
    if has_avx512f() {
        unsafe {
            avx512::gemm_one_row_accumulate_strided_avx512(a_row, b, b_row_stride, c_row, n, k);
        }
        return;
    }
    gemm_one_row_accumulate_strided_f32x8(a_row, b, b_row_stride, c_row, n, k);
}

#[inline]
fn gemm_one_row_accumulate_strided_f32x8(
    a_row: &[f32],
    b: &[f32],
    b_row_stride: usize,
    c_row: &mut [f32],
    n: usize,
    k: usize,
) {
    let n32 = n - (n % 32);
    let n8 = n - (n % 8);

    let mut j = 0;
    while j < n32 {
        let mut acc0 = load8(&c_row[j..j + 8]);
        let mut acc1 = load8(&c_row[j + 8..j + 16]);
        let mut acc2 = load8(&c_row[j + 16..j + 24]);
        let mut acc3 = load8(&c_row[j + 24..j + 32]);

        for kk in 0..k {
            let a_s = f32x8::splat(a_row[kk]);
            let b_base = kk * b_row_stride + j;
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
            let b_vec = load8(&b[kk * b_row_stride + j..kk * b_row_stride + j + 8]);
            acc = a_s.mul_add(b_vec, acc);
        }
        store8(&mut c_row[j..j + 8], acc);
        j += 8;
    }

    while j < n {
        let mut sum = c_row[j];
        for kk in 0..k {
            sum += a_row[kk] * b[kk * b_row_stride + j];
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
    #[cfg(target_arch = "x86_64")]
    if has_avx512f() {
        let _ = k8;
        return unsafe { avx512::dot_product_avx512(row, x, k) };
    }
    dot_product_f32x8(row, x, k8, k)
}

#[inline]
fn dot_product_f32x8(row: &[f32], x: &[f32], k8: usize, k: usize) -> f32 {
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
