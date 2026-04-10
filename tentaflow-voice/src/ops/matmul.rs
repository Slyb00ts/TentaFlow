// =============================================================================
// Plik: ops/matmul.rs
// Opis: Mnozenie macierz-wektor (matvec) — naiwne + SIMD f32x8 przez `wide`.
//       Uzywane przez linear layers w LSTM i wyjsciowych.
// =============================================================================

use wide::f32x8;

/// Naiwna implementacja — dla referencji i malych rozmiarow.
/// out[m] = sum_k matrix[m*k..m*k+k] * vec[k]
/// matrix shape: [m_rows, k_cols]
#[inline]
pub fn matvec_f32(matrix: &[f32], vec: &[f32], m_rows: usize, k_cols: usize, out: &mut [f32]) {
    debug_assert_eq!(matrix.len(), m_rows * k_cols);
    debug_assert_eq!(vec.len(), k_cols);
    debug_assert_eq!(out.len(), m_rows);

    for m in 0..m_rows {
        let row = &matrix[m * k_cols..(m + 1) * k_cols];
        let mut sum = 0.0_f32;
        for k in 0..k_cols {
            sum += row[k] * vec[k];
        }
        out[m] = sum;
    }
}

/// SIMD version — f32x8 (256-bit) dot product, portable przez `wide`.
/// Dziala na x86_64 (SSE/AVX), aarch64 (NEON), wasm32 (SIMD128) itd.
///
/// Dla m_rows iteracji liczymy dot product row*vec przez chunks po 8 elementow,
/// ze scalar tail gdy k_cols nie jest wielokrotnoscia 8.
pub fn matvec_f32_simd(
    matrix: &[f32],
    vec: &[f32],
    m_rows: usize,
    k_cols: usize,
    out: &mut [f32],
) {
    debug_assert_eq!(matrix.len(), m_rows * k_cols);
    debug_assert_eq!(vec.len(), k_cols);
    debug_assert_eq!(out.len(), m_rows);

    let simd_width = 8;
    let simd_chunks = k_cols / simd_width;
    let tail_start = simd_chunks * simd_width;

    for m in 0..m_rows {
        let row = &matrix[m * k_cols..(m + 1) * k_cols];
        let mut acc = f32x8::splat(0.0);

        // SIMD hot loop — 8 mnozen + 8 dodawan per iteracja
        for c in 0..simd_chunks {
            let offset = c * simd_width;
            let a = f32x8::from([
                row[offset], row[offset + 1], row[offset + 2], row[offset + 3],
                row[offset + 4], row[offset + 5], row[offset + 6], row[offset + 7],
            ]);
            let b = f32x8::from([
                vec[offset], vec[offset + 1], vec[offset + 2], vec[offset + 3],
                vec[offset + 4], vec[offset + 5], vec[offset + 6], vec[offset + 7],
            ]);
            acc += a * b;
        }

        // Horizontal sum lanes
        let lanes = acc.to_array();
        let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                    + lanes[4] + lanes[5] + lanes[6] + lanes[7];

        // Scalar tail dla pozostalych elementow
        for k in tail_start..k_cols {
            sum += row[k] * vec[k];
        }

        out[m] = sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matvec_naive_correct() {
        // Identity matrix 3x3 * [1,2,3] = [1,2,3]
        let matrix = vec![
            1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
            0.0, 0.0, 1.0,
        ];
        let vec = vec![1.0, 2.0, 3.0];
        let mut out = vec![0.0; 3];
        matvec_f32(&matrix, &vec, 3, 3, &mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn matvec_simd_matches_naive() {
        // Losowe dane, porownanie naive vs simd
        let m_rows = 16;
        let k_cols = 17; // nie wielokrotnosc 8 → testuje tail
        let matrix: Vec<f32> = (0..m_rows * k_cols).map(|i| (i as f32) * 0.1).collect();
        let vec_in: Vec<f32> = (0..k_cols).map(|i| (i as f32) * 0.2 - 1.0).collect();

        let mut out_naive = vec![0.0; m_rows];
        let mut out_simd = vec![0.0; m_rows];

        matvec_f32(&matrix, &vec_in, m_rows, k_cols, &mut out_naive);
        matvec_f32_simd(&matrix, &vec_in, m_rows, k_cols, &mut out_simd);

        for i in 0..m_rows {
            let diff = (out_naive[i] - out_simd[i]).abs();
            assert!(diff < 1e-4, "row {}: naive={}, simd={}, diff={}",
                    i, out_naive[i], out_simd[i], diff);
        }
    }

    #[test]
    fn matvec_simd_aligned() {
        // k_cols = 16 (wielokrotnosc 8)
        let m_rows = 4;
        let k_cols = 16;
        let matrix: Vec<f32> = (0..m_rows * k_cols).map(|i| (i as f32) * 0.05).collect();
        let vec_in: Vec<f32> = vec![1.0; k_cols];

        let mut out_naive = vec![0.0; m_rows];
        let mut out_simd = vec![0.0; m_rows];

        matvec_f32(&matrix, &vec_in, m_rows, k_cols, &mut out_naive);
        matvec_f32_simd(&matrix, &vec_in, m_rows, k_cols, &mut out_simd);

        for i in 0..m_rows {
            let diff = (out_naive[i] - out_simd[i]).abs();
            assert!(diff < 1e-4);
        }
    }
}
