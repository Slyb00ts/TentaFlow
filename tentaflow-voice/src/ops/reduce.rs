// =============================================================================
// Plik: ops/reduce.rs
// Opis: Reduce operacje (mean, sum, std) — uzywane w pooling i attentive stats.
// =============================================================================

/// Mean axis=2 dla tensora 3D [C, T] -> [C]
/// Lub dla [B, C, T] traktowany jako [C, T] gdy batch=1.
pub fn mean_axis_last(data: &[f32], num_channels: usize, length: usize) -> Vec<f32> {
    debug_assert_eq!(data.len(), num_channels * length);
    let mut out = vec![0.0_f32; num_channels];
    let inv_n = 1.0 / length as f32;
    for c in 0..num_channels {
        let mut sum = 0.0;
        for t in 0..length {
            sum += data[c * length + t];
        }
        out[c] = sum * inv_n;
    }
    out
}

/// Sum axis=last [C, T] -> [C]
pub fn sum_axis_last(data: &[f32], num_channels: usize, length: usize) -> Vec<f32> {
    debug_assert_eq!(data.len(), num_channels * length);
    let mut out = vec![0.0_f32; num_channels];
    for c in 0..num_channels {
        let mut sum = 0.0;
        for t in 0..length {
            sum += data[c * length + t];
        }
        out[c] = sum;
    }
    out
}

/// Weighted mean axis=last z wagami [C, T] * w[T] (broadcasted)
/// out[c] = sum_t (data[c, t] * w[t])
pub fn weighted_mean(data: &[f32], weights: &[f32], num_channels: usize, length: usize) -> Vec<f32> {
    debug_assert_eq!(data.len(), num_channels * length);
    debug_assert_eq!(weights.len(), num_channels * length); // same shape jak data dla per-channel attention
    let mut out = vec![0.0_f32; num_channels];
    for c in 0..num_channels {
        let mut sum = 0.0;
        for t in 0..length {
            sum += data[c * length + t] * weights[c * length + t];
        }
        out[c] = sum;
    }
    out
}

/// Weighted std axis=last — zgodnie z WeSpeaker ONNX: clip(var, min=eps) → sqrt
/// out[c] = sqrt(max(sum_t (data[c, t]^2 * w[c, t]) - mean[c]^2, eps))
pub fn weighted_std(
    data: &[f32],
    weights: &[f32],
    means: &[f32],
    num_channels: usize,
    length: usize,
    eps: f32,
) -> Vec<f32> {
    debug_assert_eq!(data.len(), num_channels * length);
    debug_assert_eq!(weights.len(), num_channels * length);
    debug_assert_eq!(means.len(), num_channels);
    let mut out = vec![0.0_f32; num_channels];
    for c in 0..num_channels {
        let mut sum_sq = 0.0;
        for t in 0..length {
            let v = data[c * length + t];
            sum_sq += v * v * weights[c * length + t];
        }
        let var = sum_sq - means[c] * means[c];
        out[c] = var.max(eps).sqrt();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_axis_last_simple() {
        // [2 channels, 3 time]: ch0=[1,2,3] mean=2, ch1=[4,5,6] mean=5
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let result = mean_axis_last(&data, 2, 3);
        assert_eq!(result, vec![2.0, 5.0]);
    }

    #[test]
    fn sum_axis_last_simple() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(sum_axis_last(&data, 2, 3), vec![6.0, 15.0]);
    }
}
