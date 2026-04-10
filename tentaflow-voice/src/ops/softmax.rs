// =============================================================================
// Plik: ops/softmax.rs
// Opis: Softmax wzdluz osi czasowej dla tensora [C, T] (per channel).
//       Numerically stable: subtract max before exp.
// =============================================================================

/// Softmax po axis=last (time) per kanal.
/// Dla [C, T]: out[c, t] = exp(in[c, t] - max[c]) / sum_t exp(in[c, t'] - max[c])
pub fn softmax_axis_last(data: &mut [f32], num_channels: usize, length: usize) {
    debug_assert_eq!(data.len(), num_channels * length);
    for c in 0..num_channels {
        let row_start = c * length;
        let row = &mut data[row_start..row_start + length];

        // Max
        let max_val = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        // Exp(x - max), suma
        let mut sum = 0.0_f32;
        for v in row.iter_mut() {
            *v = (*v - max_val).exp();
            sum += *v;
        }

        // Normalizacja
        if sum > 0.0 {
            let inv = 1.0 / sum;
            for v in row.iter_mut() {
                *v *= inv;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_uniform_input() {
        let mut data = vec![1.0, 1.0, 1.0, 1.0];
        softmax_axis_last(&mut data, 1, 4);
        for v in &data {
            assert!((*v - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn softmax_sum_to_one() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0];
        softmax_axis_last(&mut data, 1, 4);
        let s: f32 = data.iter().sum();
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn softmax_per_channel_independent() {
        // 2 kanaly, kazdy normalizowany niezaleznie
        let mut data = vec![
            1.0, 1.0, 1.0,    // ch0
            5.0, 5.0, 5.0,    // ch1
        ];
        softmax_axis_last(&mut data, 2, 3);
        for v in &data {
            assert!((*v - 1.0/3.0).abs() < 1e-6);
        }
    }
}
