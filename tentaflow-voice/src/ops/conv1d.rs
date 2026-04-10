// =============================================================================
// Plik: ops/conv1d.rs
// Opis: 1D convolution forward pass — parametryzowany stride/padding/dilation.
//       Dispatches do GEMM dla k=1 (dominujacy case w WeSpeaker) albo petla
//       GEMM-accumulate po kernel positions dla k>1.
//
// Format zgodny z PyTorch/ONNX Conv1d:
//   input:  [in_channels, in_length]
//   weight: [out_channels, in_channels, kernel_size]
//   bias:   [out_channels]  (opcjonalny)
//   output: [out_channels, out_length]
//
//   out_length = (in_length + 2*padding - dilation*(kernel_size-1) - 1) / stride + 1
//
// Kluczowa obserwacja: Conv1D k=1 stride=1 to DOKLADNIE GEMM. W WeSpeaker ~80%
// FLOPow lezy wlasnie w conv k=1 (pre/post SE-Res2Block, aggregation, pool
// linears), wiec efektywny GEMM daje >5x speedup w calym modelu.
// =============================================================================

use super::gemm::{gemm, gemm_accumulate, gemm_accumulate_strided};

/// Pre-permutowana reprezentacja wag Conv1D — jedna matryca [OC, IC] na kazda
/// pozycje kernela. Dzieki temu forward pass nie wymaga zadnej alokacji ani
/// kopiowania: dla kazdego k wywolujemy GEMM bezposrednio z slice'em input'u.
///
/// Layout ONNX to [OC, IC, K]. Permutujemy do [K, OC, IC] ktore jest idealne
/// dla iteracji po kernel positions w conv1d forward.
#[derive(Debug, Clone)]
pub struct PackedConv1dWeight {
    /// Jedna matryca [OC * IC] na pozycje kernela, laczne K wektorow
    pub per_k: Vec<Vec<f32>>,
    pub in_channels: usize,
    pub out_channels: usize,
    pub kernel_size: usize,
}

impl PackedConv1dWeight {
    /// Konwertuje wagi z ONNX layout [OC, IC, K] row-major do [K][OC*IC].
    pub fn from_onnx(weight: &[f32], out_channels: usize, in_channels: usize, kernel_size: usize) -> Self {
        debug_assert_eq!(weight.len(), out_channels * in_channels * kernel_size);
        let mut per_k = Vec::with_capacity(kernel_size);
        for k in 0..kernel_size {
            let mut m = vec![0.0_f32; out_channels * in_channels];
            for oc in 0..out_channels {
                for ic in 0..in_channels {
                    // weight[oc, ic, k] = weight[oc * IC * K + ic * K + k]
                    m[oc * in_channels + ic] = weight[oc * in_channels * kernel_size + ic * kernel_size + k];
                }
            }
            per_k.push(m);
        }
        Self {
            per_k,
            in_channels,
            out_channels,
            kernel_size,
        }
    }
}

/// Zero-allocation Conv1D — uzywa pre-permutowanych wag i strided GEMM.
/// Dla kazdej pozycji kernela czyta input bezposrednio (bez kopiowania) przez
/// gemm_accumulate_strided.
pub fn conv1d_prepacked(
    packed: &PackedConv1dWeight,
    input: &[f32],
    bias: Option<&[f32]>,
    params: &Conv1dParams,
    in_length: usize,
    output: &mut [f32],
) {
    debug_assert_eq!(packed.in_channels, params.in_channels);
    debug_assert_eq!(packed.out_channels, params.out_channels);
    debug_assert_eq!(packed.kernel_size, params.kernel_size);
    debug_assert_eq!(input.len(), params.in_channels * in_length);

    let out_length = params.output_length(in_length);
    if out_length == 0 {
        return;
    }
    let m = params.out_channels;
    let k_ic = params.in_channels;
    let ks = params.kernel_size;

    // k=1 fast path: bezposredni GEMM
    if ks == 1 && params.stride == 1 && params.padding == 0 {
        gemm(&packed.per_k[0], input, output, m, in_length, k_ic, bias);
        return;
    }

    // Initialize output z bias'em lub zerami — single pass
    match bias {
        Some(b) => {
            for oc in 0..m {
                let row = &mut output[oc * out_length..(oc + 1) * out_length];
                let bv = b[oc];
                for v in row.iter_mut() {
                    *v = bv;
                }
            }
        }
        None => {
            for v in output.iter_mut() {
                *v = 0.0;
            }
        }
    }

    // Stride != 1: fallback na scalar loop (rzadko uzywane w WeSpeaker/Silero).
    if params.stride != 1 {
        let w_flat = &packed.per_k;
        for k in 0..ks {
            let shift = k as i64 * params.dilation as i64 - params.padding as i64;
            for t_out in 0..out_length {
                let t_in = (t_out * params.stride) as i64 + shift;
                if t_in < 0 || t_in >= in_length as i64 {
                    continue;
                }
                let t_in = t_in as usize;
                for oc in 0..m {
                    let mut sum = 0.0_f32;
                    for ic in 0..k_ic {
                        sum += w_flat[k][oc * k_ic + ic] * input[ic * in_length + t_in];
                    }
                    output[oc * out_length + t_out] += sum;
                }
            }
        }
        return;
    }

    // Stride=1 glowna sciezka: zero-alloc strided GEMM dla kazdej pozycji kernela
    for k in 0..ks {
        let shift = k as i64 * params.dilation as i64 - params.padding as i64;

        // Walidny zakres wyjsciowy dla tego k: t_in in [0, in_length)
        let t_out_min_i = (-shift).max(0) as usize;
        let t_out_max_i = ((in_length as i64 - 1 - shift) + 1).max(0) as usize;
        let t_out_min = t_out_min_i.min(out_length);
        let t_out_max = t_out_max_i.min(out_length);

        if t_out_min >= t_out_max {
            continue;
        }
        let valid_n = t_out_max - t_out_min;
        let t_in_start = (t_out_min as i64 + shift) as usize;

        // GEMM: C[:, t_out_min..t_out_min+valid_n] += W[k] * B[:, t_in_start..t_in_start+valid_n]
        // W[k]: [M=OC, K=IC] contiguous
        // B: input, row_stride = in_length, starts at offset t_in_start per row
        // C: output, row_stride = out_length, starts at offset t_out_min per row
        let b_view = &input[t_in_start..];
        let c_view = &mut output[t_out_min..];

        gemm_accumulate_strided(
            &packed.per_k[k],
            b_view,
            in_length,
            c_view,
            out_length,
            m,
            valid_n,
            k_ic,
        );
    }
}

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

/// Conv1D naiwny — dla referencji i prawidlowosci (testy, fallback).
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

/// Conv1D wysokowydajne — dispatch do GEMM.
///
/// Dla k=1 stride=1 padding=0: bezposredni GEMM (A=weight [OC,IC], B=input
/// [IC,T], C=output [OC,T]). To najlepszy mozliwy pattern dostepu do pamieci.
///
/// Dla k>1: petla po kernel positions, kazda pozycja to GEMM z shifted input
/// slice. Akumulujemy do output. Padding obslugiwany przez wycinanie
/// walidnego przedzialu czasowego per kernel position.
pub fn conv1d_simd(
    input: &[f32],
    weight: &[f32],
    bias: Option<&[f32]>,
    params: &Conv1dParams,
    in_length: usize,
    output: &mut [f32],
) {
    let out_length = params.output_length(in_length);
    if out_length == 0 {
        return;
    }

    // Fast path: k=1, stride=1, padding=0 — pure GEMM
    if params.kernel_size == 1 && params.stride == 1 && params.padding == 0 {
        // A = weight [OC, IC*1] = [OC, IC]
        // B = input [IC, in_length]
        // C = output [OC, out_length=in_length]
        gemm(
            weight,
            input,
            output,
            params.out_channels,
            in_length,
            params.in_channels,
            bias,
        );
        return;
    }

    // General path: k > 1 (lub stride/padding nie-trivial).
    // Obserwacja: Conv1D to suma po kernel positions niezaleznych GEMMow:
    //   out[oc, t] = sum_k W[oc, :, k] * in[:, t*stride + k*dilation - padding]
    // Dla kazdego k wywolujemy GEMM-accumulate na OVERLAP walidnych kolumn.
    let m = params.out_channels;
    let n = out_length;
    let k_ic = params.in_channels;
    let ks = params.kernel_size;

    // Initialize output z bias'em albo zerami
    match bias {
        Some(b) => {
            for oc in 0..m {
                let row = &mut output[oc * n..(oc + 1) * n];
                let bv = b[oc];
                for v in row.iter_mut() {
                    *v = bv;
                }
            }
        }
        None => {
            for v in output.iter_mut() {
                *v = 0.0;
            }
        }
    }

    // Extract weight slice W[:, :, k] dla kazdego k → [OC, IC] matrix
    // W jest [OC, IC, K] row-major → W[oc, ic, k] = weight[oc*IC*K + ic*K + k]
    // Dla stalego k chcemy slice [OC, IC] gdzie element [oc, ic] = W[oc*IC*K + ic*K + k]
    // To nie jest contiguous — musimy przepakowac.
    let mut w_k: Vec<f32> = vec![0.0; m * k_ic];

    for k in 0..ks {
        // Pack W[:, :, k] → w_k [OC, IC]
        for oc in 0..m {
            for ic in 0..k_ic {
                w_k[oc * k_ic + ic] = weight[oc * k_ic * ks + ic * ks + k];
            }
        }

        // Dla tej pozycji kernel k, okresl walidny zakres output t
        // t_in = t_out * stride + k * dilation - padding
        // t_in in [0, in_length)  ⇒  t_out in [t_out_min, t_out_max)
        let shift = k as i64 * params.dilation as i64 - params.padding as i64;
        let t_out_min = ((-shift).max(0)) as usize / params.stride
            + if (-shift).max(0) as usize % params.stride != 0 { 1 } else { 0 };
        let t_out_max_i64 = (in_length as i64 - 1 - shift) / params.stride as i64 + 1;
        let t_out_max = (t_out_max_i64.max(0) as usize).min(out_length);

        if t_out_min >= t_out_max {
            continue;
        }

        let valid_n = t_out_max - t_out_min;

        // Zbuduj shifted input slice [IC, valid_n] gdzie kolumna j to
        // input[:, t_out_min*stride + j*stride + shift]
        // Dla stride=1 to po prostu continuous slice w czasie — mozemy go
        // uniknac i podac input bezposrednio jako B z odpowiednim offsetem.
        if params.stride == 1 {
            // B = input[:, t_start..t_start + valid_n] — row i ma stride in_length
            // Ale GEMM oczekuje B jako [K, N] contiguous. Input jest [IC, in_length]
            // a my chcemy tylko kolumny [t_start..t_start+valid_n]. To NIE jest
            // continuous w pamieci pomiedzy wierszami — musimy zbudowac temp.
            let t_start = (t_out_min as i64 + shift) as usize;
            let mut b_tmp = vec![0.0_f32; k_ic * valid_n];
            for ic in 0..k_ic {
                b_tmp[ic * valid_n..(ic + 1) * valid_n]
                    .copy_from_slice(&input[ic * in_length + t_start..ic * in_length + t_start + valid_n]);
            }

            // Temp output dla tej pozycji (trafi do output[:, t_out_min..t_out_max])
            let mut c_tmp = vec![0.0_f32; m * valid_n];
            for oc in 0..m {
                c_tmp[oc * valid_n..(oc + 1) * valid_n].copy_from_slice(
                    &output[oc * out_length + t_out_min..oc * out_length + t_out_min + valid_n],
                );
            }

            gemm_accumulate(&w_k, &b_tmp, &mut c_tmp, m, valid_n, k_ic);

            for oc in 0..m {
                output[oc * out_length + t_out_min..oc * out_length + t_out_min + valid_n]
                    .copy_from_slice(&c_tmp[oc * valid_n..(oc + 1) * valid_n]);
            }
        } else {
            // Stride != 1 — fallback na scalar accumulate. Rzadkie w WeSpeaker/Silero.
            for t_out in t_out_min..t_out_max {
                let t_in = (t_out * params.stride) as i64 + shift;
                if t_in < 0 || t_in >= in_length as i64 {
                    continue;
                }
                let t_in = t_in as usize;
                for oc in 0..m {
                    let mut sum = 0.0_f32;
                    for ic in 0..k_ic {
                        sum += w_k[oc * k_ic + ic] * input[ic * in_length + t_in];
                    }
                    output[oc * out_length + t_out] += sum;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv1d_output_length() {
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
    fn conv1d_simd_matches_naive_k1() {
        // k=1 fast path (GEMM)
        let params = Conv1dParams {
            in_channels: 80,
            out_channels: 512,
            kernel_size: 1,
            stride: 1,
            padding: 0,
            dilation: 1,
        };
        let in_length = 141;
        let input: Vec<f32> = (0..params.in_channels * in_length)
            .map(|i| ((i as f32) * 0.013).sin())
            .collect();
        let weight: Vec<f32> = (0..params.out_channels * params.in_channels * params.kernel_size)
            .map(|i| ((i as f32) * 0.0007).cos())
            .collect();
        let bias: Vec<f32> = (0..params.out_channels).map(|i| i as f32 * 0.001).collect();

        let out_len = params.output_length(in_length);
        let mut out_naive = vec![0.0; params.out_channels * out_len];
        let mut out_simd = vec![0.0; params.out_channels * out_len];

        conv1d_naive(&input, &weight, Some(&bias), &params, in_length, &mut out_naive);
        conv1d_simd(&input, &weight, Some(&bias), &params, in_length, &mut out_simd);

        for (i, (n, s)) in out_naive.iter().zip(out_simd.iter()).enumerate() {
            let diff = (n - s).abs();
            assert!(diff < 1e-2, "idx={}: naive={}, simd={}, diff={}", i, n, s, diff);
        }
    }

    #[test]
    fn conv1d_simd_matches_naive_k5_pad2() {
        // WeSpeaker layer1: Conv(80→512, k=5, pad=2)
        let params = Conv1dParams {
            in_channels: 80,
            out_channels: 32, // mniejsze do testu
            kernel_size: 5,
            stride: 1,
            padding: 2,
            dilation: 1,
        };
        let in_length = 50;
        let input: Vec<f32> = (0..params.in_channels * in_length)
            .map(|i| ((i as f32) * 0.017).sin())
            .collect();
        let weight: Vec<f32> = (0..params.out_channels * params.in_channels * params.kernel_size)
            .map(|i| ((i as f32) * 0.0013).cos())
            .collect();
        let bias: Vec<f32> = (0..params.out_channels).map(|i| i as f32 * 0.02).collect();

        let out_len = params.output_length(in_length);
        let mut out_naive = vec![0.0; params.out_channels * out_len];
        let mut out_simd = vec![0.0; params.out_channels * out_len];

        conv1d_naive(&input, &weight, Some(&bias), &params, in_length, &mut out_naive);
        conv1d_simd(&input, &weight, Some(&bias), &params, in_length, &mut out_simd);

        for (i, (n, s)) in out_naive.iter().zip(out_simd.iter()).enumerate() {
            let diff = (n - s).abs();
            assert!(diff < 1e-2, "idx={}: naive={}, simd={}, diff={}", i, n, s, diff);
        }
    }

    #[test]
    fn conv1d_simd_matches_naive_k3_dilation() {
        // Res2 block conv: k=3 dilation=2/3/4 padding=dilation
        for dilation in [2, 3, 4] {
            let params = Conv1dParams {
                in_channels: 64,
                out_channels: 64,
                kernel_size: 3,
                stride: 1,
                padding: dilation,
                dilation,
            };
            let in_length = 60;
            let input: Vec<f32> = (0..params.in_channels * in_length)
                .map(|i| ((i as f32 + dilation as f32) * 0.011).sin())
                .collect();
            let weight: Vec<f32> = (0..params.out_channels * params.in_channels * params.kernel_size)
                .map(|i| ((i as f32 + dilation as f32) * 0.002).cos())
                .collect();
            let bias: Vec<f32> = (0..params.out_channels).map(|i| i as f32 * 0.01).collect();

            let out_len = params.output_length(in_length);
            let mut out_naive = vec![0.0; params.out_channels * out_len];
            let mut out_simd = vec![0.0; params.out_channels * out_len];

            conv1d_naive(&input, &weight, Some(&bias), &params, in_length, &mut out_naive);
            conv1d_simd(&input, &weight, Some(&bias), &params, in_length, &mut out_simd);

            for (i, (n, s)) in out_naive.iter().zip(out_simd.iter()).enumerate() {
                let diff = (n - s).abs();
                assert!(
                    diff < 1e-2,
                    "dilation={} idx={}: naive={}, simd={}, diff={}",
                    dilation, i, n, s, diff
                );
            }
        }
    }

    #[test]
    fn conv1d_stride2() {
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
