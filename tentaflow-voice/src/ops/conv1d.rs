// =============================================================================
// Plik: ops/conv1d.rs
// Opis: 1D convolution forward pass — parametryzowany stride/padding/dilation.
//       Hot loop iteruje po in_channels z SIMD f32x8 accumulation.
//
// Format zgodny z PyTorch/ONNX Conv1d:
//   input:  [in_channels, in_length]          (batch=1, pomijany)
//   weight: [out_channels, in_channels, kernel_size]
//   bias:   [out_channels]  (opcjonalny)
//   output: [out_channels, out_length]
//
//   out_length = (in_length + 2*padding - dilation*(kernel_size-1) - 1) / stride + 1
// =============================================================================

use wide::f32x8;

/// Parametry Conv1D
#[derive(Debug, Clone, Copy)]
pub struct Conv1dParams {
    pub in_channels: usize,
    pub out_channels: usize,
    pub kernel_size: usize,
    pub stride: usize,
    pub padding: usize,
    pub dilation: usize,
}

impl Conv1dParams {
    /// Oblicza dlugosc wyjscia dla danej dlugosci wejscia
    pub fn output_length(&self, in_length: usize) -> usize {
        let eff_kernel = self.dilation * (self.kernel_size - 1) + 1;
        if in_length + 2 * self.padding < eff_kernel {
            return 0;
        }
        (in_length + 2 * self.padding - eff_kernel) / self.stride + 1
    }
}

/// Conv1D naiwny — dla referencji i prawidlowosci
pub fn conv1d_naive(
    input: &[f32],
    weight: &[f32],
    bias: Option<&[f32]>,
    params: &Conv1dParams,
    in_length: usize,
    output: &mut [f32],
) {
    let out_length = params.output_length(in_length);
    debug_assert_eq!(input.len(), params.in_channels * in_length);
    debug_assert_eq!(
        weight.len(),
        params.out_channels * params.in_channels * params.kernel_size
    );
    debug_assert_eq!(output.len(), params.out_channels * out_length);

    for oc in 0..params.out_channels {
        for t_out in 0..out_length {
            let mut sum = 0.0_f32;
            for ic in 0..params.in_channels {
                for k in 0..params.kernel_size {
                    let t_in = (t_out * params.stride) as i64
                        + (k * params.dilation) as i64
                        - params.padding as i64;
                    if t_in < 0 || t_in >= in_length as i64 {
                        continue;
                    }
                    let w_idx = oc * params.in_channels * params.kernel_size
                        + ic * params.kernel_size
                        + k;
                    let i_idx = ic * in_length + t_in as usize;
                    sum += weight[w_idx] * input[i_idx];
                }
            }
            if let Some(b) = bias {
                sum += b[oc];
            }
            output[oc * out_length + t_out] = sum;
        }
    }
}

/// Conv1D z SIMD — hot loop po in_channels (wektorowany).
/// Im wiecej in_channels, tym szybciej vs naive.
pub fn conv1d_simd(
    input: &[f32],
    weight: &[f32],
    bias: Option<&[f32]>,
    params: &Conv1dParams,
    in_length: usize,
    output: &mut [f32],
) {
    let out_length = params.output_length(in_length);
    let ic_total = params.in_channels;
    let ks = params.kernel_size;
    let simd_width = 8;
    let ic_simd = ic_total / simd_width;
    let ic_tail_start = ic_simd * simd_width;

    for oc in 0..params.out_channels {
        let w_oc_base = oc * ic_total * ks;
        let bias_v = bias.map(|b| b[oc]).unwrap_or(0.0);

        for t_out in 0..out_length {
            let mut acc = f32x8::splat(0.0);
            let mut scalar_acc = 0.0_f32;

            for k in 0..ks {
                let t_in = (t_out * params.stride) as i64
                    + (k * params.dilation) as i64
                    - params.padding as i64;
                if t_in < 0 || t_in >= in_length as i64 {
                    continue;
                }
                let t_in = t_in as usize;

                // SIMD hot loop po blokach in_channels
                for block in 0..ic_simd {
                    let ic_start = block * simd_width;
                    // Zbierz wagi [oc, ic_start..ic_start+8, k]
                    let w = f32x8::from([
                        weight[w_oc_base + (ic_start + 0) * ks + k],
                        weight[w_oc_base + (ic_start + 1) * ks + k],
                        weight[w_oc_base + (ic_start + 2) * ks + k],
                        weight[w_oc_base + (ic_start + 3) * ks + k],
                        weight[w_oc_base + (ic_start + 4) * ks + k],
                        weight[w_oc_base + (ic_start + 5) * ks + k],
                        weight[w_oc_base + (ic_start + 6) * ks + k],
                        weight[w_oc_base + (ic_start + 7) * ks + k],
                    ]);
                    // Input [ic_start..ic_start+8, t_in]
                    let x = f32x8::from([
                        input[(ic_start + 0) * in_length + t_in],
                        input[(ic_start + 1) * in_length + t_in],
                        input[(ic_start + 2) * in_length + t_in],
                        input[(ic_start + 3) * in_length + t_in],
                        input[(ic_start + 4) * in_length + t_in],
                        input[(ic_start + 5) * in_length + t_in],
                        input[(ic_start + 6) * in_length + t_in],
                        input[(ic_start + 7) * in_length + t_in],
                    ]);
                    acc += w * x;
                }

                // Scalar tail dla pozostalych ic
                for ic in ic_tail_start..ic_total {
                    let w_val = weight[w_oc_base + ic * ks + k];
                    let x_val = input[ic * in_length + t_in];
                    scalar_acc += w_val * x_val;
                }
            }

            // Horizontal sum
            let lanes = acc.to_array();
            let total = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                      + lanes[4] + lanes[5] + lanes[6] + lanes[7]
                      + scalar_acc + bias_v;
            output[oc * out_length + t_out] = total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv1d_output_length() {
        // Stride=1, padding=1, kernel=3 → out_length = in_length
        let p = Conv1dParams {
            in_channels: 1,
            out_channels: 1,
            kernel_size: 3,
            stride: 1,
            padding: 1,
            dilation: 1,
        };
        assert_eq!(p.output_length(10), 10);
        assert_eq!(p.output_length(100), 100);
    }

    #[test]
    fn conv1d_identity_single_channel() {
        // Kernel = [0, 1, 0] → output = input (identity)
        let params = Conv1dParams {
            in_channels: 1,
            out_channels: 1,
            kernel_size: 3,
            stride: 1,
            padding: 1,
            dilation: 1,
        };
        let input = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let weight = vec![0.0, 1.0, 0.0];
        let mut out = vec![0.0; 5];
        conv1d_naive(&input, &weight, None, &params, 5, &mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn conv1d_bias_added() {
        let params = Conv1dParams {
            in_channels: 1,
            out_channels: 1,
            kernel_size: 3,
            stride: 1,
            padding: 1,
            dilation: 1,
        };
        let input = vec![0.0, 0.0, 0.0];
        let weight = vec![1.0, 1.0, 1.0];
        let bias = vec![5.0];
        let mut out = vec![0.0; 3];
        conv1d_naive(&input, &weight, Some(&bias), &params, 3, &mut out);
        assert_eq!(out, vec![5.0, 5.0, 5.0]);
    }

    #[test]
    fn conv1d_simd_matches_naive() {
        // in_channels = 17 (z tailem), kernel=3
        let params = Conv1dParams {
            in_channels: 17,
            out_channels: 4,
            kernel_size: 3,
            stride: 1,
            padding: 1,
            dilation: 1,
        };
        let in_length = 12;
        let input: Vec<f32> = (0..params.in_channels * in_length)
            .map(|i| (i as f32) * 0.01 - 1.0)
            .collect();
        let weight: Vec<f32> = (0..params.out_channels * params.in_channels * params.kernel_size)
            .map(|i| (i as f32) * 0.001)
            .collect();
        let bias: Vec<f32> = (0..params.out_channels).map(|i| i as f32).collect();

        let out_len = params.output_length(in_length);
        let mut out_naive = vec![0.0; params.out_channels * out_len];
        let mut out_simd = vec![0.0; params.out_channels * out_len];

        conv1d_naive(&input, &weight, Some(&bias), &params, in_length, &mut out_naive);
        conv1d_simd(&input, &weight, Some(&bias), &params, in_length, &mut out_simd);

        for (i, (n, s)) in out_naive.iter().zip(out_simd.iter()).enumerate() {
            let diff = (n - s).abs();
            assert!(diff < 1e-3, "idx={}: naive={}, simd={}, diff={}", i, n, s, diff);
        }
    }

    #[test]
    fn conv1d_stride2() {
        // stride=2 → out_length ~ in_length/2
        let params = Conv1dParams {
            in_channels: 1,
            out_channels: 1,
            kernel_size: 3,
            stride: 2,
            padding: 1,
            dilation: 1,
        };
        assert_eq!(params.output_length(10), 5);
        assert_eq!(params.output_length(11), 6);
    }
}
