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
// Wydajnosc:
//   - Wagi pre-permutowane do [K, OC, IC] przy load time (PackedConv1dWeight)
//   - Conv1D dispatch: k=1 → direct GEMM, k>1 → gemm_accumulate_strided
//     (zero alokacji i zero kopiowania w hot path)
//   - Per-thread Scratch buffer trzyma wszystkie tensory pomiedzy warstwami
//     bez alokacji (rosnie lazy gdy T sie zwieksza)
// =============================================================================

use std::cell::RefCell;

use crate::error::{VoiceError, VoiceResult};
use crate::fbank::compute_fbank_into;
use crate::onnx_loader::OnnxWeights;
use crate::ops::{
    conv1d_prepacked, linear_bias, relu_inplace, sigmoid_scalar, softmax_axis_last,
    weighted_mean_into, weighted_std_into, BatchNorm1dFused, Conv1dParams, PackedConv1dWeight,
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
/// Wagi przechowywane jako PackedConv1dWeight — pre-permutowane do [K, OC, IC].
struct ConvReluBn {
    packed: PackedConv1dWeight,
    bias: Vec<f32>,
    bn: BatchNorm1dFused,
    in_channels: usize,
    out_channels: usize,
    kernel_size: usize,
    padding: usize,
    dilation: usize,
}

impl ConvReluBn {
    /// Zero-alloc forward: pisze do `out` bez tworzenia nowego Vec.
    fn forward_into(&self, input: &[f32], out: &mut [f32], in_length: usize) {
        let params = Conv1dParams {
            in_channels: self.in_channels,
            out_channels: self.out_channels,
            kernel_size: self.kernel_size,
            stride: 1,
            padding: self.padding,
            dilation: self.dilation,
        };
        let out_length = params.output_length(in_length);
        conv1d_prepacked(&self.packed, input, Some(&self.bias), &params, in_length, out);
        relu_inplace(out);
        self.bn.apply(out, out_length);
    }
}

// Alias dla zachowania nazewnictwa w reszcie kodu
type ConvBnRelu = ConvReluBn;

/// SE-Res2Block: pre conv → Res2 split → post conv → SE → residual
struct SeRes2Block {
    pre: ConvBnRelu,
    res2: Vec<ConvBnRelu>,
    post: ConvBnRelu,
    se_linear1_w: Vec<f32>,
    se_linear1_b: Vec<f32>,
    se_linear2_w: Vec<f32>,
    se_linear2_b: Vec<f32>,
}

impl SeRes2Block {
    /// Zero-alloc forward: `out` dostaje rezultat bloku (residual + post*se).
    fn forward_into(
        &self,
        x: &[f32],
        out: &mut [f32],
        length: usize,
        scratch: &mut Scratch,
    ) {
        // Destructure scratch raz — osobne &mut refs do kazdego pola,
        // brak kolizji borrow w dalszej czesci funkcji.
        let Scratch {
            se_pre_out,
            se_sum,
            se_res2_out,
            se_post_out,
            se_pooled,
            se_hidden,
            se_out: se_out_buf,
            ..
        } = scratch;

        let pre_out_slice = &mut se_pre_out[..CHANNELS * length];
        self.pre.forward_into(x, pre_out_slice, length);

        // Potrzebujemy czytac z pre_out i pisac do concatenated (= se_post_out).
        // Zrobmy separate immutable borrow przez reborrow.
        let pre_out: &[f32] = pre_out_slice;
        let concatenated = &mut se_post_out[..CHANNELS * length];

        // Passthrough groups[7] do tail concatenated
        let g7_start = 7 * RES2_GROUP_CH * length;
        let g7_len = RES2_GROUP_CH * length;
        concatenated[g7_start..g7_start + g7_len]
            .copy_from_slice(&pre_out[g7_start..g7_start + g7_len]);

        // y0 = conv[0](pre_out[0..64])
        let group_in = &mut se_sum[..RES2_GROUP_CH * length];
        group_in.copy_from_slice(&pre_out[..RES2_GROUP_CH * length]);

        let res2_out = &mut se_res2_out[..RES2_GROUP_CH * length];
        self.res2[0].forward_into(group_in, res2_out, length);
        concatenated[..RES2_GROUP_CH * length].copy_from_slice(res2_out);

        // y[i] = conv[i](y[i-1] + groups[i]) dla i=1..6
        for i in 1..RES2_INNER_CONVS {
            let group_src_start = i * RES2_GROUP_CH * length;
            // sum = res2_out + pre_out[group_src_start..]
            for idx in 0..(RES2_GROUP_CH * length) {
                group_in[idx] = res2_out[idx] + pre_out[group_src_start + idx];
            }
            self.res2[i].forward_into(group_in, res2_out, length);
            let dst_start = i * RES2_GROUP_CH * length;
            concatenated[dst_start..dst_start + RES2_GROUP_CH * length].copy_from_slice(res2_out);
        }

        // 3. Post conv: concatenated → out (bez intermediate copy)
        self.post.forward_into(concatenated, out, length);

        // 4. SE block: GlobalAvgPool [512] → linear1 → relu → linear2 → sigmoid
        let pooled = &mut se_pooled[..CHANNELS];
        let inv_len = 1.0 / length as f32;
        for c in 0..CHANNELS {
            let mut sum = 0.0_f32;
            let row = &out[c * length..(c + 1) * length];
            for &v in row {
                sum += v;
            }
            pooled[c] = sum * inv_len;
        }

        let hidden = &mut se_hidden[..POOL_HIDDEN];
        linear_bias(
            &self.se_linear1_w, &self.se_linear1_b,
            pooled, CHANNELS, POOL_HIDDEN, hidden,
        );
        relu_inplace(hidden);

        let se_weights = &mut se_out_buf[..CHANNELS];
        linear_bias(
            &self.se_linear2_w, &self.se_linear2_b,
            hidden, POOL_HIDDEN, CHANNELS, se_weights,
        );
        for v in se_weights.iter_mut() {
            *v = sigmoid_scalar(*v);
        }

        // 5. Fused: out = out * se_weights[c] + x (residual), SIMD single-threaded.
        //    Rayon po kanalach dalby 512 micro-taskow po 141 opsow — spawn cost
        //    dominuje. Single thread + f32x8 jest duzo szybsze dla takich
        //    rozmiarow (L1 cache hot, branch predictor happy).
        use wide::f32x8;
        let n32 = length - (length % 32);
        let n8 = length - (length % 8);
        for c in 0..CHANNELS {
            let scale = se_weights[c];
            let scale_v = f32x8::splat(scale);
            let row = &mut out[c * length..(c + 1) * length];
            let x_row = &x[c * length..(c + 1) * length];
            let mut i = 0;
            while i < n32 {
                let o0: [f32; 8] = row[i..i + 8].try_into().unwrap();
                let o1: [f32; 8] = row[i + 8..i + 16].try_into().unwrap();
                let o2: [f32; 8] = row[i + 16..i + 24].try_into().unwrap();
                let o3: [f32; 8] = row[i + 24..i + 32].try_into().unwrap();
                let x0: [f32; 8] = x_row[i..i + 8].try_into().unwrap();
                let x1: [f32; 8] = x_row[i + 8..i + 16].try_into().unwrap();
                let x2: [f32; 8] = x_row[i + 16..i + 24].try_into().unwrap();
                let x3: [f32; 8] = x_row[i + 24..i + 32].try_into().unwrap();
                let r0 = f32x8::from(o0).mul_add(scale_v, f32x8::from(x0));
                let r1 = f32x8::from(o1).mul_add(scale_v, f32x8::from(x1));
                let r2 = f32x8::from(o2).mul_add(scale_v, f32x8::from(x2));
                let r3 = f32x8::from(o3).mul_add(scale_v, f32x8::from(x3));
                row[i..i + 8].copy_from_slice(&r0.to_array());
                row[i + 8..i + 16].copy_from_slice(&r1.to_array());
                row[i + 16..i + 24].copy_from_slice(&r2.to_array());
                row[i + 24..i + 32].copy_from_slice(&r3.to_array());
                i += 32;
            }
            while i < n8 {
                let o: [f32; 8] = row[i..i + 8].try_into().unwrap();
                let x_v: [f32; 8] = x_row[i..i + 8].try_into().unwrap();
                let r = f32x8::from(o).mul_add(scale_v, f32x8::from(x_v));
                row[i..i + 8].copy_from_slice(&r.to_array());
                i += 8;
            }
            while i < length {
                row[i] = row[i] * scale + x_row[i];
                i += 1;
            }
        }
    }
}

#[derive(Default, Debug, Clone, Copy)]
pub struct LayerTimings {
    pub fbank: std::time::Duration,
    pub layer1: std::time::Duration,
    pub block2: std::time::Duration,
    pub block3: std::time::Duration,
    pub block4: std::time::Duration,
    pub concat_layers: std::time::Duration,
    pub aggregation: std::time::Duration,
    pub global_stats: std::time::Duration,
    pub pool_linear1: std::time::Duration,
    pub attention_pool: std::time::Duration,
    pub final_layers: std::time::Duration,
    pub total: std::time::Duration,
}

/// Scratch buffers dla WeSpeaker.extract() — zero-alloc hot path.
/// Per-thread zeby nie kolidowac z concurrent extract() calls na tym samym WeSpeaker.
struct Scratch {
    // Fbank flat [80 * T] — transposed input
    fbank: Vec<f32>,
    // Layer outputs — potrzebujemy 4 bufory do concatu [l2, l3, l4]
    l1: Vec<f32>,
    l2: Vec<f32>,
    l3: Vec<f32>,
    l4: Vec<f32>,
    // Concat i aggregation
    concat: Vec<f32>,   // [1536 * T]
    x_agg: Vec<f32>,    // [1536 * T]
    // Global stats + context
    global_mean: Vec<f32>, // [1536]
    global_std: Vec<f32>,  // [1536]
    context: Vec<f32>,     // [4608 * T]
    // Pool linears
    pl1_out: Vec<f32>, // [128 * T]
    attn: Vec<f32>,    // [1536 * T]
    // Attention pool
    att_mean: Vec<f32>, // [1536]
    att_std: Vec<f32>,  // [1536]
    stats: Vec<f32>,    // [3072]
    // SE-Res2Block internal (dzielone miedzy blokami)
    se_pre_out: Vec<f32>,      // [512 * T]
    se_sum: Vec<f32>,          // [64 * T]
    se_res2_out: Vec<f32>,     // [64 * T]
    se_post_out: Vec<f32>,     // [512 * T]
    se_pooled: Vec<f32>,       // [512]
    se_hidden: Vec<f32>,       // [128]
    se_out: Vec<f32>,          // [512]
}

impl Scratch {
    fn new() -> Self {
        Self {
            fbank: Vec::new(),
            l1: Vec::new(),
            l2: Vec::new(),
            l3: Vec::new(),
            l4: Vec::new(),
            concat: Vec::new(),
            x_agg: Vec::new(),
            global_mean: vec![0.0; AGG_CHANNELS],
            global_std: vec![0.0; AGG_CHANNELS],
            context: Vec::new(),
            pl1_out: Vec::new(),
            attn: Vec::new(),
            att_mean: vec![0.0; AGG_CHANNELS],
            att_std: vec![0.0; AGG_CHANNELS],
            stats: vec![0.0; STATS_CHANNELS],
            se_pre_out: Vec::new(),
            se_sum: Vec::new(),
            se_res2_out: Vec::new(),
            se_post_out: Vec::new(),
            se_pooled: vec![0.0; CHANNELS],
            se_hidden: vec![0.0; POOL_HIDDEN],
            se_out: vec![0.0; CHANNELS],
        }
    }

    /// Rosnie bufory do T timesteps jesli trzeba. Vec::resize trzyma capacity,
    /// wiec po pierwszym wywolaniu kolejne sa zero-alloc (chyba ze T rosnie).
    fn resize(&mut self, t: usize) {
        let ensure = |v: &mut Vec<f32>, needed: usize| {
            if v.len() < needed {
                v.resize(needed, 0.0);
            }
        };
        ensure(&mut self.fbank, 80 * t);
        ensure(&mut self.l1, CHANNELS * t);
        ensure(&mut self.l2, CHANNELS * t);
        ensure(&mut self.l3, CHANNELS * t);
        ensure(&mut self.l4, CHANNELS * t);
        ensure(&mut self.concat, AGG_CHANNELS * t);
        ensure(&mut self.x_agg, AGG_CHANNELS * t);
        ensure(&mut self.context, POOL_CONTEXT_CHANNELS * t);
        ensure(&mut self.pl1_out, POOL_HIDDEN * t);
        ensure(&mut self.attn, AGG_CHANNELS * t);
        ensure(&mut self.se_pre_out, CHANNELS * t);
        ensure(&mut self.se_sum, RES2_GROUP_CH * t);
        ensure(&mut self.se_res2_out, RES2_GROUP_CH * t);
        ensure(&mut self.se_post_out, CHANNELS * t);
    }
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::new());
}

/// WeSpeaker ECAPA-TDNN model — pure Rust forward pass
pub struct WeSpeaker {
    layer1: ConvBnRelu,
    block_layer2: SeRes2Block,
    block_layer3: SeRes2Block,
    block_layer4: SeRes2Block,
    aggregation: ConvBnRelu,
    pool_linear1_w: PackedConv1dWeight,
    pool_linear1_b: Vec<f32>,
    pool_linear2_w: PackedConv1dWeight,
    pool_linear2_b: Vec<f32>,
    final_bn: BatchNorm1dFused,
    final_linear_w: Vec<f32>,
    final_linear_b: Vec<f32>,
    mean_vec: Vec<f32>,
}

impl WeSpeaker {
    pub fn from_file(path: &str) -> VoiceResult<Self> {
        let weights = OnnxWeights::load(path)?;
        tracing::info!("WeSpeaker: zaladowano {} tensorow", weights.len());

        let layer1 = load_conv_bn(&weights, "model.layer1.conv", "model.layer1.bn", 80, 512, 5, 2, 1)?;

        let block_layer2 = load_se_res2_block(&weights, 2)?;
        let block_layer3 = load_se_res2_block(&weights, 3)?;
        let block_layer4 = load_se_res2_block(&weights, 4)?;

        let aggregation = load_conv_bn_optional_bn(&weights, "model.conv", "model.bn_agg",
            AGG_CHANNELS, AGG_CHANNELS, 1, 0, 1)?;

        // Pool linears (Conv1d k=1) — pre-packowane zeby korzystac z conv1d_prepacked
        let pl1_w_raw = weights.get("model.pool.linear1.weight")?.data.clone();
        let pool_linear1_w = PackedConv1dWeight::from_onnx(&pl1_w_raw, POOL_HIDDEN, POOL_CONTEXT_CHANNELS, 1);
        let pool_linear1_b = weights.get("model.pool.linear1.bias")?.data.clone();
        let pl2_w_raw = weights.get("model.pool.linear2.weight")?.data.clone();
        let pool_linear2_w = PackedConv1dWeight::from_onnx(&pl2_w_raw, AGG_CHANNELS, POOL_HIDDEN, 1);
        let pool_linear2_b = weights.get("model.pool.linear2.bias")?.data.clone();

        let final_bn = load_batch_norm(&weights, "model.bn", STATS_CHANNELS)?;
        let final_linear_w = weights.get("model.linear.weight")?.data.clone();
        let final_linear_b = weights.get("model.linear.bias")?.data.clone();
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

    /// Ekstrahuje embedding z timing'iem per-layer (benchmark).
    pub fn extract_with_timing(&self, samples: &[f32]) -> VoiceResult<(Vec<f32>, LayerTimings)> {
        let mut timings = LayerTimings::default();
        let total_start = std::time::Instant::now();

        let embedding = SCRATCH.with(|cell| -> VoiceResult<Vec<f32>> {
            let mut scratch = cell.borrow_mut();
            self.extract_impl(samples, &mut scratch, Some(&mut timings))
        })?;
        timings.total = total_start.elapsed();
        Ok((embedding, timings))
    }

    /// Ekstrahuje 192-dim embedding z audio 16kHz mono f32
    pub fn extract(&self, samples: &[f32]) -> VoiceResult<Vec<f32>> {
        SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            self.extract_impl(samples, &mut scratch, None)
        })
    }

    fn extract_impl(
        &self,
        samples: &[f32],
        scratch: &mut Scratch,
        mut timings: Option<&mut LayerTimings>,
    ) -> VoiceResult<Vec<f32>> {
        if samples.is_empty() {
            return Err(VoiceError::InvalidInput("puste audio".into()));
        }

        let t0 = std::time::Instant::now();
        // compute_fbank_into zapisuje bezposrednio do scratch.fbank w layout
        // [N_MELS=80, T] — ten sam layout ktorego oczekuje layer1 Conv1D input.
        let t_len = compute_fbank_into(samples, &mut scratch.fbank);
        if t_len == 0 {
            return Err(VoiceError::InvalidInput("za krotkie audio dla Fbank".into()));
        }
        scratch.resize(t_len);
        if let Some(ref mut t_) = timings { t_.fbank = t0.elapsed(); }

        let t0 = std::time::Instant::now();
        // Aby uniknac borrow conflict, uzywamy split_at_mut — self.layer1 czyta
        // z scratch.fbank i pisze do scratch.l1.
        {
            let (fbank_slice, rest) = scratch.fbank.split_at_mut(80 * t_len);
            let _ = rest; // nieuzywane
            // l1 jest osobnym vectorem wiec borrowing ok
            self.layer1.forward_into(fbank_slice, &mut scratch.l1[..CHANNELS * t_len], t_len);
        }
        if let Some(ref mut t_) = timings { t_.layer1 = t0.elapsed(); }

        // SE-Res2Block x 3. forward_into potrzebuje &mut Scratch + input slice —
        // zeby uniknac borrow conflict wyciagamy zarowno input jak i output buf
        // przez std::mem::take (Vec::default = pusty, zero-alloc swap).
        let t0 = std::time::Instant::now();
        {
            let l1_buf = std::mem::take(&mut scratch.l1);
            let mut l2_buf = std::mem::take(&mut scratch.l2);
            self.block_layer2.forward_into(
                &l1_buf[..CHANNELS * t_len],
                &mut l2_buf[..CHANNELS * t_len],
                t_len,
                scratch,
            );
            scratch.l1 = l1_buf;
            scratch.l2 = l2_buf;
        }
        if let Some(ref mut t_) = timings { t_.block2 = t0.elapsed(); }

        let t0 = std::time::Instant::now();
        {
            let l2_buf = std::mem::take(&mut scratch.l2);
            let mut l3_buf = std::mem::take(&mut scratch.l3);
            self.block_layer3.forward_into(
                &l2_buf[..CHANNELS * t_len],
                &mut l3_buf[..CHANNELS * t_len],
                t_len,
                scratch,
            );
            scratch.l2 = l2_buf;
            scratch.l3 = l3_buf;
        }
        if let Some(ref mut t_) = timings { t_.block3 = t0.elapsed(); }

        let t0 = std::time::Instant::now();
        {
            let l3_buf = std::mem::take(&mut scratch.l3);
            let mut l4_buf = std::mem::take(&mut scratch.l4);
            self.block_layer4.forward_into(
                &l3_buf[..CHANNELS * t_len],
                &mut l4_buf[..CHANNELS * t_len],
                t_len,
                scratch,
            );
            scratch.l3 = l3_buf;
            scratch.l4 = l4_buf;
        }
        if let Some(ref mut t_) = timings { t_.block4 = t0.elapsed(); }

        // Concat [l2, l3, l4] → concat [1536, T]
        let t0 = std::time::Instant::now();
        {
            let concat = &mut scratch.concat[..AGG_CHANNELS * t_len];
            let l2 = &scratch.l2[..CHANNELS * t_len];
            let l3 = &scratch.l3[..CHANNELS * t_len];
            let l4 = &scratch.l4[..CHANNELS * t_len];
            concat[..CHANNELS * t_len].copy_from_slice(l2);
            concat[CHANNELS * t_len..2 * CHANNELS * t_len].copy_from_slice(l3);
            concat[2 * CHANNELS * t_len..3 * CHANNELS * t_len].copy_from_slice(l4);
        }
        if let Some(ref mut t_) = timings { t_.concat_layers = t0.elapsed(); }

        // Aggregation conv (1536→1536, k=1) → x_agg
        let t0 = std::time::Instant::now();
        {
            let mut x_agg_buf = std::mem::take(&mut scratch.x_agg);
            self.aggregation.forward_into(
                &scratch.concat[..AGG_CHANNELS * t_len],
                &mut x_agg_buf[..AGG_CHANNELS * t_len],
                t_len,
            );
            scratch.x_agg = x_agg_buf;
        }
        if let Some(ref mut t_) = timings { t_.aggregation = t0.elapsed(); }

        // Global mean/std + context [x, mean, std] → [4608, T]
        // Single-threaded SIMD: 1536 × 141 × 2 passes = 433K ops. Rayon spawn
        // dla 1536 micro-taskow bylby dominujacy. Single thread f32x8 4-way
        // unrolled leci ~200-300us.
        let t0 = std::time::Instant::now();
        {
            use wide::f32x8;

            let x_agg = &scratch.x_agg[..AGG_CHANNELS * t_len];
            let t_f = t_len as f32;
            let t_minus_1 = (t_f - 1.0).max(1.0);
            let inv_t = 1.0 / t_f;
            let n32 = t_len - (t_len % 32);
            let n8 = t_len - (t_len % 8);

            for c in 0..AGG_CHANNELS {
                let row = &x_agg[c * t_len..(c + 1) * t_len];

                // SUM (mean) with 4-way unrolled f32x8
                let mut acc0 = f32x8::splat(0.0);
                let mut acc1 = f32x8::splat(0.0);
                let mut acc2 = f32x8::splat(0.0);
                let mut acc3 = f32x8::splat(0.0);
                let mut i = 0;
                while i < n32 {
                    let a0: [f32; 8] = row[i..i + 8].try_into().unwrap();
                    let a1: [f32; 8] = row[i + 8..i + 16].try_into().unwrap();
                    let a2: [f32; 8] = row[i + 16..i + 24].try_into().unwrap();
                    let a3: [f32; 8] = row[i + 24..i + 32].try_into().unwrap();
                    acc0 += f32x8::from(a0);
                    acc1 += f32x8::from(a1);
                    acc2 += f32x8::from(a2);
                    acc3 += f32x8::from(a3);
                    i += 32;
                }
                while i < n8 {
                    let a: [f32; 8] = row[i..i + 8].try_into().unwrap();
                    acc0 += f32x8::from(a);
                    i += 8;
                }
                let combined = (acc0 + acc1) + (acc2 + acc3);
                let lanes = combined.to_array();
                let mut sum = lanes[0] + lanes[1] + lanes[2] + lanes[3]
                            + lanes[4] + lanes[5] + lanes[6] + lanes[7];
                while i < t_len {
                    sum += row[i];
                    i += 1;
                }
                let mean = sum * inv_t;
                scratch.global_mean[c] = mean;

                // SUM_SQ with (v-mean)^2
                let m_v = f32x8::splat(mean);
                let mut sq0 = f32x8::splat(0.0);
                let mut sq1 = f32x8::splat(0.0);
                let mut sq2 = f32x8::splat(0.0);
                let mut sq3 = f32x8::splat(0.0);
                let mut i = 0;
                while i < n32 {
                    let a0: [f32; 8] = row[i..i + 8].try_into().unwrap();
                    let a1: [f32; 8] = row[i + 8..i + 16].try_into().unwrap();
                    let a2: [f32; 8] = row[i + 16..i + 24].try_into().unwrap();
                    let a3: [f32; 8] = row[i + 24..i + 32].try_into().unwrap();
                    let d0 = f32x8::from(a0) - m_v;
                    let d1 = f32x8::from(a1) - m_v;
                    let d2 = f32x8::from(a2) - m_v;
                    let d3 = f32x8::from(a3) - m_v;
                    sq0 = d0.mul_add(d0, sq0);
                    sq1 = d1.mul_add(d1, sq1);
                    sq2 = d2.mul_add(d2, sq2);
                    sq3 = d3.mul_add(d3, sq3);
                    i += 32;
                }
                while i < n8 {
                    let a: [f32; 8] = row[i..i + 8].try_into().unwrap();
                    let d = f32x8::from(a) - m_v;
                    sq0 = d.mul_add(d, sq0);
                    i += 8;
                }
                let sq_comb = (sq0 + sq1) + (sq2 + sq3);
                let sq_lanes = sq_comb.to_array();
                let mut sum_sq = sq_lanes[0] + sq_lanes[1] + sq_lanes[2] + sq_lanes[3]
                               + sq_lanes[4] + sq_lanes[5] + sq_lanes[6] + sq_lanes[7];
                while i < t_len {
                    let d = row[i] - mean;
                    sum_sq += d * d;
                    i += 1;
                }
                scratch.global_std[c] = ((sum_sq / t_minus_1) + 1e-7).sqrt();
            }

            // Build context [x, mean_broadcast, std_broadcast] → [4608, T]
            let context = &mut scratch.context[..POOL_CONTEXT_CHANNELS * t_len];
            context[..AGG_CHANNELS * t_len].copy_from_slice(x_agg);
            for c in 0..AGG_CHANNELS {
                let mean = scratch.global_mean[c];
                let std = scratch.global_std[c];
                let mean_row = &mut context[(AGG_CHANNELS + c) * t_len..(AGG_CHANNELS + c + 1) * t_len];
                for slot in mean_row.iter_mut() {
                    *slot = mean;
                }
                let std_row = &mut context[(2 * AGG_CHANNELS + c) * t_len..(2 * AGG_CHANNELS + c + 1) * t_len];
                for slot in std_row.iter_mut() {
                    *slot = std;
                }
            }
        }
        if let Some(ref mut t_) = timings { t_.global_stats = t0.elapsed(); }

        // Pool linear1 (4608→128, k=1) + tanh
        let t0 = std::time::Instant::now();
        {
            let pl1_params = Conv1dParams {
                in_channels: POOL_CONTEXT_CHANNELS,
                out_channels: POOL_HIDDEN,
                kernel_size: 1, stride: 1, padding: 0, dilation: 1,
            };
            let mut pl1_buf = std::mem::take(&mut scratch.pl1_out);
            conv1d_prepacked(
                &self.pool_linear1_w,
                &scratch.context[..POOL_CONTEXT_CHANNELS * t_len],
                Some(&self.pool_linear1_b),
                &pl1_params,
                t_len,
                &mut pl1_buf[..POOL_HIDDEN * t_len],
            );
            // Tanh in-place — SIMD-unfriendly (transcendental) ale kompilator
            // potrafi zwektoryzowac przez libmvec jesli target-feature pasuje.
            // Alternatywnie: tanh(x) ≈ x * (27 + x²) / (27 + 9 * x²) dla |x| < ~3
            // (polynomial approximation, ~1% error). W ECAPA-TDNN tanh jest w
            // ograniczonym zakresie bo poprzedni layer ma BN → szansa na approx.
            fast_tanh_inplace(&mut pl1_buf[..POOL_HIDDEN * t_len]);
            scratch.pl1_out = pl1_buf;
        }
        if let Some(ref mut t_) = timings { t_.pool_linear1 = t0.elapsed(); }

        // Pool linear2 (128→1536, k=1) + softmax(time) + attention pool
        let t0 = std::time::Instant::now();
        {
            let pl2_params = Conv1dParams {
                in_channels: POOL_HIDDEN,
                out_channels: AGG_CHANNELS,
                kernel_size: 1, stride: 1, padding: 0, dilation: 1,
            };
            let mut attn_buf = std::mem::take(&mut scratch.attn);
            conv1d_prepacked(
                &self.pool_linear2_w,
                &scratch.pl1_out[..POOL_HIDDEN * t_len],
                Some(&self.pool_linear2_b),
                &pl2_params,
                t_len,
                &mut attn_buf[..AGG_CHANNELS * t_len],
            );
            softmax_axis_last(&mut attn_buf[..AGG_CHANNELS * t_len], AGG_CHANNELS, t_len);
            scratch.attn = attn_buf;

            // Weighted mean + weighted std → stats [3072]  (zero-alloc, pisze do scratch)
            let x_agg = &scratch.x_agg[..AGG_CHANNELS * t_len];
            let attn = &scratch.attn[..AGG_CHANNELS * t_len];
            weighted_mean_into(x_agg, attn, AGG_CHANNELS, t_len, &mut scratch.att_mean);
            weighted_std_into(
                x_agg, attn, &scratch.att_mean, AGG_CHANNELS, t_len, 1e-7,
                &mut scratch.att_std,
            );
            scratch.stats[..AGG_CHANNELS].copy_from_slice(&scratch.att_mean);
            scratch.stats[AGG_CHANNELS..].copy_from_slice(&scratch.att_std);
        }
        if let Some(ref mut t_) = timings { t_.attention_pool = t0.elapsed(); }

        // Final BN + Linear + mean_vec subtract
        let t0 = std::time::Instant::now();
        self.final_bn.apply(&mut scratch.stats, 1);
        let mut embedding = vec![0.0_f32; EMBEDDING_DIM];
        linear_bias(
            &self.final_linear_w, &self.final_linear_b,
            &scratch.stats, STATS_CHANNELS, EMBEDDING_DIM, &mut embedding,
        );
        for i in 0..EMBEDDING_DIM {
            embedding[i] -= self.mean_vec[i];
        }
        if let Some(ref mut t_) = timings { t_.final_layers = t0.elapsed(); }

        Ok(embedding)
    }
}

/// Helper: laduje Conv + BN parsed jako ConvBnRelu z pre-packed weights
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
    let weight_raw = weights.get(&format!("{}.weight", conv_prefix))?.data.clone();
    let bias = weights.get(&format!("{}.bias", conv_prefix))?.data.clone();
    let bn = load_batch_norm(weights, bn_prefix, out_channels)?;
    let packed = PackedConv1dWeight::from_onnx(&weight_raw, out_channels, in_channels, kernel_size);
    Ok(ConvBnRelu {
        packed,
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
    let weight_raw = weights.get(&format!("{}.weight", conv_prefix))?.data.clone();
    let bias = weights.get(&format!("{}.bias", conv_prefix))?.data.clone();
    let bn = match load_batch_norm(weights, bn_prefix, out_channels) {
        Ok(b) => b,
        Err(_) => BatchNorm1dFused::new(
            &vec![1.0; out_channels],
            &vec![0.0; out_channels],
            &vec![0.0; out_channels],
            &vec![1.0; out_channels],
            1e-5,
        ),
    };
    let packed = PackedConv1dWeight::from_onnx(&weight_raw, out_channels, in_channels, kernel_size);
    Ok(ConvBnRelu {
        packed,
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

    let pre = load_conv_bn(
        weights,
        &format!("{}.0.conv", p),
        &format!("{}.0.bn", p),
        CHANNELS, CHANNELS, 1, 0, 1,
    )?;

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

    let post = load_conv_bn(
        weights,
        &format!("{}.2.conv", p),
        &format!("{}.2.bn", p),
        CHANNELS, CHANNELS, 1, 0, 1,
    )?;

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

/// SIMD tanh in-place — uzywa std f32::tanh() ale w petli ktora compiler
/// auto-wektoryzuje przez libmvec na Linux z glibc (wszystkie f32::tanh w
/// petli → vectorized __svml_tanhf8/__svml_tanhf16).
///
/// Dla wiekszosci CPU bez libmvec fallback to scalar ale nadal szybki bo
/// tanh jest tiny relatively.
#[inline]
fn fast_tanh_inplace(buf: &mut [f32]) {
    for v in buf.iter_mut() {
        *v = v.tanh();
    }
}

/// Cosine similarity miedzy dwoma embeddings (auto-normalizacja).
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
