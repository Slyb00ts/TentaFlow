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
    let mut out = vec![0.0_f32; num_channels];
    weighted_mean_into(data, weights, num_channels, length, &mut out);
    out
}

/// Zero-alloc wariant — SIMD + rayon po kanalach (duzo kanalow w WeSpeaker 1536).
pub fn weighted_mean_into(
    data: &[f32],
    weights: &[f32],
    num_channels: usize,
    length: usize,
    out: &mut [f32],
) {
    use rayon::prelude::*;
    use wide::f32x8;

    debug_assert_eq!(data.len(), num_channels * length);
    debug_assert_eq!(weights.len(), num_channels * length);
    debug_assert!(out.len() >= num_channels);

    out[..num_channels].par_iter_mut().enumerate().for_each(|(c, out_slot)| {
        let data_row = &data[c * length..(c + 1) * length];
        let w_row = &weights[c * length..(c + 1) * length];
        let mut acc0 = f32x8::splat(0.0);
        let mut acc1 = f32x8::splat(0.0);
        let mut acc2 = f32x8::splat(0.0);
        let mut acc3 = f32x8::splat(0.0);
        let n32 = length - (length % 32);
        let n8 = length - (length % 8);
        let mut i = 0;
        while i < n32 {
            let d0: [f32; 8] = data_row[i..i + 8].try_into().unwrap();
            let d1: [f32; 8] = data_row[i + 8..i + 16].try_into().unwrap();
            let d2: [f32; 8] = data_row[i + 16..i + 24].try_into().unwrap();
            let d3: [f32; 8] = data_row[i + 24..i + 32].try_into().unwrap();
            let w0: [f32; 8] = w_row[i..i + 8].try_into().unwrap();
            let w1: [f32; 8] = w_row[i + 8..i + 16].try_into().unwrap();
            let w2: [f32; 8] = w_row[i + 16..i + 24].try_into().unwrap();
            let w3: [f32; 8] = w_row[i + 24..i + 32].try_into().unwrap();
            acc0 = f32x8::from(d0).mul_add(f32x8::from(w0), acc0);
            acc1 = f32x8::from(d1).mul_add(f32x8::from(w1), acc1);
            acc2 = f32x8::from(d2).mul_add(f32x8::from(w2), acc2);
            acc3 = f32x8::from(d3).mul_add(f32x8::from(w3), acc3);
            i += 32;
        }
        while i < n8 {
            let d: [f32; 8] = data_row[i..i + 8].try_into().unwrap();
            let w: [f32; 8] = w_row[i..i + 8].try_into().unwrap();
            acc0 = f32x8::from(d).mul_add(f32x8::from(w), acc0);
            i += 8;
        }
        let comb = (acc0 + acc1) + (acc2 + acc3);
        let lanes = comb.to_array();
        let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                    + lanes[4] + lanes[5] + lanes[6] + lanes[7];
        while i < length {
            sum += data_row[i] * w_row[i];
            i += 1;
        }
        *out_slot = sum;
    });
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
    let mut out = vec![0.0_f32; num_channels];
    weighted_std_into(data, weights, means, num_channels, length, eps, &mut out);
    out
}

/// Zero-alloc wariant — SIMD + rayon po kanalach.
pub fn weighted_std_into(
    data: &[f32],
    weights: &[f32],
    means: &[f32],
    num_channels: usize,
    length: usize,
    eps: f32,
    out: &mut [f32],
) {
    use rayon::prelude::*;
    use wide::f32x8;

    debug_assert_eq!(data.len(), num_channels * length);
    debug_assert_eq!(weights.len(), num_channels * length);
    debug_assert_eq!(means.len(), num_channels);
    debug_assert!(out.len() >= num_channels);

    out[..num_channels].par_iter_mut().enumerate().for_each(|(c, out_slot)| {
        let data_row = &data[c * length..(c + 1) * length];
        let w_row = &weights[c * length..(c + 1) * length];
        let mut acc0 = f32x8::splat(0.0);
        let mut acc1 = f32x8::splat(0.0);
        let mut acc2 = f32x8::splat(0.0);
        let mut acc3 = f32x8::splat(0.0);
        let n32 = length - (length % 32);
        let n8 = length - (length % 8);
        let mut i = 0;
        while i < n32 {
            let d0: [f32; 8] = data_row[i..i + 8].try_into().unwrap();
            let d1: [f32; 8] = data_row[i + 8..i + 16].try_into().unwrap();
            let d2: [f32; 8] = data_row[i + 16..i + 24].try_into().unwrap();
            let d3: [f32; 8] = data_row[i + 24..i + 32].try_into().unwrap();
            let w0: [f32; 8] = w_row[i..i + 8].try_into().unwrap();
            let w1: [f32; 8] = w_row[i + 8..i + 16].try_into().unwrap();
            let w2: [f32; 8] = w_row[i + 16..i + 24].try_into().unwrap();
            let w3: [f32; 8] = w_row[i + 24..i + 32].try_into().unwrap();
            let dv0 = f32x8::from(d0);
            let dv1 = f32x8::from(d1);
            let dv2 = f32x8::from(d2);
            let dv3 = f32x8::from(d3);
            // sum_sq += v*v * w   (v*v w FMA v, v, then mul z w)
            let sq0 = dv0 * dv0;
            let sq1 = dv1 * dv1;
            let sq2 = dv2 * dv2;
            let sq3 = dv3 * dv3;
            acc0 = sq0.mul_add(f32x8::from(w0), acc0);
            acc1 = sq1.mul_add(f32x8::from(w1), acc1);
            acc2 = sq2.mul_add(f32x8::from(w2), acc2);
            acc3 = sq3.mul_add(f32x8::from(w3), acc3);
            i += 32;
        }
        while i < n8 {
            let d: [f32; 8] = data_row[i..i + 8].try_into().unwrap();
            let w: [f32; 8] = w_row[i..i + 8].try_into().unwrap();
            let dv = f32x8::from(d);
            let sq = dv * dv;
            acc0 = sq.mul_add(f32x8::from(w), acc0);
            i += 8;
        }
        let comb = (acc0 + acc1) + (acc2 + acc3);
        let lanes = comb.to_array();
        let mut sum_sq = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                       + lanes[4] + lanes[5] + lanes[6] + lanes[7];
        while i < length {
            let v = data_row[i];
            sum_sq += v * v * w_row[i];
            i += 1;
        }
        let var = sum_sq - means[c] * means[c];
        *out_slot = var.max(eps).sqrt();
    });
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
