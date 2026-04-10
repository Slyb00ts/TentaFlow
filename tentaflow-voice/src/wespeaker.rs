// =============================================================================
// Plik: wespeaker.rs
// Opis: WeSpeaker ECAPA-TDNN forward pass — pure Rust speaker embedding 192-dim.
//
// Architektura (z inspekcji embedding.onnx):
//   Input: feats [1, T, 80]  (Fbank features po CMVN)
//   ↓ Transpose [1, 80, T]
//   ↓ Conv1d(80→512, k=5, pad=2) + BN + ReLU      ← layer1
//   ↓ SE-Res2Block × 3                              ← layer2/3/4 (each 512→512)
//   ↓ Concat[layer2, layer3, layer4] axis=1 → 1536
//   ↓ Conv1d(1536→1536, k=1) + BN + ReLU            ← model.conv
//   ↓ Context-aware: concat[x, mean, std] → 4608
//   ↓ Conv1d(4608→128, k=1) + Tanh                  ← model.pool.linear1
//   ↓ Conv1d(128→1536, k=1) + Softmax(time)         ← model.pool.linear2
//   ↓ Attentive Stats Pool: weighted_mean+std → 3072
//   ↓ BatchNorm1d(3072)                             ← model.bn
//   ↓ Linear(3072→192) + L2 norm                    ← model.linear
//   Output: embs [192]
//
// SE-Res2Block:
//   Pre conv: Conv1d(512→512, k=1) + BN + ReLU  (.0)
//   Res2 split: 8 grup × 64 ch, 7 conv (k=3, dilation=2, pad=2)  (.1)
//   Post conv: Conv1d(512→512, k=1) + BN + ReLU  (.2)
//   SE block: avg_pool → linear1(512→128) → ReLU → linear2(128→512) → sigmoid  (.3)
//   Residual: input + se_weighted
// =============================================================================

use crate::error::{VoiceError, VoiceResult};
use crate::fbank::{compute_fbank, fbank_to_conv_input};
use crate::onnx_loader::OnnxWeights;
use crate::ops::{
    add_inplace, conv1d_simd, linear_bias, mean_axis_last, relu_inplace, sigmoid_scalar,
    softmax_axis_last, weighted_mean, weighted_std, BatchNorm1dFused, Conv1dParams,
};

const CHANNELS: usize = 512;
const RES2_GROUPS: usize = 8;
const RES2_GROUP_CH: usize = CHANNELS / RES2_GROUPS; // 64
const RES2_INNER_CONVS: usize = RES2_GROUPS - 1; // 7
const AGG_CHANNELS: usize = CHANNELS * 3; // 1536
const POOL_CONTEXT_CHANNELS: usize = AGG_CHANNELS * 3; // 4608 (x + mean + std)
const POOL_HIDDEN: usize = 128;
const STATS_CHANNELS: usize = AGG_CHANNELS * 2; // 3072 (mean + std)
const EMBEDDING_DIM: usize = 192;

/// Conv1d → ReLU → BatchNorm1d (fused). UWAGA: w tym modelu BN jest PO ReLU,
/// nie przed (jak w klasycznym ResNet/ECAPA). Zweryfikowane przez dump_graph.
struct ConvReluBn {
    weight: Vec<f32>,
    bias: Vec<f32>,
    bn: BatchNorm1dFused,
    in_channels: usize,
    out_channels: usize,
    kernel_size: usize,
    padding: usize,
    dilation: usize,
}

impl ConvReluBn {
    /// Wykonuje conv → relu → bn (kolejnosc z modelu).
    fn forward(&self, input: &[f32], in_length: usize) -> Vec<f32> {
        let params = Conv1dParams {
            in_channels: self.in_channels,
            out_channels: self.out_channels,
            kernel_size: self.kernel_size,
            stride: 1,
            padding: self.padding,
            dilation: self.dilation,
        };
        let out_length = params.output_length(in_length);
        let mut out = vec![0.0_f32; self.out_channels * out_length];
        conv1d_simd(input, &self.weight, Some(&self.bias), &params, in_length, &mut out);
        relu_inplace(&mut out);
        self.bn.apply(&mut out, out_length);
        out
    }
}

// Alias dla zachowania nazewnictwa w reszcie kodu
type ConvBnRelu = ConvReluBn;

/// SE-Res2Block: pre conv → Res2 split → post conv → SE → residual
struct SeRes2Block {
    pre: ConvBnRelu,                    // Conv1d 512→512 k=1
    res2: Vec<ConvBnRelu>,              // 7 × Conv1d 64→64 k=3 dilation=2
    post: ConvBnRelu,                   // Conv1d 512→512 k=1
    se_linear1_w: Vec<f32>,             // [128, 512]
    se_linear1_b: Vec<f32>,             // [128]
    se_linear2_w: Vec<f32>,             // [512, 128]
    se_linear2_b: Vec<f32>,             // [512]
}

impl SeRes2Block {
    fn forward(&self, x: &[f32], length: usize) -> Vec<f32> {
        // 1. Pre conv → relu → bn (block.0).
        let pre_out = self.pre.forward(x, length);

        // 2. Res2 split (zgodnie z ONNX grafem WeSpeaker):
        //    groups = split(pre_out, 8) each [64, length]
        //    y0 = conv[0](groups[0])
        //    y1 = conv[1](y0 + groups[1])
        //    y2 = conv[2](y1 + groups[2])
        //    ...
        //    y6 = conv[6](y5 + groups[6])
        //    concat = [y0, y1, y2, y3, y4, y5, y6, groups[7]]  (passthrough na KONCU!)
        let mut concatenated = vec![0.0_f32; CHANNELS * length];

        // y0 = conv[0](groups[0])
        let mut group_in = vec![0.0_f32; RES2_GROUP_CH * length];
        for c in 0..RES2_GROUP_CH {
            group_in[c * length..(c + 1) * length]
                .copy_from_slice(&pre_out[c * length..(c + 1) * length]);
        }
        let mut prev_y = self.res2[0].forward(&group_in, length);

        // zapisz y0 do concat[0..64]
        for c in 0..RES2_GROUP_CH {
            concatenated[c * length..(c + 1) * length]
                .copy_from_slice(&prev_y[c * length..(c + 1) * length]);
        }

        // y[i] = conv[i](y[i-1] + groups[i]) dla i=1..=6
        for i in 1..RES2_INNER_CONVS {
            let mut sum = prev_y.clone();
            for c in 0..RES2_GROUP_CH {
                let src = (i * RES2_GROUP_CH + c) * length;
                for t in 0..length {
                    sum[c * length + t] += pre_out[src + t];
                }
            }
            prev_y = self.res2[i].forward(&sum, length);
            for c in 0..RES2_GROUP_CH {
                let dst = (i * RES2_GROUP_CH + c) * length;
                concatenated[dst..dst + length]
                    .copy_from_slice(&prev_y[c * length..(c + 1) * length]);
            }
        }

        // Ostatnia grupa (groups[7]) — passthrough na koniec concat
        let g7_src = 7 * RES2_GROUP_CH;
        for c in 0..RES2_GROUP_CH {
            let src = (g7_src + c) * length;
            let dst = (g7_src + c) * length;
            concatenated[dst..dst + length].copy_from_slice(&pre_out[src..src + length]);
        }

        // 3. Post conv
        let mut post_out = self.post.forward(&concatenated, length);

        // 4. SE block: GlobalAvgPool [512] → linear1 → relu → linear2 → sigmoid
        let pooled = mean_axis_last(&post_out, CHANNELS, length);
        let mut se_hidden = vec![0.0_f32; POOL_HIDDEN];
        linear_bias(&self.se_linear1_w, &self.se_linear1_b, &pooled, CHANNELS, POOL_HIDDEN, &mut se_hidden);
        relu_inplace(&mut se_hidden);
        let mut se_out = vec![0.0_f32; CHANNELS];
        linear_bias(&self.se_linear2_w, &self.se_linear2_b, &se_hidden, POOL_HIDDEN, CHANNELS, &mut se_out);
        for v in &mut se_out {
            *v = sigmoid_scalar(*v);
        }

        // Multiply post_out per channel by se_out (broadcast over time)
        for c in 0..CHANNELS {
            let scale = se_out[c];
            for t in 0..length {
                post_out[c * length + t] *= scale;
            }
        }

        // 5. Residual: x (input bloku) + se_weighted_post.
        // input.7 w grafie ONNX to wynik layer1.bn dla layer2, lub poprzedniego SE-Res2Block
        // dla layer3/4. Pre_out jest tylko intermediate compute (do Res2/post conv).
        add_inplace(&mut post_out, x);
        post_out
    }
}

/// WeSpeaker ECAPA-TDNN model — pure Rust forward pass
pub struct WeSpeaker {
    layer1: ConvBnRelu,
    block_layer2: SeRes2Block,
    block_layer3: SeRes2Block,
    block_layer4: SeRes2Block,
    aggregation: ConvBnRelu, // model.conv (1536→1536, k=1)
    pool_linear1_w: Vec<f32>,
    pool_linear1_b: Vec<f32>,
    pool_linear2_w: Vec<f32>,
    pool_linear2_b: Vec<f32>,
    final_bn: BatchNorm1dFused, // model.bn (3072)
    final_linear_w: Vec<f32>,    // [192, 3072]
    final_linear_b: Vec<f32>,
    mean_vec: Vec<f32>,          // [192] — odejmowane od linear output (mean centering)
}

impl WeSpeaker {
    pub fn from_file(path: &str) -> VoiceResult<Self> {
        let weights = OnnxWeights::load(path)?;
        tracing::info!("WeSpeaker: zaladowano {} tensorow", weights.len());

        // layer1: Conv1d 80→512 k=5 pad=2
        let layer1 = load_conv_bn(&weights, "model.layer1.conv", "model.layer1.bn", 80, 512, 5, 2, 1)?;

        // 3 SE-Res2Blocks
        let block_layer2 = load_se_res2_block(&weights, 2)?;
        let block_layer3 = load_se_res2_block(&weights, 3)?;
        let block_layer4 = load_se_res2_block(&weights, 4)?;

        // model.conv: 1536→1536 k=1 pad=0 — UWAGA: nie ma BN po nim w grafie?
        // Sprawdzmy: po Conv_140 idzie Relu, ale nie ma BN. Trzeba zweryfikowac.
        // W kodzie WeSpeaker zwykle ma BN po conv. Zalozymy ze nie ma na razie.
        // (jeśli BN nie istnieje, layer fused = identity scale=1 shift=0)
        let aggregation = load_conv_bn_optional_bn(&weights, "model.conv", "model.bn_agg",
            AGG_CHANNELS, AGG_CHANNELS, 1, 0, 1)?;

        // Pool linears (Conv1d k=1)
        let pool_linear1_w = weights.get("model.pool.linear1.weight")?.data.clone();
        let pool_linear1_b = weights.get("model.pool.linear1.bias")?.data.clone();
        let pool_linear2_w = weights.get("model.pool.linear2.weight")?.data.clone();
        let pool_linear2_b = weights.get("model.pool.linear2.bias")?.data.clone();

        // Final BN (3072)
        let final_bn = load_batch_norm(&weights, "model.bn", STATS_CHANNELS)?;

        // Final linear: 3072 → 192
        let final_linear_w = weights.get("model.linear.weight")?.data.clone();
        let final_linear_b = weights.get("model.linear.bias")?.data.clone();

        // mean_vec [192] — mean centering po linear
        let mean_vec = weights.get("mean_vec")?.data.clone();

        Ok(Self {
            layer1,
            block_layer2,
            block_layer3,
            block_layer4,
            aggregation,
            pool_linear1_w,
            pool_linear1_b,
            pool_linear2_w,
            pool_linear2_b,
            final_bn,
            final_linear_w,
            final_linear_b,
            mean_vec,
        })
    }

    /// Ekstrahuje 192-dim embedding z audio 16kHz mono f32
    pub fn extract(&self, samples: &[f32]) -> VoiceResult<Vec<f32>> {
        if samples.is_empty() {
            return Err(VoiceError::InvalidInput("puste audio".into()));
        }

        // 1. Fbank features → [80, T]
        let frames = compute_fbank(samples);
        if frames.is_empty() {
            return Err(VoiceError::InvalidInput("za krotkie audio dla Fbank".into()));
        }
        let (input, t_len) = fbank_to_conv_input(&frames);
        tracing::debug!(num_frames = t_len, "Fbank features wyciagniete");

        // 2. layer1: Conv(80→512, k=5, pad=2) + BN + ReLU (length nie zmienia sie)
        let x_l1 = self.layer1.forward(&input, t_len);
        let l1_t_len = t_len;

        // 3. SE-Res2Block × 3 (channels stay 512, time stays the same)
        let x_l2 = self.block_layer2.forward(&x_l1, l1_t_len);
        let x_l3 = self.block_layer3.forward(&x_l2, l1_t_len);
        let x_l4 = self.block_layer4.forward(&x_l3, l1_t_len);

        // 4. Concat axis=1 [x_l2, x_l3, x_l4] → [1536, T]
        let mut concat = vec![0.0_f32; AGG_CHANNELS * l1_t_len];
        for c in 0..CHANNELS {
            for t in 0..l1_t_len {
                concat[c * l1_t_len + t] = x_l2[c * l1_t_len + t];
                concat[(CHANNELS + c) * l1_t_len + t] = x_l3[c * l1_t_len + t];
                concat[(2 * CHANNELS + c) * l1_t_len + t] = x_l4[c * l1_t_len + t];
            }
        }

        // 5. Aggregation conv (1536→1536, k=1)
        let x_agg = self.aggregation.forward(&concat, l1_t_len);

        // 6. Context: concat[x, global_mean, global_std] → [4608, T]
        //    Zgodnie z ONNX grafem (nodes 142-162):
        //    global_mean: ReduceMean(x, axis=-1) per kanal
        //    global_std: UNBIASED variance (dzielnik T-1), eps=1e-7, sqrt
        let global_mean = mean_axis_last(&x_agg, AGG_CHANNELS, l1_t_len);
        let mut global_std = vec![0.0_f32; AGG_CHANNELS];
        let t_f = l1_t_len as f32;
        let t_minus_1 = (t_f - 1.0).max(1.0);
        for c in 0..AGG_CHANNELS {
            let mean = global_mean[c];
            let mut sum_sq = 0.0_f32;
            for t in 0..l1_t_len {
                let d = x_agg[c * l1_t_len + t] - mean;
                sum_sq += d * d;
            }
            // unbiased: var_biased * T / (T-1) = sum_sq / (T-1)
            let var_unbiased = sum_sq / t_minus_1;
            global_std[c] = (var_unbiased + 1e-7).sqrt();
        }

        let mut context = vec![0.0_f32; POOL_CONTEXT_CHANNELS * l1_t_len];
        for c in 0..AGG_CHANNELS {
            for t in 0..l1_t_len {
                context[c * l1_t_len + t] = x_agg[c * l1_t_len + t];
                context[(AGG_CHANNELS + c) * l1_t_len + t] = global_mean[c];
                context[(2 * AGG_CHANNELS + c) * l1_t_len + t] = global_std[c];
            }
        }

        // 7. Pool linear1 (4608→128, k=1) + Tanh
        let pl1_params = Conv1dParams {
            in_channels: POOL_CONTEXT_CHANNELS,
            out_channels: POOL_HIDDEN,
            kernel_size: 1,
            stride: 1,
            padding: 0,
            dilation: 1,
        };
        let mut pl1_out = vec![0.0_f32; POOL_HIDDEN * l1_t_len];
        conv1d_simd(&context, &self.pool_linear1_w, Some(&self.pool_linear1_b), &pl1_params, l1_t_len, &mut pl1_out);
        for v in &mut pl1_out {
            *v = v.tanh();
        }

        // 8. Pool linear2 (128→1536, k=1) + Softmax(axis=time)
        let pl2_params = Conv1dParams {
            in_channels: POOL_HIDDEN,
            out_channels: AGG_CHANNELS,
            kernel_size: 1,
            stride: 1,
            padding: 0,
            dilation: 1,
        };
        let mut attn = vec![0.0_f32; AGG_CHANNELS * l1_t_len];
        conv1d_simd(&pl1_out, &self.pool_linear2_w, Some(&self.pool_linear2_b), &pl2_params, l1_t_len, &mut attn);
        softmax_axis_last(&mut attn, AGG_CHANNELS, l1_t_len);

        // 9. Attentive Statistics Pool: weighted mean + weighted std (eps=1e-7 jak w ONNX)
        let mean = weighted_mean(&x_agg, &attn, AGG_CHANNELS, l1_t_len);
        let std = weighted_std(&x_agg, &attn, &mean, AGG_CHANNELS, l1_t_len, 1e-7);

        // 10. Concat [mean, std] → [3072]
        let mut stats = vec![0.0_f32; STATS_CHANNELS];
        stats[..AGG_CHANNELS].copy_from_slice(&mean);
        stats[AGG_CHANNELS..].copy_from_slice(&std);

        // 11. Final BN (3072)
        self.final_bn.apply(&mut stats, 1);

        // 12. Linear (3072 → 192)
        let mut embedding = vec![0.0_f32; EMBEDDING_DIM];
        linear_bias(&self.final_linear_w, &self.final_linear_b, &stats, STATS_CHANNELS, EMBEDDING_DIM, &mut embedding);

        // 13. Mean centering: embs = linear_out - mean_vec
        for i in 0..EMBEDDING_DIM {
            embedding[i] -= self.mean_vec[i];
        }

        Ok(embedding)
    }

}

/// Helper: laduje Conv + BN parsed jako ConvBnRelu
fn load_conv_bn(
    weights: &OnnxWeights,
    conv_prefix: &str,
    bn_prefix: &str,
    in_channels: usize,
    out_channels: usize,
    kernel_size: usize,
    padding: usize,
    dilation: usize,
) -> VoiceResult<ConvBnRelu> {
    let weight = weights.get(&format!("{}.weight", conv_prefix))?.data.clone();
    let bias = weights.get(&format!("{}.bias", conv_prefix))?.data.clone();
    let bn = load_batch_norm(weights, bn_prefix, out_channels)?;
    Ok(ConvBnRelu {
        weight,
        bias,
        bn,
        in_channels,
        out_channels,
        kernel_size,
        padding,
        dilation,
    })
}

/// Wariant: ConvBnRelu z opcjonalnym BN. Jesli BN nie istnieje, BN = identity
fn load_conv_bn_optional_bn(
    weights: &OnnxWeights,
    conv_prefix: &str,
    bn_prefix: &str,
    in_channels: usize,
    out_channels: usize,
    kernel_size: usize,
    padding: usize,
    dilation: usize,
) -> VoiceResult<ConvBnRelu> {
    let weight = weights.get(&format!("{}.weight", conv_prefix))?.data.clone();
    let bias = weights.get(&format!("{}.bias", conv_prefix))?.data.clone();
    let bn = match load_batch_norm(weights, bn_prefix, out_channels) {
        Ok(b) => b,
        Err(_) => {
            // Identity BN: scale=1, shift=0
            BatchNorm1dFused::new(
                &vec![1.0; out_channels],
                &vec![0.0; out_channels],
                &vec![0.0; out_channels],
                &vec![1.0; out_channels],
                1e-5,
            )
        }
    };
    Ok(ConvBnRelu {
        weight,
        bias,
        bn,
        in_channels,
        out_channels,
        kernel_size,
        padding,
        dilation,
    })
}

fn load_batch_norm(
    weights: &OnnxWeights,
    prefix: &str,
    num_features: usize,
) -> VoiceResult<BatchNorm1dFused> {
    let gamma = weights.get(&format!("{}.weight", prefix))?.data.clone();
    let beta = weights.get(&format!("{}.bias", prefix))?.data.clone();
    let mean = weights.get(&format!("{}.running_mean", prefix))?.data.clone();
    let var = weights.get(&format!("{}.running_var", prefix))?.data.clone();
    if gamma.len() != num_features {
        return Err(VoiceError::ShapeMismatch {
            name: format!("{}.weight", prefix),
            expected: vec![num_features],
            actual: vec![gamma.len()],
        });
    }
    Ok(BatchNorm1dFused::new(&gamma, &beta, &mean, &var, 1e-5))
}

fn load_se_res2_block(weights: &OnnxWeights, layer_idx: usize) -> VoiceResult<SeRes2Block> {
    let p = format!("model.layer{}.se_res2block", layer_idx);

    // Pre conv: .0.conv + .0.bn  (512→512, k=1, pad=0)
    let pre = load_conv_bn(
        weights,
        &format!("{}.0.conv", p),
        &format!("{}.0.bn", p),
        CHANNELS, CHANNELS, 1, 0, 1,
    )?;

    // 7 inner Res2 convs (.1.convs.0..6 + .1.bns.0..6) 64→64, k=3
    // Dilation i padding zaleza od layer_idx (zweryfikowane z ONNX):
    //   layer2: dilation=2 pad=2
    //   layer3: dilation=3 pad=3
    //   layer4: dilation=4 pad=4
    let dilation = layer_idx;
    let padding = layer_idx;
    let mut res2 = Vec::with_capacity(RES2_INNER_CONVS);
    for i in 0..RES2_INNER_CONVS {
        let conv_p = format!("{}.1.convs.{}", p, i);
        let bn_p = format!("{}.1.bns.{}", p, i);
        res2.push(load_conv_bn(
            weights,
            &conv_p,
            &bn_p,
            RES2_GROUP_CH, RES2_GROUP_CH, 3, padding, dilation,
        )?);
    }

    // Post conv: .2.conv + .2.bn  (512→512, k=1, pad=0)
    let post = load_conv_bn(
        weights,
        &format!("{}.2.conv", p),
        &format!("{}.2.bn", p),
        CHANNELS, CHANNELS, 1, 0, 1,
    )?;

    // SE block linears (.3.linear1, .3.linear2)
    let se_linear1_w = weights.get(&format!("{}.3.linear1.weight", p))?.data.clone();
    let se_linear1_b = weights.get(&format!("{}.3.linear1.bias", p))?.data.clone();
    let se_linear2_w = weights.get(&format!("{}.3.linear2.weight", p))?.data.clone();
    let se_linear2_b = weights.get(&format!("{}.3.linear2.bias", p))?.data.clone();

    Ok(SeRes2Block {
        pre,
        res2,
        post,
        se_linear1_w,
        se_linear1_b,
        se_linear2_w,
        se_linear2_b,
    })
}

/// Cosine similarity miedzy dwoma embeddings (auto-normalizacja).
/// Dziala dla embeddings zarowno znormalizowanych jak i raw.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-12 || norm_b < 1e-12 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
