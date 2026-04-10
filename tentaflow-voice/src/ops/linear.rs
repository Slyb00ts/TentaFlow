// =============================================================================
// Plik: ops/linear.rs
// Opis: Linear layer — matvec + bias. Wrapper nad matmul::matvec_f32_simd.
// =============================================================================

use super::matmul::matvec_f32_simd;

/// Linear layer: out = W * x
/// weights shape: [out_dim, in_dim]
pub fn linear(weights: &[f32], x: &[f32], in_dim: usize, out_dim: usize, out: &mut [f32]) {
    matvec_f32_simd(weights, x, out_dim, in_dim, out);
}

/// Linear layer z bias: out = W * x + b
pub fn linear_bias(
    weights: &[f32],
    bias: &[f32],
    x: &[f32],
    in_dim: usize,
    out_dim: usize,
    out: &mut [f32],
) {
    debug_assert_eq!(bias.len(), out_dim);
    matvec_f32_simd(weights, x, out_dim, in_dim, out);
    for i in 0..out_dim {
        out[i] += bias[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_identity_no_bias() {
        let w = vec![
            1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
            0.0, 0.0, 1.0,
        ];
        let x = vec![5.0, 6.0, 7.0];
        let mut out = vec![0.0; 3];
        linear(&w, &x, 3, 3, &mut out);
        assert_eq!(out, vec![5.0, 6.0, 7.0]);
    }

    #[test]
    fn linear_with_bias() {
        let w = vec![
            1.0, 0.0,
            0.0, 1.0,
        ];
        let b = vec![10.0, 20.0];
        let x = vec![1.0, 2.0];
        let mut out = vec![0.0; 2];
        linear_bias(&w, &b, &x, 2, 2, &mut out);
        assert_eq!(out, vec![11.0, 22.0]);
    }
}
