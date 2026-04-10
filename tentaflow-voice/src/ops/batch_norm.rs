// =============================================================================
// Plik: ops/batch_norm.rs
// Opis: BatchNorm1D inference forward pass.
//       y[c, t] = gamma[c] * (x[c, t] - running_mean[c]) / sqrt(running_var[c] + eps) + beta[c]
//
//       W praktyce dla speedup pre-computujemy: scale = gamma / sqrt(var + eps)
//       i shift = beta - scale * mean, wtedy: y = scale * x + shift.
// =============================================================================

/// Parametry BatchNorm1D pre-obliczone do formy affine: y = scale*x + shift
pub struct BatchNorm1dFused {
    pub num_features: usize,
    pub scale: Vec<f32>,
    pub shift: Vec<f32>,
}

impl BatchNorm1dFused {
    /// Fuzuje gamma/beta/mean/var w scale i shift.
    pub fn new(gamma: &[f32], beta: &[f32], mean: &[f32], var: &[f32], eps: f32) -> Self {
        let n = gamma.len();
        assert_eq!(beta.len(), n);
        assert_eq!(mean.len(), n);
        assert_eq!(var.len(), n);

        let mut scale = vec![0.0_f32; n];
        let mut shift = vec![0.0_f32; n];
        for c in 0..n {
            let s = gamma[c] / (var[c] + eps).sqrt();
            scale[c] = s;
            shift[c] = beta[c] - s * mean[c];
        }

        Self {
            num_features: n,
            scale,
            shift,
        }
    }

    /// Aplikuje in-place: y[c, t] = scale[c] * y[c, t] + shift[c]
    /// dla tensora o shape [num_features, length]
    pub fn apply(&self, data: &mut [f32], length: usize) {
        debug_assert_eq!(data.len(), self.num_features * length);
        for c in 0..self.num_features {
            let s = self.scale[c];
            let b = self.shift[c];
            let offset = c * length;
            for t in 0..length {
                data[offset + t] = s * data[offset + t] + b;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batchnorm_identity_gamma_one_beta_zero() {
        // gamma=1, beta=0, mean=0, var=1 → y = x (identity)
        let bn = BatchNorm1dFused::new(&[1.0, 1.0], &[0.0, 0.0], &[0.0, 0.0], &[1.0, 1.0], 1e-5);
        let mut data = vec![1.0, 2.0, 3.0, 4.0];
        bn.apply(&mut data, 2);
        // Z numeryczna tolerancja epsilon
        for (i, &v) in data.iter().enumerate() {
            let expected = (i + 1) as f32;
            assert!((v - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn batchnorm_shift_and_scale() {
        // Scale=2, shift=10
        let bn = BatchNorm1dFused::new(&[2.0], &[10.0], &[0.0], &[1.0], 0.0);
        let mut data = vec![0.0, 1.0, 2.0];
        bn.apply(&mut data, 3);
        assert_eq!(data, vec![10.0, 12.0, 14.0]);
    }
}
