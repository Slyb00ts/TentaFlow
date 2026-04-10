// =============================================================================
// Plik: ops/matmul.rs
// Opis: Mnozenie macierz-wektor (matvec) — naiwne + SIMD f32x8 przez `wide`.
//       Uzywane przez linear layers w LSTM i wyjsciowych.
// =============================================================================

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

/// SIMD matvec — deleguje do zoptymalizowanego gemv w ops::gemm. Zachowane dla
/// kompatybilnosci API. Wewnetrznie: 4-way unrolled FMA loop, contiguous loads,
/// rayon parallelizm dla >32 wierszy.
pub fn matvec_f32_simd(
    matrix: &[f32],
    vec: &[f32],
    m_rows: usize,
    k_cols: usize,
    out: &mut [f32],
) {
    super::gemm::gemv(matrix, vec, None, m_rows, k_cols, out);
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
