// ===== File: model.rs — single-sequence forward pass (batched prefill + graphed decode) =====
// Decode runs one token per step through a captured CUDA graph; prefill runs
// whole prompt chunks through batched GEMM/attention kernels (same math, T
// tokens at once). The residual stream is carried through fused
// rmsnorm_residual chaining, so no standalone add kernel exists: each fusion
// adds the previous sublayer's output and produces the next sublayer's
// normed input.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use forge_formats::PoolingType;
use forge_hal::{DevBuffer, Device, Event, ExecGraph, Pool, Stream};
use forge_kernels::{Kernels, Nvfp4GgufQ8Projection};
use forge_types::{ForgeError, MemKind, Result, Vendor};
use half::f16;

use crate::kv::{KvCache, KvConfig, KvQuant, SeqKv};
use crate::mtp::{MtpDraftState, MtpEmbedding};
use crate::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use crate::tier::{KvTierConfig, TierManager, STAGE_SLOTS};
use crate::weights::{
    AttnWeights, CalibStats, DeltaNetWeights, DevWeight, Fp8Weight, GateUpWeights, LayerFfn,
    LayerMixer, ModelWeights, MoeFfn, QkvWeights, W4A8Weight,
};

/// Largest token count `prefill_chunk` accepts per call; callers split longer
/// prompts. Bounds the persistent prefill scratch allocation.
pub const MAX_PREFILL_CHUNK: usize = 1024;

/// Fixed built-in calibration passage for W4A8 SmoothQuant. A few hundred
/// tokens of representative English prose plus a code snippet — enough to
/// exercise every linear's input-channel dynamic range so the migration scales
/// reflect real activation outliers. Embedded in-tree so calibration needs no
/// download and no original fp16 weights (SmoothQuant needs statistics only).
pub const W4A8_CALIB_TEXT: &str = "\
The quick brown fox jumps over the lazy dog. In the beginning the universe was \
created; this has made a lot of people very angry and been widely regarded as a \
bad move. Language models predict the next token given the preceding context, \
learning statistical regularities from vast corpora of natural text. Attention \
mechanisms let each position attend to every other position, weighting their \
contributions by learned compatibility scores. Quantization reduces the \
precision of weights and activations to shrink memory bandwidth and accelerate \
matrix multiplication on tensor cores, trading a small amount of accuracy for a \
large gain in throughput. The Eiffel Tower is located in Paris, the capital of \
France, and was completed in 1889 for the World's Fair. Water boils at one \
hundred degrees Celsius at sea level, and freezes at zero. Photosynthesis \
converts carbon dioxide and water into glucose and oxygen using energy from \
sunlight captured by chlorophyll in the chloroplasts of plant cells.\n\n\
fn fibonacci(n: u64) -> u64 {\n    let (mut a, mut b) = (0u64, 1u64);\n    \
for _ in 0..n {\n        let next = a + b;\n        a = b;\n        b = next;\n    \
}\n    a\n}\n\n\
def quicksort(xs):\n    if len(xs) <= 1:\n        return xs\n    pivot = xs[len(xs) // 2]\n    \
left = [x for x in xs if x < pivot]\n    mid = [x for x in xs if x == pivot]\n    \
right = [x for x in xs if x > pivot]\n    return quicksort(left) + mid + quicksort(right)\n\n\
SELECT id, name, SUM(amount) AS total FROM orders WHERE status = 'paid' GROUP BY id, name \
HAVING total > 1000 ORDER BY total DESC LIMIT 20;\n\n\
The mitochondria is the powerhouse of the cell. Supply and demand determine \
prices in a competitive market: when demand rises and supply stays fixed, the \
equilibrium price increases. Newton's second law states that force equals mass \
times acceleration. The derivative of sine is cosine, and the integral of one \
over x is the natural logarithm of x. Machine translation, summarization, and \
question answering are classic natural-language processing tasks that modern \
transformer architectures address with a single unified sequence-to-sequence \
formulation trained end to end on large and diverse multilingual datasets.";

/// Largest speculative draft (tokens) a single verification forward accepts
/// (SPEC §6). One verify runs `fed + draft` = up to `MAX_SPEC_DRAFT + 1` query
/// positions, bounding the [T, vocab] verify-logit scratch.
pub const MAX_SPEC_DRAFT: usize = 16;

/// Context splits for decode attention. Splitting shortens each warp's
/// sequential online-softmax chain by this factor (decode runs one block per
/// head — heavily latency-bound; 8 splits cut the attention kernel from
/// ~24 us to ~7 us per layer on RTX 4090), at the cost of a regrouped
/// softmax whose rounding differs slightly from the single-block order.
/// Measured drift vs splits=1 over 16 greedy steps: logit max-abs-diff
/// 0.087 (Bielik NVFP4) / 0.042 (Qwen3 Q8_0) with the argmax identical at
/// every step. 1 reproduces the unsplit arithmetic bit-exactly.
const ATTN_DECODE_SPLITS: usize = 8;
const ATTN_DECODE_GQA_SPLITS: usize = 32;

/// Coarse per-phase wall-clock attribution for `prefill_chunk`, enabled by
/// FORGE_PREFILL_TRACE=1. Every probe synchronizes the device, so absolute
/// numbers are pessimistic (no inter-kernel overlap) — use the ratios.
struct PrefillTrace {
    enabled: bool,
    names: Vec<&'static str>,
    totals: Vec<std::time::Duration>,
    last: std::time::Instant,
}

impl PrefillTrace {
    fn new() -> Self {
        Self {
            enabled: std::env::var("FORGE_PREFILL_TRACE").is_ok_and(|v| v == "1"),
            names: Vec::new(),
            totals: Vec::new(),
            last: std::time::Instant::now(),
        }
    }

    fn start(&mut self, device: &dyn Device) {
        if self.enabled {
            let _ = device.synchronize();
            self.last = std::time::Instant::now();
        }
    }

    fn mark(&mut self, device: &dyn Device, name: &'static str) {
        if !self.enabled {
            return;
        }
        let _ = device.synchronize();
        let now = std::time::Instant::now();
        let dt = now - self.last;
        self.last = now;
        match self.names.iter().position(|n| *n == name) {
            Some(i) => self.totals[i] += dt,
            None => {
                self.names.push(name);
                self.totals.push(dt);
            }
        }
    }

    fn report(&self, n_tokens: usize) {
        if !self.enabled {
            return;
        }
        let total: std::time::Duration = self.totals.iter().sum();
        eprintln!("prefill_chunk trace (T={n_tokens}, total {total:?}):");
        let mut order: Vec<usize> = (0..self.names.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.totals[i]));
        for i in order {
            eprintln!(
                "  {:<16} {:>10.3?} ({:>5.1}%)",
                self.names[i],
                self.totals[i],
                self.totals[i].as_secs_f64() / total.as_secs_f64() * 100.0
            );
        }
    }
}

pub struct ModelConfig {
    pub kv_page_size: usize,
    pub kv_pages: usize,
    pub max_seq_len: usize,
    /// KV cache storage mode. F16 (default, bit-exact canonical path), Fp8
    /// (halves KV memory + bandwidth; per-value scale-free e4m3, fused decode
    /// only), or Rot{bits} (TurboQuant-class rotational 3/4-bit; single-stream
    /// decode path). Validated at load.
    pub kv_quant: KvQuant,
    /// KV tiering (SPEC §5.4B): spill cold pages to pinned RAM / NVMe and
    /// stream them back per layer, unlocking contexts beyond the VRAM pool.
    /// Off (default) = today's VRAM-only behavior; f16/fp8 caches only.
    pub kv_tier: KvTierConfig,
    /// Radix-tree prefix caching (SPEC §5.2): dedup shared KV prefixes across
    /// sequences so a request sharing a prefix skips re-prefilling it. `true`
    /// (default) engages only when it is a strict optimization — F16/Fp8 KV,
    /// no tiering, non-hybrid arch; otherwise silently inactive.
    pub prefix_cache: bool,
    /// Ładuje i uruchamia opcjonalną głowę MTP/NextN modelu.
    pub native_mtp: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            kv_page_size: 32,
            kv_pages: 512,
            max_seq_len: 8192,
            kv_quant: KvQuant::F16,
            kv_tier: KvTierConfig::default(),
            prefix_cache: true,
            native_mtp: false,
        }
    }
}

/// L2-normalize a vector in place. A zero vector is left unchanged (no NaNs
/// from dividing by a zero norm).
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        let inv = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

pub struct Model {
    pub device: Arc<dyn Device>,
    pub kernels: Kernels,
    pub weights: ModelWeights,
    pub kv: KvCache,
    stream: Stream,
    /// Device-side page table + seq len for the active sequence (v0: one).
    page_table_dev: DevBuffer,
    seq_len_dev: DevBuffer,
    max_pages_per_seq: usize,
    bufs: DecodeBufs,
    /// Batched-prefill scratch; allocated lazily on the first prefill_chunk.
    prefill_bufs: Option<PrefillBufs>,
    /// Speculative-verification logit scratch (SPEC §6): the [T, vocab] f32
    /// logits of one draft-verification forward. Allocated lazily on the first
    /// `verify_greedy_draft`; `None` until speculation runs.
    verify_bufs: Option<VerifyBufs>,
    /// Warstwowy verifier T=3/4 dla hybrydowego targetu MTP.
    hybrid_verify_bufs: Option<HybridVerifyBufs>,
    /// Captured decode step; replayed per token (inputs are device-resident).
    decode_graph: Option<ExecGraph>,
    /// Captured non-hybrid MoE decode step (fully device-side grouped expert
    /// dispatch — no host readback), replayed per token like `decode_graph`.
    decode_moe_graph: Option<ExecGraph>,
    /// Captured rotational (rot4/rot3) decode step; replayed per token. The
    /// pack kernel reads the token position from `bufs.pos`, so the whole
    /// dual-region chain is position-independent and graph-capturable.
    decode_rot_graph: Option<ExecGraph>,
    /// Continuous-batching decode scratch (sized for `batch_cap` sequences),
    /// allocated on the first `ensure_batch`.
    batch_bufs: Option<BatchBufs>,
    /// Per-bucket captured batched forward+logits graphs (bucket = padded
    /// batch size). Replayed for any live batch that rounds up to the bucket.
    batch_graphs: HashMap<usize, ExecGraph>,
    /// Largest batch the scratch is provisioned for (0 until `ensure_batch`).
    batch_cap: usize,
    /// KV tier manager (SPEC §5.4B); `None` = tiering off (VRAM-only paging,
    /// bit-for-bit today's behavior).
    tier: Option<TierManager>,
    /// Streamed-attention staging: full-context K/V slabs for ONE layer plus
    /// an identity page table (staging page index == logical page index).
    tier_bufs: Option<TierBufs>,
    /// Sequence whose page table currently occupies `page_table_dev`
    /// (0 = none/stale). Spills, restores and batched growth invalidate it,
    /// forcing a re-upload on the next single-stream step.
    pt_seq: u64,
    /// MoE scratch; `Some` only for Mixture-of-Experts models.
    moe_bufs: Option<MoeBufs>,
    /// Per-layer Gated-DeltaNet recurrent state (hybrid `qwen35moe` only):
    /// `Some` for DeltaNet layers, `None` for attention layers. Persistent for
    /// the model lifetime (single active sequence at a time on the hybrid
    /// path), zeroed at each sequence start (`pos == 0`).
    ssm: Vec<Option<SsmState>>,
    /// Gated-attention + DeltaNet single-token scratch; allocated lazily for a
    /// hybrid model on the first hybrid forward.
    hybrid_bufs: Option<HybridBufs>,
    /// FORGE_HYBRID_DEBUG=1: dump per-layer residual-stream norms.
    hybrid_debug: bool,
    /// Radix-tree prefix cache (SPEC §5.2); `None` = inactive (disabled by
    /// config, or ineligible: tiering / rot / hybrid arch). When active, admitted
    /// sequences borrow shared prefix pages before prefill and donate their own
    /// prefilled pages on completion.
    prefix_cache: Option<crate::prefix::PrefixCache>,
    /// Active W4A8 SmoothQuant calibration accumulator: when `Some`, the dense
    /// prefill path records per-input-channel activation abs-max at the four
    /// linear-input points. Set only during `calibrate_w4a8`, `None` otherwise.
    calib: Option<CalibAccum>,
    /// Izolowany stan jednowarstwowego draftu NextN; nie uczestniczy w server flow.
    mtp_state: Option<MtpDraftState>,
}

/// Running per-input-channel activation abs-max for the W4A8 SmoothQuant
/// calibration pass. One vector per layer per linear-input point; reduced from
/// the f16 activation buffers read back after each relevant kernel.
struct CalibAccum {
    attn_in: Vec<Vec<f32>>,
    attn_out: Vec<Vec<f32>>,
    ffn_in: Vec<Vec<f32>>,
    down_in: Vec<Vec<f32>>,
    /// Host staging for the largest captured buffer (grown as needed).
    scratch: Vec<u8>,
}

impl CalibAccum {
    fn new(n_layers: usize, hidden: usize, q_dim: usize, inter: usize) -> Self {
        Self {
            attn_in: vec![vec![0.0f32; hidden]; n_layers],
            attn_out: vec![vec![0.0f32; q_dim]; n_layers],
            ffn_in: vec![vec![0.0f32; hidden]; n_layers],
            down_in: vec![vec![0.0f32; inter]; n_layers],
            scratch: Vec::new(),
        }
    }

    /// Fold `t` rows of an f16 activation buffer into a per-channel abs-max acc.
    fn absorb(
        device: &dyn Device,
        buf: &DevBuffer,
        acc: &mut [f32],
        t: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<()> {
        let dim = acc.len();
        let bytes = t * dim * 2;
        if scratch.len() < bytes {
            scratch.resize(bytes, 0);
        }
        device.read(buf, 0, &mut scratch[..bytes])?;
        let rows: &[f16] = bytemuck::cast_slice(&scratch[..bytes]);
        for row in 0..t {
            let r = &rows[row * dim..row * dim + dim];
            for (a, h) in acc.iter_mut().zip(r) {
                *a = a.max(h.to_f32().abs());
            }
        }
        Ok(())
    }
}

/// One DeltaNet layer's resident recurrent state for the active sequence.
struct SsmState {
    /// Causal conv window `[conv_dim, d_conv-1]` f16 (oldest sample first).
    conv: DevBuffer,
    /// Recurrent state matrices `[n_v_heads, d_state, d_state]` f32.
    state: DevBuffer,
}

/// Single-token scratch for the hybrid (gated-attention + DeltaNet) forward.
/// Buffers that exceed the standard decode scratch widths (the gated Q
/// projection is `2*n_heads*head_dim`, the conv stream `conv_dim`) live here.
struct HybridBufs {
    /// Gated Q projection output `[2*n_heads*head_dim]` f16.
    q_full: DevBuffer,
    /// De-interleaved query `[n_heads*head_dim]` f16.
    qc: DevBuffer,
    /// De-interleaved output gate `[n_heads*head_dim]` f16.
    gatec: DevBuffer,
    /// Gated attention output `[n_heads*head_dim]` f16 (attn ⊙ sigmoid(gate)).
    gated: DevBuffer,
    /// DeltaNet in-projection conv stream `[conv_dim]` f16.
    qkv_mixed: DevBuffer,
    /// DeltaNet output gate `z` `[value_dim]` f16.
    z: DevBuffer,
    /// Conv + SiLU output `[conv_dim]` f16.
    conv_out: DevBuffer,
    /// Per-head-split conv q/k `[key_dim]` and their repeat to `[value_dim]`.
    q16: DevBuffer,
    k16: DevBuffer,
    q16src: DevBuffer,
    k16src: DevBuffer,
    q32: DevBuffer,
    k32: DevBuffer,
    /// Conv value heads `[value_dim]` f16.
    vtok: DevBuffer,
    /// Raw alpha / beta projections `[n_v_heads]` f16.
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    /// Per-head log-decay `g` and write-gate `beta` `[n_v_heads]` f32.
    g: DevBuffer,
    beta_f: DevBuffer,
    /// DeltaNet recurrence output + gated-RMSNorm output `[value_dim]` f16.
    o: DevBuffer,
    normed: DevBuffer,
    /// Pinned-host staging for the per-token embedding row `[hidden]` f16, so
    /// the host gather lands via an async H2D on the compute stream (no
    /// per-token blocking legacy-stream drain).
    pinned_embed: DevBuffer,
}

/// Attention source for the shared decode chains: the paged VRAM cache (fast
/// path, graph-capturable) or the tier staging slabs carrying the sequence's
/// full context per layer (streamed path, never captured).
enum AttnSrc<'a> {
    Paged,
    Staged(&'a SeqKv),
}

/// VRAM staging for the streamed tier path (allocated only with tiering on).
/// Two slots ping-pong so the fused decode chain restores layer l+1 on the
/// tier's transfer stream while layer l's attention runs on the compute
/// stream; the synchronous paths (separate chain, prefill, rot, batched
/// streamed lanes) use slot 0 only.
struct TierBufs {
    slots: Vec<StageSlot>,
    identity_pt: DevBuffer,
    /// Bytes of one page of each staged region (KvConfig::tier_region_bytes
    /// order: K/V for f16/fp8; packed K/V + K/V scales for rot).
    region_bytes: Vec<usize>,
}

/// One staging generation: full-context slabs for one layer (one slab per
/// spillable region) plus the cross-stream handshake events.
struct StageSlot {
    stage: Vec<DevBuffer>,
    /// Recorded on the transfer stream when the slot's staging copies are
    /// enqueued; the compute stream waits on it before the attention launch.
    ready: Event,
    /// Recorded on the compute stream when the slot's slabs are no longer
    /// read; the transfer stream waits on it before restaging the slot.
    free: Event,
}

/// Persistent per-step activation buffers. Fixed addresses are what makes the
/// decode step CUDA-graph-replayable: only their contents change per token.
struct DecodeBufs {
    h: DevBuffer,
    /// Unrounded f32 mirror of the residual stream, written by the fused
    /// gemv_residual kernels; the norm-recomputing kernels take their
    /// sum-of-squares from it (rmsnorm_residual_f16's exact dataflow).
    h32: DevBuffer,
    x: DevBuffer,
    /// Fused q|k|v output ([q_dim + 2*kv_dim]); q starts at offset 0, so the
    /// attention kernel reads it directly. Split-layer fallbacks use q/k/v.
    qkv: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_out: DevBuffer,
    /// Split-attention partials: [n_heads, ATTN_DECODE_SPLITS, head_dim + 2]
    /// f32 (unnormalized acc + running max + running sum per split).
    attn_parts: DevBuffer,
    o_out: DevBuffer,
    /// Fused gate|up output ([2*inter]); split-layer fallbacks use gate/up.
    gate_up: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    logits: DevBuffer,
    ids: DevBuffer,
    pos: DevBuffer,
    /// Pinned-host staging: [token_id, pos, seq_len] i32 triple.
    pinned_in: DevBuffer,
    /// Pinned-host mirror of the page table (async H2D on page boundary).
    pinned_pt: DevBuffer,
    /// Pinned-host landing buffer for logits (avoids pageable D2H).
    pinned_logits: DevBuffer,
    /// Per-block partials of the sampling kernels ((f32, i32) pair arrays).
    sample_vals: DevBuffer,
    sample_idx: DevBuffer,
    /// Sampling result: [token_id i32, logprob f32].
    sample_out: DevBuffer,
    /// Pinned-host landing buffer for the 8-byte sampling result.
    pinned_sample: DevBuffer,
    /// Device-resident distinct-token list for the repetition penalty.
    penalty_ids: DevBuffer,
    /// Pinned-host staging for `penalty_ids`.
    pinned_penalty: DevBuffer,
}

/// Persistent prefill scratch sized for MAX_PREFILL_CHUNK tokens. Activation
/// matrices are [T, cols] row-major; the batched GEMMs consume them directly
/// (token/column tails are clamped inside the kernels).
struct PrefillBufs {
    h: DevBuffer,
    x: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_out: DevBuffer,
    o_out: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
}

/// Speculative-verification scratch (SPEC §6): the [cap, vocab] f32 logits of
/// one draft-verification forward (one row per query position: the fed token
/// plus each draft token) plus the per-row greedy argmax, sized for `cap` =
/// MAX_SPEC_DRAFT + 1 positions. The argmax runs on the GPU so only `cap` token
/// ids cross PCIe, never the [cap, vocab] logits.
struct VerifyBufs {
    cap: usize,
    logits: DevBuffer,
    /// Per-row argmax token ids (i32, `cap` long), device-side.
    ids: DevBuffer,
    /// Pinned-host landing for `ids`.
    pinned_ids: DevBuffer,
}

/// Sposób przechowania danych potrzebnych do zatwierdzenia stanu DeltaNet.
enum DeltaVerifyCommit {
    Retained { checkpoint_byte_offset: usize },
    Recompute {
        q: DevBuffer,
        k: DevBuffer,
        v: DevBuffer,
        g: DevBuffer,
        beta: DevBuffer,
    },
}

/// Dane jednej warstwy DeltaNet potrzebne do zatwierdzenia prefiksu.
struct DeltaVerifyCache {
    commit: DeltaVerifyCommit,
    conv_initial: DevBuffer,
    conv_checkpoints: DevBuffer,
    state_initial: DevBuffer,
}

/// Bufory krótkiego, warstwowego przebiegu weryfikacyjnego modelu hybrydowego.
struct HybridVerifyBufs {
    cap: usize,
    q_full: DevBuffer,
    qc: DevBuffer,
    gatec: DevBuffer,
    gated: DevBuffer,
    qkv_mixed: DevBuffer,
    z: DevBuffer,
    conv_out: DevBuffer,
    q16: DevBuffer,
    k16: DevBuffer,
    q32: DevBuffer,
    k32: DevBuffer,
    vtok: DevBuffer,
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    g: DevBuffer,
    beta_f: DevBuffer,
    o: DevBuffer,
    normed: DevBuffer,
    state_checkpoints: DevBuffer,
    retained_state_checkpoints: Option<DevBuffer>,
    accepted: DevBuffer,
    pinned_embed: DevBuffer,
    delta: Vec<Option<DeltaVerifyCache>>,
}

fn alloc_checked(
    device: &dyn Device,
    name: &str,
    dimensions: &[usize],
    element_bytes: usize,
    kind: MemKind,
) -> Result<DevBuffer> {
    let bytes = dimensions
        .iter()
        .try_fold(element_bytes, |size, &dimension| {
            size.checked_mul(dimension).ok_or_else(|| {
                ForgeError::Scheduler(format!(
                    "przepełnienie rozmiaru bufora {name}: {dimensions:?}"
                ))
            })
        })?;
    device.alloc(bytes, kind, Pool::Activations)
}

fn native_mtp_greedy_decision(draft: &[u32], predictions: &[i32]) -> (usize, u32) {
    debug_assert_eq!(predictions.len(), draft.len() + 1);
    let mut accepted = 0usize;
    while accepted < draft.len() && predictions[accepted] as u32 == draft[accepted] {
        accepted += 1;
    }
    (accepted, predictions[accepted] as u32)
}

fn finish_greedy_verification(
    kv: &mut KvCache,
    page_table_seq: &mut u64,
    seq: &mut SeqKv,
    base: usize,
    result: Result<(usize, u32)>,
) -> Result<(usize, u32)> {
    let result = match result {
        Ok((accepted, correction)) => {
            let target_len = accepted
                .checked_add(1)
                .and_then(|retained| base.checked_add(retained));
            match target_len {
                Some(target_len) if target_len <= seq.len => {
                    kv.rollback(seq, target_len);
                    Ok((accepted, correction))
                }
                _ => {
                    if seq.len >= base {
                        kv.rollback(seq, base);
                    }
                    Err(ForgeError::Scheduler(
                        "invalid speculative verification rollback target".into(),
                    ))
                }
            }
        }
        Err(error) => {
            if seq.len >= base {
                kv.rollback(seq, base);
            }
            Err(error)
        }
    };
    *page_table_seq = 0;
    result
}

/// Persistent continuous-batching decode scratch sized for `cap` sequences.
/// Activation matrices are `[cap, cols]` row-major (the batched GEMM/attention
/// kernels consume them directly, one row per active sequence). Per-step inputs
/// (ids/positions/seq_lens/page_table) and per-seq sampling params live in
/// device buffers refreshed by one async H2D per replay, so the forward+logits
/// path is CUDA-graph-replayable per batch-size bucket.
struct BatchBufs {
    cap: usize,
    h: DevBuffer,
    x: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn_parts: DevBuffer,
    attn_out: DevBuffer,
    o_out: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    down: DevBuffer,
    logits: DevBuffer,
    ids: DevBuffer,
    positions: DevBuffer,
    seq_lens: DevBuffer,
    page_table: DevBuffer,
    /// Pinned staging: [ids | positions | seq_lens], i32, cap each.
    pinned_meta: DevBuffer,
    pinned_pt: DevBuffer,
    /// Per-seq sampling params (device + pinned staging).
    samp_k: DevBuffer,
    samp_inv_t: DevBuffer,
    samp_top_p: DevBuffer,
    samp_min_p: DevBuffer,
    samp_seed: DevBuffer,
    samp_step: DevBuffer,
    pinned_samp: DevBuffer,
    /// Repetition-penalty staging: flat distinct-id list, prefix offsets,
    /// per-seq penalty.
    pen_ids: DevBuffer,
    pen_offsets: DevBuffer,
    pen_vals: DevBuffer,
    pinned_pen_ids: DevBuffer,
    pinned_pen_offsets: DevBuffer,
    pinned_pen_vals: DevBuffer,
    out_ids: DevBuffer,
    pinned_out: DevBuffer,
}

/// MoE scratch (allocated only for Mixture-of-Experts models). The router
/// output is sized for a full prefill chunk; decode uses the first row.
struct MoeBufs {
    /// Selected expert ids, i32 [MAX_PREFILL_CHUNK * top_k].
    ids: DevBuffer,
    /// Routing weights, f32 [MAX_PREFILL_CHUNK * top_k].
    weights: DevBuffer,
    pinned_ids: DevBuffer,
    pinned_weights: DevBuffer,
    /// One token's FFN-normed hidden, f16 [hidden] — prefill copies a row here
    /// so the per-expert GEMV reads a contiguous single-token activation.
    xrow: DevBuffer,
    /// One expert's down-projection output, f16 [hidden].
    tmp: DevBuffer,
    /// Pinned-host landing for the shared-expert gate logit (f16), read back in
    /// the same sync as the router top-k (fallback readback path only).
    pinned_shared: DevBuffer,
    /// Device-resident shared-expert sigmoid gate scale (f32, one element). The
    /// device dispatch path computes it on-GPU so folding the shared expert
    /// needs no host round-trip.
    shared_scale: DevBuffer,
}

impl Model {
    pub fn load_gguf(device: Arc<dyn Device>, path: &Path, cfg: ModelConfig) -> Result<Self> {
        let weights = ModelWeights::load_gguf(&device, path, cfg.native_mtp)?;
        Self::finish(device, weights, cfg)
    }

    pub fn load_safetensors_dir(
        device: Arc<dyn Device>,
        dir: &Path,
        cfg: ModelConfig,
    ) -> Result<Self> {
        let weights = ModelWeights::load_safetensors_dir(&device, dir, cfg.native_mtp)?;
        Self::finish(device, weights, cfg)
    }

    fn finish(device: Arc<dyn Device>, weights: ModelWeights, cfg: ModelConfig) -> Result<Self> {
        let p = &weights.descriptor.params;
        // head_dim 256 has an f16-only attention specialization (qwen35moe
        // gated attention layers); the hybrid arch always uses the f16 cache.
        if p.head_dim != 64 && p.head_dim != 128 && p.head_dim != 256 {
            return Err(ForgeError::Unsupported(format!(
                "head_dim {} has no attention specialization",
                p.head_dim
            )));
        }
        if weights.is_moe() {
            // The routed decode path is a dedicated, non-graph-captured chain
            // over the f16 paged cache; low-bit KV modes and tiering are tracked
            // follow-ups (they need the fused decode kernels MoE bypasses).
            if !matches!(cfg.kv_quant, KvQuant::F16) {
                return Err(ForgeError::Unsupported(
                    "MoE models currently support only the f16 KV cache".into(),
                ));
            }
            // The hybrid `qwen35moe` arch (attention + Gated-DeltaNet MoE) DOES
            // tier: only its ~10 attention layers hold a paged KV cache, and
            // that cache spills/restores/streams through the same tier manager
            // as the dense path. The DeltaNet layers keep a small resident
            // recurrent state that is never paged. Non-hybrid MoE (OLMoE,
            // Qwen3-MoE) still lacks a staged-attention decode chain.
            let hybrid = weights.descriptor.params.ssm.is_some();
            if cfg.kv_tier.enabled() && !hybrid {
                return Err(ForgeError::Unsupported(
                    "non-hybrid MoE models do not support KV tiering yet".into(),
                ));
            }
        }
        match cfg.kv_quant {
            KvQuant::F16 => {}
            KvQuant::Fp8 => {
                // The non-fused decode chain (qkv_post + attn_decode) has no
                // fp8 cache kernels; fp8 decode goes through attn_decode_split
                // exclusively.
                if !Self::fused_decode_supported(&weights) {
                    return Err(ForgeError::Unsupported(
                        "kv_dtype fp8 requires the fused decode path; this model's weight \
                         formats fall back to the separate decode kernels"
                            .into(),
                    ));
                }
            }
            KvQuant::Rot { bits, .. } => {
                if bits != 3 && bits != 4 {
                    return Err(ForgeError::Unsupported(format!(
                        "rotational KV supports 3 or 4 bits, got {bits}"
                    )));
                }
                // Rot decode reads the packed store through attn_decode_rot;
                // prefill stays on the bit-exact f16 slab. Only head_dim 64/128
                // have compiled specializations (already checked above).
            }
        }
        let max_pages_per_seq = cfg.max_seq_len.div_ceil(cfg.kv_page_size);
        let kernels = Kernels::load(device.clone())?;
        let kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: p.block_count,
                n_kv_heads: p.n_kv_heads,
                head_dim: p.head_dim,
                page_size: cfg.kv_page_size,
                n_pages: cfg.kv_pages,
                max_pages_per_seq,
                quant: cfg.kv_quant,
            },
        )?;
        let stream = device.create_stream()?;
        let page_table_dev = device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Weights)?;
        let seq_len_dev = device.alloc(4, MemKind::Device, Pool::Weights)?;
        let (tier, tier_bufs) = if cfg.kv_tier.enabled() {
            let region_bytes = kv.cfg.tier_region_bytes();
            let mut slots = Vec::with_capacity(STAGE_SLOTS);
            for _ in 0..STAGE_SLOTS {
                let stage = region_bytes
                    .iter()
                    .map(|&rb| device.alloc(max_pages_per_seq * rb, MemKind::Device, Pool::Weights))
                    .collect::<Result<Vec<_>>>()?;
                slots.push(StageSlot {
                    stage,
                    ready: device.create_event()?,
                    free: device.create_event()?,
                });
            }
            let identity: Vec<i32> = (0..max_pages_per_seq as i32).collect();
            let identity_pt =
                device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Weights)?;
            device.write(bytemuck::cast_slice(&identity), &identity_pt, 0)?;
            // Tier only the attention layers: for a dense/rot model that is
            // every layer (`layer_kinds` is all-Attention, so behavior is
            // unchanged), for the hybrid arch it is the ~10 attention layers
            // (the DeltaNet layers keep a resident recurrent state, never paged).
            let tier_layers: Vec<usize> = weights
                .descriptor
                .layer_kinds
                .iter()
                .enumerate()
                .filter(|(_, k)| matches!(k, forge_formats::LayerKind::Attention))
                .map(|(i, _)| i)
                .collect();
            let tm = TierManager::new(
                cfg.kv_tier.clone(),
                device.clone(),
                tier_layers,
                region_bytes.clone(),
            )?;
            (
                Some(tm),
                Some(TierBufs {
                    slots,
                    identity_pt,
                    region_bytes,
                }),
            )
        } else {
            (None, None)
        };
        let hidden = p.hidden_size;
        let q_dim = p.n_heads.checked_mul(p.head_dim).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie wymiaru Q verifiera MTP".into())
        })?;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let attn_parts_bytes = p
            .n_heads
            .checked_mul(ATTN_DECODE_GQA_SPLITS)
            .and_then(|elements| elements.checked_mul(p.head_dim.checked_add(2)?))
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Format("przepełnienie bufora partiali attention GQA".into())
            })?;
        // Persistent decode scratch lives in the activation pool: it is the
        // pool provisioned for exactly this purpose, and nothing else uses it
        // on the LLM path anymore (the ring never needs to wrap).
        let alloc = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let bufs = DecodeBufs {
            h: alloc(hidden)?,
            h32: device.alloc(hidden * 4, MemKind::Device, Pool::Activations)?,
            x: alloc(hidden)?,
            qkv: alloc(q_dim + 2 * kv_dim)?,
            q: alloc(q_dim)?,
            k: alloc(kv_dim)?,
            v: alloc(kv_dim)?,
            attn_out: alloc(q_dim)?,
            attn_parts: device.alloc(attn_parts_bytes, MemKind::Device, Pool::Activations)?,
            o_out: alloc(hidden)?,
            gate_up: alloc(2 * inter)?,
            gate: alloc(inter)?,
            up: alloc(inter)?,
            act: alloc(inter)?,
            down: alloc(hidden)?,
            logits: device.alloc(p.vocab_size * 4, MemKind::Device, Pool::Activations)?,
            ids: device.alloc(4, MemKind::Device, Pool::Activations)?,
            pos: device.alloc(4, MemKind::Device, Pool::Activations)?,
            pinned_in: device.alloc(12, MemKind::PinnedHost, Pool::Activations)?,
            pinned_pt: device.alloc(
                max_pages_per_seq * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            pinned_logits: device.alloc(
                p.vocab_size * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
            sample_vals: device.alloc(
                forge_kernels::SAMPLE_SCRATCH_PAIRS * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
            sample_idx: device.alloc(
                forge_kernels::SAMPLE_SCRATCH_PAIRS * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
            sample_out: device.alloc(8, MemKind::Device, Pool::Activations)?,
            pinned_sample: device.alloc(8, MemKind::PinnedHost, Pool::Activations)?,
            penalty_ids: device.alloc(cfg.max_seq_len * 4, MemKind::Device, Pool::Activations)?,
            pinned_penalty: device.alloc(
                cfg.max_seq_len * 4,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
        };
        let moe_bufs = match &weights.descriptor.params.moe {
            Some(m) => {
                let top_k = m.n_experts_used;
                let idw = MAX_PREFILL_CHUNK * top_k;
                Some(MoeBufs {
                    ids: device.alloc(idw * 4, MemKind::Device, Pool::Activations)?,
                    weights: device.alloc(idw * 4, MemKind::Device, Pool::Activations)?,
                    pinned_ids: device.alloc(idw * 4, MemKind::PinnedHost, Pool::Activations)?,
                    pinned_weights: device.alloc(
                        idw * 4,
                        MemKind::PinnedHost,
                        Pool::Activations,
                    )?,
                    xrow: device.alloc(hidden * 2, MemKind::Device, Pool::Activations)?,
                    tmp: device.alloc(hidden * 2, MemKind::Device, Pool::Activations)?,
                    pinned_shared: device.alloc(2, MemKind::PinnedHost, Pool::Activations)?,
                    shared_scale: {
                        // Seed 1.0 so a shared expert without a per-token gate
                        // (no shared_gate) folds in unscaled; the device sigmoid
                        // kernel overwrites this each layer when a gate exists.
                        let sc = device.alloc(4, MemKind::Device, Pool::Activations)?;
                        device.write(&1.0f32.to_le_bytes(), &sc, 0)?;
                        sc
                    },
                })
            }
            None => None,
        };
        // Gated-DeltaNet recurrent state, one entry per DeltaNet layer (hybrid
        // arch only). Allocated once and reused; zeroed at each sequence start.
        let ssm = match &weights.descriptor.params.ssm {
            Some(sp) => {
                let conv_bytes = sp.conv_dim() * (sp.d_conv - 1) * 2;
                let state_bytes = sp.n_v_heads() * sp.d_state * sp.d_state * 4;
                weights
                    .descriptor
                    .layer_kinds
                    .iter()
                    .map(|k| match k {
                        forge_formats::LayerKind::DeltaNet => Ok(Some(SsmState {
                            conv: device.alloc(conv_bytes, MemKind::Device, Pool::Weights)?,
                            state: device.alloc(state_bytes, MemKind::Device, Pool::Weights)?,
                        })),
                        forge_formats::LayerKind::Attention => Ok(None),
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            None => Vec::new(),
        };
        // Prefix caching is a strict optimization: engage only where a borrowed
        // prefix page is byte-identical to a fresh prefill and never mutated.
        // That means the verbatim F16/Fp8 paged cache with no tiering (tiering
        // spills/rewrites pages), no rotational store (position-indexed residual
        // ring, not per-page) and no hybrid arch (recurrent SSM state is not in
        // KV pages). Otherwise the cache stays inactive and behavior is
        // bit-for-bit unchanged.
        let prefix_eligible = cfg.prefix_cache
            && !cfg.kv_tier.enabled()
            && matches!(cfg.kv_quant, KvQuant::F16 | KvQuant::Fp8)
            && weights.descriptor.params.ssm.is_none();
        let prefix_cache =
            prefix_eligible.then(|| crate::prefix::PrefixCache::new(cfg.kv_page_size));
        let mtp_state = if weights.mtp.is_some() {
            Some(MtpDraftState::new(
                device.clone(),
                KvConfig {
                    n_layers: 1,
                    n_kv_heads: p.n_kv_heads,
                    head_dim: p.head_dim,
                    page_size: cfg.kv_page_size,
                    n_pages: cfg.kv_pages,
                    max_pages_per_seq,
                    quant: KvQuant::F16,
                },
                hidden,
                p.vocab_size,
            )?)
        } else {
            None
        };
        Ok(Model {
            device,
            kernels,
            weights,
            kv,
            stream,
            page_table_dev,
            seq_len_dev,
            max_pages_per_seq,
            bufs,
            prefill_bufs: None,
            verify_bufs: None,
            hybrid_verify_bufs: None,
            decode_graph: None,
            decode_moe_graph: None,
            decode_rot_graph: None,
            batch_bufs: None,
            batch_graphs: HashMap::new(),
            batch_cap: 0,
            tier,
            tier_bufs,
            pt_seq: 0,
            moe_bufs,
            ssm,
            hybrid_bufs: None,
            hybrid_debug: std::env::var("FORGE_HYBRID_DEBUG").is_ok_and(|v| v == "1"),
            prefix_cache,
            calib: None,
            mtp_state,
        })
    }

    pub fn new_seq(&self) -> SeqKv {
        self.kv.new_seq()
    }

    pub fn release_seq(&mut self, seq: &mut SeqKv) {
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        if self.prefix_cache.is_some() {
            self.finalize_prefix(seq);
        }
        self.kv.release(seq);
        if let Some(state) = &mut self.mtp_state {
            state.release();
        }
    }

    pub fn tier_enabled(&self) -> bool {
        self.tier.is_some()
    }

    /// Whether the radix prefix cache is active for this model.
    pub fn prefix_enabled(&self) -> bool {
        self.prefix_cache.is_some()
    }

    /// Longest cached-prefix length (tokens) servable for `prompt`, leaving at
    /// least one token to prefill (so the sequence still produces logits). Used
    /// by admission to project the reduced page demand; no state change.
    pub fn prefix_match_len(&self, prompt: &[u32]) -> usize {
        match &self.prefix_cache {
            Some(pc) if prompt.len() > self.kv.cfg.page_size => {
                pc.match_len(prompt, prompt.len() - 1)
            }
            _ => 0,
        }
    }

    /// Borrow the longest cached prefix of `prompt` into `seq` (SPEC §5.2):
    /// shared pages are attached read-only, `seq.len`/`tokens`/`prefilled_len`
    /// advance to the shared boundary, and the divergent suffix is left to
    /// prefill. Returns the number of prompt tokens served from cache
    /// (`cache_read_tokens`). At least one token is always left to prefill.
    pub fn acquire_prefix(&mut self, seq: &mut SeqKv, prompt: &[u32]) -> usize {
        let ps = self.kv.cfg.page_size;
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        if prompt.len() <= ps {
            return 0;
        }
        let (pages, node, shared) = pc.acquire(prompt, prompt.len() - 1);
        if shared == 0 {
            return 0;
        }
        seq.pages = pages;
        seq.shared_pages = seq.pages.len();
        seq.prefix_node = node;
        seq.len = shared;
        // Keep `tokens` page-aligned with `pages` so the completion-time
        // donation indexes shared + private pages uniformly. The borrowed prefix
        // is prefill-built (bit-identical), so it counts toward `prefilled_len`.
        seq.tokens = prompt[..shared].to_vec();
        seq.prefilled_len = shared;
        // The single-stream decode path re-uploads the page table when a
        // different sequence's pages were resident; a borrow rewrites the table.
        self.pt_seq = 0;
        shared
    }

    /// Donate a completing sequence's freshly-prefilled complete pages back into
    /// the radix tree and release its borrow. Leading shared/donated pages are
    /// drained from `seq.pages` so the subsequent `kv.release` frees only the
    /// sequence's remaining private (partial + decode) pages.
    fn finalize_prefix(&mut self, seq: &mut SeqKv) {
        let ps = self.kv.cfg.page_size;
        let Some(node) = seq.prefix_node.take() else {
            // No borrow — but the sequence may still have prefilled a brand-new
            // prefix worth caching (cache miss). Donate from the root.
            let n_full = seq.prefilled_len / ps;
            if n_full == 0 {
                return;
            }
            let (dups, consumed) = {
                let pc = self.prefix_cache.as_mut().expect("prefix path");
                pc.donate(crate::prefix::ROOT, 0, n_full, &seq.tokens, &seq.pages)
            };
            for p in dups {
                self.kv.push_free(p);
            }
            seq.pages.drain(0..consumed.min(seq.pages.len()));
            seq.shared_pages = 0;
            return;
        };
        let n_full = seq.prefilled_len / ps;
        let (dups, consumed) = {
            let pc = self.prefix_cache.as_mut().expect("prefix path");
            let r = pc.donate(node, seq.shared_pages, n_full, &seq.tokens, &seq.pages);
            pc.release(node);
            r
        };
        for p in dups {
            self.kv.push_free(p);
        }
        seq.pages.drain(0..consumed.min(seq.pages.len()));
        seq.shared_pages = 0;
    }

    /// Reclaim up to `need` KV pages from the prefix cache (evicting refcount-0
    /// LRU prefixes) onto the free stack. No-op when the cache is inactive or
    /// already empty of evictable pages. Returns the number of pages freed.
    fn reclaim_prefix_pages(&mut self, need: usize) -> usize {
        let Some(pc) = self.prefix_cache.as_mut() else {
            return 0;
        };
        let freed = pc.evict(need);
        let n = freed.len();
        for p in freed {
            self.kv.push_free(p);
        }
        n
    }

    /// Ensure at least `need` free KV pages, evicting cached prefixes if the
    /// free stack is short. Called before prefill/decode growth so a cache hit
    /// never starves the pool.
    fn ensure_free_pages(&mut self, need: usize) {
        if self.prefix_cache.is_none() {
            return;
        }
        let free = self.kv.free_page_count();
        if free < need {
            self.reclaim_prefix_pages(need - free);
        }
    }

    /// Pages the engine can still hand out for a new request: the free stack
    /// plus everything the prefix cache can evict. Admission uses this so a
    /// reclaimable cache never blocks otherwise-fittable work.
    pub fn available_pages(&self) -> usize {
        self.kv.free_page_count()
            + self
                .prefix_cache
                .as_ref()
                .map(|pc| pc.evictable_pages())
                .unwrap_or(0)
    }

    /// Largest per-request KV demand (in pages) the engine can hold: the VRAM
    /// pool when tiering is off, the full context window when tiers extend it.
    pub fn max_request_pages(&self) -> usize {
        if self.tier.is_some() {
            self.max_pages_per_seq
        } else {
            self.kv.cfg.n_pages
        }
    }

    /// Whether `seq`'s spilled pages can be restored without dropping the pool
    /// below the watermark reserve — restoring tighter than that would only
    /// thrash (the next step's capacity check would spill the pages again).
    fn tier_can_restore(&self, seq: &SeqKv) -> bool {
        let Some(tier) = &self.tier else { return false };
        seq.spilled_page_count() + tier.reserve_pages(self.kv.cfg.n_pages)
            <= self.kv.free_page_count()
    }

    /// Cross-sequence eviction (SPEC §5.4B): spill the globally coldest pages
    /// — across every provided sequence — until the pool can absorb
    /// `upcoming_pages` of growth plus the watermark reserve. Sequences with
    /// the largest spillable cold prefix donate first, so one long-context
    /// request no longer stalls behind neighbors' cold history. No-op with
    /// tiering off.
    pub fn tier_balance(&mut self, seqs: &mut [&mut SeqKv], upcoming_pages: usize) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let need = upcoming_pages + tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need {
            return Ok(());
        }
        let mut deficit = need - free;
        while deficit > 0 {
            let Some((idx, spillable)) = seqs
                .iter()
                .enumerate()
                .map(|(i, s)| (i, tier.spillable_pages(s)))
                .filter(|&(_, sp)| sp > 0)
                .max_by_key(|&(_, sp)| sp)
            else {
                break;
            };
            let take = deficit.min(spillable);
            let done = tier.spill(&mut self.kv, &mut *seqs[idx], take, &self.stream)?;
            if done == 0 {
                break;
            }
            self.pt_seq = 0;
            deficit = deficit.saturating_sub(done);
        }
        Ok(())
    }

    /// Spill this sequence's coldest pages until the pool can absorb
    /// `new_tokens` more tokens plus the watermark reserve. No-op with
    /// tiering off (the pool then errors on exhaustion, as before).
    fn tier_ensure_capacity(&mut self, seq: &mut SeqKv, new_tokens: usize) -> Result<()> {
        let Some(tier) = &mut self.tier else {
            return Ok(());
        };
        let ps = self.kv.cfg.page_size;
        let need = (seq.len + new_tokens)
            .div_ceil(ps)
            .saturating_sub(seq.pages.len());
        let reserve = tier.reserve_pages(self.kv.cfg.n_pages);
        let free = self.kv.free_page_count();
        if free >= need + reserve {
            return Ok(());
        }
        let deficit = need + reserve - free;
        let spilled = tier.spill(&mut self.kv, seq, deficit, &self.stream)?;
        if spilled > 0 {
            self.pt_seq = 0;
        }
        Ok(())
    }

    /// Transfer-vs-recompute rule (SPEC §5.4): restore spilled chunks when the
    /// estimated transfer time beats re-prefilling the history. Recompute is
    /// only bit-identical for a purely prefilled history (decode writes its
    /// K/V through different kernels), so decode-extended sequences always
    /// transfer. Every decision is logged with the measured estimates.
    fn tier_restore_or_recompute(&mut self, seq: &mut SeqKv) -> Result<()> {
        let tier = self.tier.as_ref().expect("caller checked tiering");
        let (bytes, t_transfer) = tier.restore_cost(seq);
        let recompute_ok = seq.prefilled_len == seq.tokens.len() && !seq.tokens.is_empty();
        let t_recompute = tier.recompute_cost(seq.len);
        let use_recompute = recompute_ok && t_recompute < t_transfer;
        tracing::info!(
            "kv tier decision: seq {} transfer {:.1} MiB ≈ {:.1} ms vs recompute {} tok ≈ {:.1} ms → {}{}",
            seq.id,
            bytes as f64 / (1 << 20) as f64,
            t_transfer * 1e3,
            seq.len,
            t_recompute * 1e3,
            if use_recompute { "recompute" } else { "transfer" },
            if recompute_ok {
                ""
            } else {
                " (recompute ineligible: decode-written KV)"
            },
        );
        if use_recompute {
            self.recompute_seq(seq)
        } else {
            let tier = self.tier.as_mut().expect("checked above");
            tier.restore_all(&mut self.kv, seq, &self.stream)?;
            self.pt_seq = 0;
            Ok(())
        }
    }

    /// Rebuild `seq`'s KV from its retained tokens by re-prefilling from
    /// scratch, dropping all tier chunks first (recompute preemption).
    fn recompute_seq(&mut self, seq: &mut SeqKv) -> Result<()> {
        let toks = std::mem::take(&mut seq.tokens);
        if let Some(t) = &mut self.tier {
            t.drop_seq(seq);
        }
        self.kv.release(seq);
        self.pt_seq = 0;
        for chunk in toks.chunks(MAX_PREFILL_CHUNK) {
            if self.is_hybrid() {
                self.prefill_hybrid(seq, chunk)?;
            } else {
                self.prefill_forward(seq, chunk)?;
            }
        }
        Ok(())
    }

    fn gemv(&self, y: &DevBuffer, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Q8_0/Q4_K take the int8-activation dp4a kernels (measured faster at
        // every decode shape); columns beyond the kernels' shared staging
        // bound keep the f16-x path. Q6_K stays on f16 x: its dot is already
        // bandwidth-bound and the dp4a variant's extra shared usage costs
        // occupancy (measured slower at the down-projection shape).
        match w {
            DevWeight::F16 { buf, rows, cols } => {
                self.kernels.gemv_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q8_0_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q8_0_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q4_k_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                self.kernels.gemv_q6_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5K { buf, rows, cols } => {
                self.kernels.gemv_q5_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q3K { buf, rows, cols } => {
                self.kernels.gemv_q3_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q2K { buf, rows, cols } => {
                self.kernels.gemv_q2_k_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_0 { buf, rows, cols } => {
                self.kernels.gemv_q4_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q4_1 { buf, rows, cols } => {
                self.kernels.gemv_q4_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_0 { buf, rows, cols } => {
                self.kernels.gemv_q5_0_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q5_1 { buf, rows, cols } => {
                self.kernels.gemv_q5_1_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => {
                self.kernels.gemv_mxfp4_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => {
                self.kernels.gemv_iq2_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq3S { buf, rows, cols } => {
                self.kernels.gemv_iq3_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_f16(y, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => {
                self.kernels.gemv_iq1_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq1M { buf, rows, cols } => {
                self.kernels.gemv_iq1_m_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                rows,
                cols,
            } => self.kernels.gemv_nvfp4_f16(
                y,
                packed,
                scales,
                x,
                *rows,
                *cols,
                *inv_global_scale,
                stream,
            ),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
            } => self.kernels.gemv_nvfp4_gguf_q8_1_group_f16(
                &[Nvfp4GgufQ8Projection {
                    output: y,
                    weights: buf,
                    rows: *rows,
                    output_scale: *output_scale,
                }],
                x,
                *cols,
                stream,
            ),
        }
    }

    /// Wykonuje kilka pełnych projekcji GGUF NVFP4 ze wspólną kwantyzacją
    /// aktywacji Q8_1. Zwraca `false`, gdy choć jedna waga ma inny format.
    fn gemv_nvfp4_gguf_group(
        &self,
        projections: &[(&DevBuffer, &DevWeight)],
        x: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let mut cols = None;
        let mut group = Vec::with_capacity(projections.len());
        for &(output, weight) in projections {
            let DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols: weight_cols,
            } = weight
            else {
                return Ok(false);
            };
            if cols.is_some_and(|value| value != *weight_cols) {
                return Err(ForgeError::Format(
                    "projekcje NVFP4 współdzielące Q8_1 mają różne szerokości".into(),
                ));
            }
            cols = Some(*weight_cols);
            group.push(Nvfp4GgufQ8Projection {
                output,
                weights: buf,
                rows: *rows,
                output_scale: *output_scale,
            });
        }
        let Some(cols) = cols else { return Ok(false) };
        self.kernels
            .gemv_nvfp4_gguf_q8_1_group_f16(&group, x, cols, stream)?;
        Ok(true)
    }

    /// True when `w` can be consumed by the fused decode kernels
    /// (gemv_norm / gemv_norm_silu / gemv_residual format + column coverage).
    fn fused_decode_weight_ok(w: &DevWeight) -> bool {
        match w {
            DevWeight::F16 { cols, .. } => cols.is_multiple_of(8),
            DevWeight::Q8_0 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::NvFp4 { cols, .. } => cols.is_multiple_of(16),
            DevWeight::NvFp4Gguf { .. } => false,
            // Q4_K stages per-32-column x sums in shared memory
            // (Q4K_MAX_SEGS in gemv2.mojo bounds cols at 32768).
            DevWeight::Q4K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q6K { cols, .. } => cols.is_multiple_of(256),
            // Q5_K shares Q4_K's 32-column x-sum staging bound; Q2_K stages
            // 16-column sums with the same 32768 ceiling.
            DevWeight::Q5K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q3K { cols, .. } => cols.is_multiple_of(256),
            DevWeight::Q2K { cols, .. } => cols.is_multiple_of(256) && *cols <= 32768,
            DevWeight::Q4_0 { cols, .. }
            | DevWeight::Q4_1 { cols, .. }
            | DevWeight::Q5_0 { cols, .. }
            | DevWeight::Q5_1 { cols, .. }
            | DevWeight::Iq4Nl { cols, .. }
            | DevWeight::Mxfp4 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::Iq4Xs { cols, .. }
            | DevWeight::Iq2Xs { cols, .. }
            | DevWeight::Iq2S { cols, .. }
            | DevWeight::Iq3S { cols, .. }
            | DevWeight::Iq2Xxs { cols, .. }
            | DevWeight::Iq3Xxs { cols, .. }
            | DevWeight::Iq1S { cols, .. }
            | DevWeight::Iq1M { cols, .. } => cols.is_multiple_of(256),
        }
    }

    /// The fused decode step carries the residual stream as an (h, h32)
    /// pair with no standalone normed-x buffer and needs a hidden size that
    /// fits the kernels' shared-memory staging. QKV and gate/up may stay
    /// split (mixed formats, e.g. Q4_K q/k + Q6_K v, or Q5_K gate + Q6_K
    /// up): each projection then runs its own gemv_norm launch — same
    /// per-row math, only the norm recompute is repeated (gate/up adds an
    /// elementwise silu_mul). Anything else records the separate chain.
    fn fused_decode_supported(weights: &ModelWeights) -> bool {
        let p = &weights.descriptor.params;
        if p.hidden_size > 8192 {
            return false;
        }
        weights.layers.iter().all(|l| {
            // Routed MoE FFN has no fused single-GEMV decode kernel; MoE models
            // take the dedicated routed path (never this fused chain).
            let LayerFfn::Dense(dffn) = &l.ffn else {
                return false;
            };
            let qkv_ok = match &l.attn().attn_qkv {
                QkvWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                QkvWeights::FusedQk { qk, v } => {
                    Self::fused_decode_weight_ok(qk) && Self::fused_decode_weight_ok(v)
                }
                QkvWeights::Split { q, k, v } => {
                    Self::fused_decode_weight_ok(q)
                        && Self::fused_decode_weight_ok(k)
                        && Self::fused_decode_weight_ok(v)
                }
            };
            let gate_up_ok = match &dffn.gate_up {
                GateUpWeights::Fused(w) => Self::fused_decode_weight_ok(w),
                // Mixed-format gate/up (e.g. Q5_K gate + Q6_K up) stays in
                // the fused chain: each projection runs its own gemv_norm
                // and a silu_mul combines them (see record_step_fused).
                GateUpWeights::Split { gate, up } => {
                    Self::fused_decode_weight_ok(gate) && Self::fused_decode_weight_ok(up)
                }
            };
            qkv_ok
                && gate_up_ok
                && Self::fused_decode_weight_ok(&l.attn().attn_o)
                && Self::fused_decode_weight_ok(&dffn.down)
        })
    }

    /// Fused rmsnorm-recompute + GEMV over the decode residual pair (h, h32).
    fn gemv_norm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        norm_w: &DevBuffer,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        match w {
            DevWeight::F16 { buf, rows, cols } => self.kernels.gemv_norm_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_q8_0_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                rows,
                cols,
            } => self.kernels.gemv_norm_nvfp4_f16(
                y,
                packed,
                scales,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                *inv_global_scale,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q4K { buf, rows, cols } => self.kernels.gemv_norm_q4_k_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_q6_k_dp4a_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_q5_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_q3_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_q2_k_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_q4_0_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_q4_1_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_q5_0_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_q5_1_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_iq4_nl_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_iq4_xs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_mxfp4_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_iq2_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_iq3_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xxs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq3_xxs_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_iq1_s_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_iq1_m_f16(
                y,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                *rows,
                *cols,
                ss_from_h16,
                eps,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_norm nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    /// Fused rmsnorm-recompute + gate|up GEMV + SiLU. `w` is the fused
    /// gate|up matrix; its row count is 2 * inter.
    fn gemv_norm_silu(
        &self,
        act: &DevBuffer,
        w: &DevWeight,
        norm_w: &DevBuffer,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        match w {
            DevWeight::F16 { buf, rows, cols } => self.kernels.gemv_norm_silu_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q8_0_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                rows,
                cols,
            } => self.kernels.gemv_norm_silu_nvfp4_f16(
                act,
                packed,
                scales,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                *inv_global_scale,
                eps,
                stream,
            ),
            DevWeight::Q4K { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_k_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_silu_q6_k_dp4a_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_silu_q3_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_silu_q2_k_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_0_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_1_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_0_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_1_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_nl_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_xs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_silu_mxfp4_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xxs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_xxs_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_s_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_m_f16(
                act,
                buf,
                &b.h,
                &b.h32,
                norm_w,
                rows / 2,
                *cols,
                eps,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_norm_silu nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    /// GEMV + residual add into the decode residual pair (h, h32).
    fn gemv_residual(&self, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        // Same dp4a policy as `gemv`: Q8_0/Q4_K quantize x block-locally and
        // dot with dp4a (wins at every decode shape), Q6_K keeps the f16-x
        // kernel (already bandwidth-bound; dp4a's shared staging loses
        // occupancy at the wide down-projection).
        let b = &self.bufs;
        match w {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_residual_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q8_0_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q8_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                rows,
                cols,
            } => self.kernels.gemv_residual_nvfp4_f16(
                &b.h,
                &b.h32,
                packed,
                scales,
                x,
                *rows,
                *cols,
                *inv_global_scale,
                stream,
            ),
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q4_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q4_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_residual_q6_k_dp4a_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_residual_q6_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q3_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_residual_q2_k_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q4_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_0_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_residual_q5_1_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_nl_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq4_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_residual_mxfp4_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq2_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq3_xxs_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_s_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_residual_iq1_m_f16(&b.h, &b.h32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "scalony gemv_residual nie obsługuje jeszcze GGUF NVFP4".into(),
            )),
        }
    }

    fn logits_gemv(&self, y_f32: &DevBuffer, x: &DevBuffer, stream: &Stream) -> Result<()> {
        if let Some(weight) = &self.weights.fp8_lm_head {
            return self.kernels.gemv_fp8_out_f32(
                y_f32,
                &weight.qweight,
                &weight.scales,
                x,
                weight.rows,
                weight.cols,
                stream,
            );
        }
        self.logits_weight_gemv(y_f32, x, &self.weights.lm_head, stream)
    }

    fn logits_weight_gemv(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        weight: &DevWeight,
        stream: &Stream,
    ) -> Result<()> {
        match weight {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_f16_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemv_q8_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q4_k_dp4a_out_f32(y_f32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q4_k_out_f32(y_f32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q6_k_dp4a_out_f32(y_f32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels
                        .gemv_q6_k_out_f32(y_f32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q5K { buf, rows, cols } => self
                .kernels
                .gemv_q5_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q3K { buf, rows, cols } => self
                .kernels
                .gemv_q3_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q2K { buf, rows, cols } => self
                .kernels
                .gemv_q2_k_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_0 { buf, rows, cols } => self
                .kernels
                .gemv_q4_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q4_1 { buf, rows, cols } => self
                .kernels
                .gemv_q4_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_0 { buf, rows, cols } => self
                .kernels
                .gemv_q5_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q5_1 { buf, rows, cols } => self
                .kernels
                .gemv_q5_1_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Nl { buf, rows, cols } => self
                .kernels
                .gemv_iq4_nl_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq4Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq4_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Mxfp4 { buf, rows, cols } => self
                .kernels
                .gemv_mxfp4_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2S { buf, rows, cols } => self
                .kernels
                .gemv_iq2_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3S { buf, rows, cols } => self
                .kernels
                .gemv_iq3_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq2Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq2_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq3Xxs { buf, rows, cols } => self
                .kernels
                .gemv_iq3_xxs_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1S { buf, rows, cols } => self
                .kernels
                .gemv_iq1_s_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Iq1M { buf, rows, cols } => self
                .kernels
                .gemv_iq1_m_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4 { .. } => Err(ForgeError::Unsupported(
                "NVFP4 lm_head has no f32-logit kernel yet".into(),
            )),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "GGUF NVFP4 lm_head nie ma jeszcze kernela logitów f32".into(),
            )),
        }
    }

    /// Batched GEMM over row-major activations x[t][col].
    fn gemm(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_rows(y, w, x, n_tokens, 0, w.rows(), stream)
    }

    /// W4A8 prefill projection GEMM (per-token int8 activation quant + int4xint8
    /// GEMM). Each W4A8 weight is a standalone logical matrix, so no windowing.
    fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        w: &W4A8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels.gemm_w4a8(
            y,
            &w.qweight,
            &w.s2_zeros,
            &w.s2_scales,
            &w.s1_scales,
            &w.inv_smooth,
            x,
            w.rows,
            w.cols,
            n_tokens,
            stream,
        )
    }

    /// fp8 (e4m3) prefill projection GEMM (per-token e4m3 activation quant +
    /// e4m3×e4m3 tensor-core GEMM). Each fp8 weight is a standalone logical
    /// matrix, so no windowing. `FORGE_GEMM=fp8`.
    fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if self.weights.fp8_modular {
            return self.kernels.gemm_fp8_modular(
                y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
            );
        }
        self.kernels.gemm_fp8(
            y, &w.qweight, &w.scales, x, w.rows, w.cols, n_tokens, stream,
        )
    }

    /// fp8mod projection over the shared per-token e4m3 activation the preceding
    /// fused rmsnorm→fp8 emitted (q/k/v share one, gate/up share one) — no
    /// per-projection activation requant. `FORGE_GEMM=fp8mod` only.
    fn gemm_fp8_prequant(
        &self,
        y: &DevBuffer,
        w: &Fp8Weight,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.kernels
            .gemm_fp8_modular_prequant(y, &w.qweight, &w.scales, w.rows, w.cols, n_tokens, stream)
    }

    /// Batched GEMM over a row window of `w`: y = W[row_off..row_off+n_rows]·x.
    /// Row offsets translate to per-format byte offsets into the weight (and,
    /// for NVFP4, scale) streams — this is how prefill reads the q/k/v and
    /// gate/up sections out of a fused matrix without storing them twice.
    #[allow(clippy::too_many_arguments)]
    fn gemm_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        n_tokens: usize,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::F16 { buf, cols, .. } => self.kernels.gemm_f16_at(
                y,
                buf,
                row_off * cols * 2,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            // Q8_0 / Q4_K prefill run the int8 TENSOR-CORE MMQ GEMM: activations
            // quantized to q8_1, weights kept as native codes, s8xs8->s32 mma
            // (m16n8k32) per 32-block, then per-block scale/min to f16. This is
            // the only path that beats the f16 tensor-core GEMM on Ada (2x MAC
            // throughput + zero dequant bandwidth). Decode still uses the dp4a
            // GEMV (see gemv). Marshalling the mma's 4x s32 output uses
            // inlined_assembly + _RegisterPackType (see kernels/mojo/MOJO_NOTES.md).
            DevWeight::Q8_0 { buf, cols, .. } => self.kernels.gemm_q8_0_i8mma_at(
                y,
                buf,
                row_off * (cols / 32) * 34,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4K { buf, cols, .. } => self.kernels.gemm_q4_k_i8mma_at(
                y,
                buf,
                row_off * (cols / 256) * 144,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemm_q6_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 210,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5K { buf, cols, .. } => self.kernels.gemm_q5_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 176,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q3K { buf, cols, .. } => self.kernels.gemm_q3_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 110,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q2K { buf, cols, .. } => self.kernels.gemm_q2_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 84,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_0 { buf, cols, .. } => self.kernels.gemm_q4_0_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 18,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4_1 { buf, cols, .. } => self.kernels.gemm_q4_1_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 20,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_0 { buf, cols, .. } => self.kernels.gemm_q5_0_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 22,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q5_1 { buf, cols, .. } => self.kernels.gemm_q5_1_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 24,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Nl { buf, cols, .. } => self.kernels.gemm_iq4_nl_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 18,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq4Xs { buf, cols, .. } => self.kernels.gemm_iq4_xs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 136,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Mxfp4 { buf, cols, .. } => self.kernels.gemm_mxfp4_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 17,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xs { buf, cols, .. } => self.kernels.gemm_iq2_xs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 74,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2S { buf, cols, .. } => self.kernels.gemm_iq2_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 82,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3S { buf, cols, .. } => self.kernels.gemm_iq3_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 110,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq2Xxs { buf, cols, .. } => self.kernels.gemm_iq2_xxs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 66,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq3Xxs { buf, cols, .. } => self.kernels.gemm_iq3_xxs_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 98,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1S { buf, cols, .. } => self.kernels.gemm_iq1_s_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 50,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Iq1M { buf, cols, .. } => self.kernels.gemm_iq1_m_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 56,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                cols,
                ..
            } => self.kernels.gemm_nvfp4_f16_at(
                y,
                packed,
                row_off * (cols / 2),
                scales,
                row_off * (cols / 16),
                x,
                n_rows,
                *cols,
                n_tokens,
                *inv_global_scale,
                stream,
            ),
            DevWeight::NvFp4Gguf {
                buf,
                output_scale,
                rows,
                cols,
            } if row_off == 0 && n_rows == *rows => self.kernels.gemm_nvfp4_gguf_f16(
                y,
                buf,
                x,
                *rows,
                *cols,
                n_tokens,
                *output_scale,
                stream,
            ),
            DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                "GGUF NVFP4 GEMM nie obsługuje okna wierszy".into(),
            )),
        }
    }

    /// Single-token GEMV over a row window of `w` (`y = W[row_off..+n_rows]·x`).
    /// The routed-MoE expert path uses this instead of the batched `gemm_rows`:
    /// a decode step feeds one token, and the GEMM tile (BM=64) then launches
    /// only `n_rows/64` blocks — far too few to saturate the SMs, so the GPU
    /// stays at idle clocks. The per-row GEMV kernels launch `n_rows/8` blocks
    /// (8 experts queued back-to-back per layer keep the device busy enough to
    /// boost). Formats without an offset GEMV variant fall back to the tile.
    fn gemv_rows(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        row_off: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::Q4K { buf, cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_at(
                    y,
                    buf,
                    row_off * (cols / 256) * 144,
                    x,
                    n_rows,
                    *cols,
                    stream,
                )
            }
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemv_q6_k_f16_at(
                y,
                buf,
                row_off * (cols / 256) * 210,
                x,
                n_rows,
                *cols,
                stream,
            ),
            _ => self.gemm_rows(y, w, x, 1, row_off, n_rows, stream),
        }
    }

    /// Whether `w` is a routed-expert stack the device-side grouped dispatch can
    /// index without a host readback: the dp4a Q4_K path (cols within the dp4a
    /// bound) and the warp-per-row Q6_K path have `_gidx` kernels that read the
    /// expert row offset on-device. Other quants keep the host-readback loop.
    fn expert_stack_gidx(w: &DevWeight) -> bool {
        matches!(
            w,
            DevWeight::Q4K { cols, .. } if *cols <= Kernels::DP4A_MAX_COLS
        ) || matches!(w, DevWeight::Q6K { .. })
    }

    /// True when every routed-expert projection of `moe` supports the
    /// device-indexed dispatch (so the whole layer runs with zero host readback).
    fn moe_gidx_capable(moe: &MoeFfn) -> bool {
        Self::expert_stack_gidx(&moe.gate_exps)
            && Self::expert_stack_gidx(&moe.up_exps)
            && Self::expert_stack_gidx(&moe.down_exps)
    }

    /// Single-token GEMV over the expert selected on-device by `ids[sel]`: the
    /// device analog of `gemv_rows`, reading the expert row offset
    /// (`ids[sel] * rows_per_expert`) inside the kernel. Bit-identical to
    /// `gemv_rows(y, w, x, ids[sel]*rows_per_expert, n_rows, ..)`. Only the
    /// quants `expert_stack_gidx` accepts reach here.
    #[allow(clippy::too_many_arguments)]
    fn gemv_rows_gidx(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        x: &DevBuffer,
        ids: &DevBuffer,
        sel: usize,
        rows_per_expert: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::Q4K { buf, cols, .. } if *cols <= Kernels::DP4A_MAX_COLS => {
                self.kernels.gemv_q4_k_dp4a_f16_gidx(
                    y,
                    buf,
                    x,
                    n_rows,
                    *cols,
                    ids,
                    sel,
                    rows_per_expert,
                    stream,
                )
            }
            DevWeight::Q6K { buf, cols, .. } => self.kernels.gemv_q6_k_f16_gidx(
                y,
                buf,
                x,
                n_rows,
                *cols,
                ids,
                sel,
                rows_per_expert,
                stream,
            ),
            _ => Err(ForgeError::Unsupported(
                "gemv_rows_gidx called for a non-gidx expert quant".into(),
            )),
        }
    }

    fn ensure_prefill_bufs(&mut self) -> Result<()> {
        if self.prefill_bufs.is_some() {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let t_max = MAX_PREFILL_CHUNK;
        let alloc = |elems: usize| {
            self.device
                .alloc(elems * 2, MemKind::Device, Pool::Activations)
        };
        self.prefill_bufs = Some(PrefillBufs {
            h: alloc(t_max * hidden)?,
            x: alloc(t_max * hidden)?,
            q: alloc(t_max * q_dim)?,
            k: alloc(t_max * kv_dim)?,
            v: alloc(t_max * kv_dim)?,
            attn_out: alloc(t_max * q_dim)?,
            o_out: alloc(t_max * hidden)?,
            gate: alloc(t_max * inter)?,
            up: alloc(t_max * inter)?,
            act: alloc(t_max * inter)?,
            down: alloc(t_max * hidden)?,
            ids: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
            positions: self
                .device
                .alloc(t_max * 4, MemKind::Device, Pool::Activations)?,
        });
        Ok(())
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through every
    /// transformer block in one batched pass, appending K/V to `seq`. Leaves
    /// the final-norm hidden states for the chunk's `t` tokens in
    /// `prefill_bufs.x` as a `[t, hidden]` row-major f16 matrix and returns
    /// `t`. The device is synchronized before returning (the stream is idle),
    /// so callers may read `x` or launch further work. `prefill_chunk` maps
    /// the last row through the lm_head for next-token logits; `embed` pools
    /// the rows into a sentence vector.
    fn prefill_forward(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<usize> {
        let p = self.weights.descriptor.params.clone();
        let t = tokens.len();
        if t == 0 {
            return Err(ForgeError::Scheduler("empty prefill chunk".into()));
        }
        if t > MAX_PREFILL_CHUNK {
            return Err(ForgeError::Scheduler(format!(
                "prefill chunk {t} exceeds MAX_PREFILL_CHUNK {MAX_PREFILL_CHUNK}"
            )));
        }
        let base_pos = seq.len;
        if base_pos + t > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                base_pos + t - 1,
                p.max_position_embeddings
            )));
        }
        self.tier_ensure_capacity(seq, t)?;
        // Record tokens + prefilled_len for the tier recompute path AND the
        // prefix cache (which donates prefill-built pages into the radix tree on
        // completion). Both need the token ids retained and the still-purely-
        // prefill span tracked; with neither active this stays a no-op and
        // behavior is bit-for-bit unchanged.
        if self.tier.is_some() || self.prefix_cache.is_some() {
            if seq.tokens.len() == seq.prefilled_len {
                seq.prefilled_len += t;
            }
            seq.tokens.extend_from_slice(tokens);
        }
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        // Free pool space for the pages this chunk will grow into by evicting
        // cached prefixes (no-op unless the prefix cache is active and short).
        let new_pages = (seq.len + t)
            .div_ceil(self.kv.cfg.page_size)
            .saturating_sub(seq.pages.len());
        self.ensure_free_pages(new_pages);
        self.ensure_prefill_bufs()?;
        for _ in 0..t {
            self.kv.grow(seq)?;
        }

        // The stream is idle here (every step/prefill ends with a sync), so
        // plain synchronous writes are safe and prefill is not launch-latency
        // critical. Upload the full page table: the chunk's attention reads
        // pages for positions 0..base_pos+T.
        let mut pt = vec![-1i32; self.max_pages_per_seq];
        pt[..seq.pages.len()].copy_from_slice(&seq.pages);
        self.device
            .write(bytemuck::cast_slice(&pt), &self.page_table_dev, 0)?;
        self.pt_seq = seq.id;
        // Spilled sequences run each layer's attention over the staging slabs
        // (full context streamed in from the tiers); resident sequences read
        // the paged cache directly. Spilled page-table entries are -1 — only
        // the append (which touches resident tail pages) may consult them.
        let streamed = !seq.spilled.is_empty();
        if streamed {
            self.tier
                .as_mut()
                .expect("spilled pages imply tiering")
                .prepare_streaming(seq)?;
        }
        // W4A8 SmoothQuant calibration: pull the accumulator out of `self` so
        // the per-layer captures can borrow it mutably alongside the immutable
        // `pb`/`device` borrows. Restored before return; `None` in normal runs.
        let mut calib = self.calib.take();
        let pb = self.prefill_bufs.as_ref().expect("allocated above");
        let ids: Vec<i32> = tokens.iter().map(|&id| id as i32).collect();
        self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
        let positions: Vec<i32> = (base_pos..base_pos + t).map(|pos| pos as i32).collect();
        self.device
            .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;

        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let eps = p.rms_norm_eps;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let kernels = &self.kernels;
        let stream = &self.stream;

        // fp8mod fused-norm path: the Modular fp8 GEMM shares ONE per-token e4m3
        // activation across q/k/v (and across gate/up) by folding the activation
        // quant into the preceding RMSNorm. Only when the fp8 packs are loaded, the
        // Modular kernel is selected, and no W4A8 calibration is capturing mid-layer
        // f16 activations.
        //
        // Q4_K prefill takes the plain rmsnorm (f16) → per-projection
        // `quantize_act_q8_1` → native int8 GEMM sequence (`gemm_q4_k_i8mma_at`
        // routes to `gemm_q4k_i8_native`). The old fused rmsnorm→`block_q8_1_mmq`
        // path was an MMQ-only perf optimization (one shared DS4 activation across
        // q/k/v & gate/up); it was retired with the CUDA MMQ kernel. The
        // shared-activation reuse can be re-added on top of the native kernel later.
        let fp8mod_fuse = self.weights.fp8.is_some() && self.weights.fp8_modular && calib.is_none();
        let fp8mod_ffn_fuse =
            self.weights.fp8_ffn.is_some() && self.weights.fp8_modular && calib.is_none();

        let mut trace = PrefillTrace::new();
        trace.start(self.device.as_ref());

        kernels.gather_rows_f16(
            &pb.h,
            &self.weights.token_embd_f16,
            &pb.ids,
            t,
            hidden,
            stream,
        )?;
        // Layer 0's attn-norm feeds the q/k/v projections.
        if fp8mod_fuse {
            kernels.rmsnorm_fp8_shared(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                t,
                hidden,
                eps,
                stream,
            )?;
        } else {
            kernels.rmsnorm_f16(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                t,
                hidden,
                eps,
                stream,
            )?;
        }
        trace.mark(self.device.as_ref(), "embed");

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // W4A8 prefill (non-default): each projection is its own logical
            // pack, so q/k/v are three standalone GEMMs. The Q4_K weights stay
            // loaded for decode + the logit head.
            let w4a8_layer = self.weights.w4a8.as_ref().map(|v| &v[l]);
            let fp8_layer = self.weights.fp8.as_ref().map(|v| &v[l]);
            let fp8_ffn_layer = self.weights.fp8_ffn.as_ref().map(|v| &v[l]);

            // Calibration capture 1/4: q/k/v input (attn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.attn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            // Prefill outputs must stay [T, dim] contiguous per projection
            // (attention/rope/append index (t*heads+h)*head_dim), so a fused
            // matrix is consumed as three row-window GEMMs into separate
            // buffers — same weight bytes, no second copy in VRAM.
            if let Some(wl) = w4a8_layer {
                self.gemm_w4a8(&pb.q, &wl.q, &pb.x, t, stream)?;
                self.gemm_w4a8(&pb.k, &wl.k, &pb.x, t, stream)?;
                self.gemm_w4a8(&pb.v, &wl.v, &pb.x, t, stream)?;
            } else if let Some(fl) = fp8_layer {
                if fp8mod_fuse {
                    // q/k/v read the shared per-token e4m3 activation the attn-norm
                    // already emitted — no per-projection requant.
                    self.gemm_fp8_prequant(&pb.q, &fl.q, t, stream)?;
                    self.gemm_fp8_prequant(&pb.k, &fl.k, t, stream)?;
                    self.gemm_fp8_prequant(&pb.v, &fl.v, t, stream)?;
                } else {
                    self.gemm_fp8(&pb.q, &fl.q, &pb.x, t, stream)?;
                    self.gemm_fp8(&pb.k, &fl.k, &pb.x, t, stream)?;
                    self.gemm_fp8(&pb.v, &fl.v, &pb.x, t, stream)?;
                }
            } else if let Some(fl) = fp8_ffn_layer {
                self.gemm_fp8(&pb.q, &fl.q, &pb.x, t, stream)?;
                match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemm_rows(&pb.k, w, &pb.x, t, q_dim, kv_dim, stream)?;
                        self.gemm_rows(&pb.v, w, &pb.x, t, q_dim + kv_dim, kv_dim, stream)?;
                    }
                    QkvWeights::FusedQk { qk, v } => {
                        self.gemm_rows(&pb.k, qk, &pb.x, t, q_dim, kv_dim, stream)?;
                        self.gemm(&pb.v, v, &pb.x, t, stream)?;
                    }
                    QkvWeights::Split { k, v, .. } => {
                        self.gemm(&pb.k, k, &pb.x, t, stream)?;
                        self.gemm(&pb.v, v, &pb.x, t, stream)?;
                    }
                }
            } else {
                match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemm_rows(&pb.q, w, &pb.x, t, 0, q_dim, stream)?;
                        self.gemm_rows(&pb.k, w, &pb.x, t, q_dim, kv_dim, stream)?;
                        self.gemm_rows(&pb.v, w, &pb.x, t, q_dim + kv_dim, kv_dim, stream)?;
                    }
                    QkvWeights::FusedQk { qk, v } => {
                        self.gemm_rows(&pb.q, qk, &pb.x, t, 0, q_dim, stream)?;
                        self.gemm_rows(&pb.k, qk, &pb.x, t, q_dim, kv_dim, stream)?;
                        self.gemm(&pb.v, v, &pb.x, t, stream)?;
                    }
                    QkvWeights::Split { q, k, v } => {
                        self.gemm(&pb.q, q, &pb.x, t, stream)?;
                        self.gemm(&pb.k, k, &pb.x, t, stream)?;
                        self.gemm(&pb.v, v, &pb.x, t, stream)?;
                    }
                }
            }
            trace.mark(self.device.as_ref(), "gemm_qkv");

            // QK-norm granularity: OLMoE normalizes the whole q/k projection
            // once per token (rows = t), Qwen3 normalizes per head (rows =
            // t*n_heads). Dense non-OLMoE arches keep the per-head form
            // bit-for-bit (qk_norm_over_hidden == false).
            if let Some(qn) = &layer.attn().q_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&pb.q, &pb.q, qn, t, q_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(
                        &pb.q,
                        &pb.q,
                        qn,
                        t * p.n_heads,
                        p.head_dim,
                        eps,
                        stream,
                    )?;
                }
            }
            if let Some(kn) = &layer.attn().k_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&pb.k, &pb.k, kn, t, kv_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(
                        &pb.k,
                        &pb.k,
                        kn,
                        t * p.n_kv_heads,
                        p.head_dim,
                        eps,
                        stream,
                    )?;
                }
            }

            kernels.rope_neox_f16(
                &pb.q,
                &pb.positions,
                t,
                p.n_heads,
                p.head_dim,
                p.rope_theta,
                stream,
            )?;
            kernels.rope_neox_f16(
                &pb.k,
                &pb.positions,
                t,
                p.n_kv_heads,
                p.head_dim,
                p.rope_theta,
                stream,
            )?;
            trace.mark(self.device.as_ref(), "norm_rope");

            if let KvQuant::Rot { bits, .. } = self.kv.cfg.quant {
                // Rot: rotate+quant the chunk's rope'd K/V (linear pb.k/pb.v)
                // straight into the full-history packed store + residual ring —
                // no f16 slab. Packing must land before the attention launch,
                // which reads the packed store causally.
                let ring_slots = self
                    .kv
                    .cfg
                    .quant
                    .ring_slots()
                    .expect("rot mode has ring_slots");
                kernels.kv_pack_rot(
                    &self.kv.k_packed[l],
                    &self.kv.v_packed[l],
                    &self.kv.k_scale[l],
                    &self.kv.v_scale[l],
                    &self.kv.k[l],
                    &self.kv.v[l],
                    &pb.k,
                    0,
                    &pb.v,
                    0,
                    &self.page_table_dev,
                    &pb.positions,
                    t,
                    p.n_kv_heads,
                    self.kv.cfg.page_size,
                    p.head_dim,
                    ring_slots,
                    bits,
                    stream,
                )?;
                trace.mark(self.device.as_ref(), "kv_pack_rot");
                if streamed {
                    // The chunk's packed K/V just landed in resident tail
                    // pages; staging pulls the full logical history (spilled
                    // chunks + resident pages) so the causal attention sees
                    // every position through the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                } else {
                    kernels.attn_prefill_rot(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k_packed[l],
                        &self.kv.v_packed[l],
                        &self.kv.k_scale[l],
                        &self.kv.v_scale[l],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            } else {
                // Causal attention reads the chunk's own K/V from the cache, so
                // the batch append must land before the attention launch.
                kernels.kv_append_batch(
                    &self.kv.k[l],
                    &self.kv.v[l],
                    &pb.k,
                    &pb.v,
                    &self.page_table_dev,
                    base_pos,
                    t,
                    p.n_kv_heads,
                    self.kv.cfg.page_size,
                    p.head_dim,
                    self.kv.cfg.dtype(),
                    stream,
                )?;
                trace.mark(self.device.as_ref(), "kv_append");
                if streamed {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("streamed prefill requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &slot.stage[0],
                        &slot.stage[1],
                        &tb.identity_pt,
                        base_pos,
                        t,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        stream,
                    )?;
                } else {
                    kernels.attn_prefill(
                        &pb.attn_out,
                        &pb.q,
                        &self.kv.k[l],
                        &self.kv.v[l],
                        &self.page_table_dev,
                        base_pos,
                        t,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        self.kv.cfg.dtype(),
                        scale,
                        stream,
                    )?;
                }
                trace.mark(self.device.as_ref(), "attn");
            }

            // Calibration capture 2/4: o_proj input (attention output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.attn_out,
                    &mut cal.attn_out[l],
                    t,
                    &mut cal.scratch,
                )?;
            }
            if let Some(wl) = w4a8_layer {
                self.gemm_w4a8(&pb.o_out, &wl.attn_o, &pb.attn_out, t, stream)?;
            } else if let Some(fl) = fp8_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, t, stream)?;
            } else if let Some(fl) = fp8_ffn_layer {
                self.gemm_fp8(&pb.o_out, &fl.attn_o, &pb.attn_out, t, stream)?;
            } else {
                self.gemm(&pb.o_out, &layer.attn().attn_o, &pb.attn_out, t, stream)?;
            }
            trace.mark(self.device.as_ref(), "gemm_o");
            let fp8mod_fuse_gateup =
                (fp8mod_fuse || fp8mod_ffn_fuse) && matches!(layer.ffn, LayerFfn::Dense(_));
            if fp8mod_fuse_gateup {
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    t,
                    hidden,
                    eps,
                    stream,
                )?;
            } else {
                kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    t,
                    hidden,
                    eps,
                    stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res");

            // Calibration capture 3/4: gate/up input (ffn-norm output).
            if let Some(cal) = calib.as_mut() {
                self.device.synchronize()?;
                CalibAccum::absorb(
                    self.device.as_ref(),
                    &pb.x,
                    &mut cal.ffn_in[l],
                    t,
                    &mut cal.scratch,
                )?;
            }

            match &layer.ffn {
                LayerFfn::Dense(dffn) => {
                    if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.gate, &wl.gate, &pb.x, t, stream)?;
                        self.gemm_w4a8(&pb.up, &wl.up, &pb.x, t, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        if fp8mod_fuse {
                            self.gemm_fp8_prequant(&pb.gate, &fl.gate, t, stream)?;
                            self.gemm_fp8_prequant(&pb.up, &fl.up, t, stream)?;
                        } else {
                            self.gemm_fp8(&pb.gate, &fl.gate, &pb.x, t, stream)?;
                            self.gemm_fp8(&pb.up, &fl.up, &pb.x, t, stream)?;
                        }
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8_prequant(&pb.gate, &fl.gate, t, stream)?;
                        self.gemm_fp8_prequant(&pb.up, &fl.up, t, stream)?;
                    } else {
                        match &dffn.gate_up {
                            GateUpWeights::Fused(w) => {
                                self.gemm_rows(&pb.gate, w, &pb.x, t, 0, inter, stream)?;
                                self.gemm_rows(&pb.up, w, &pb.x, t, inter, inter, stream)?;
                            }
                            GateUpWeights::Split { gate, up } => {
                                self.gemm(&pb.gate, gate, &pb.x, t, stream)?;
                                self.gemm(&pb.up, up, &pb.x, t, stream)?;
                            }
                        }
                    }
                    trace.mark(self.device.as_ref(), "gemm_gateup");
                    kernels.silu_mul_f16(&pb.act, &pb.gate, &pb.up, t * inter, stream)?;
                    trace.mark(self.device.as_ref(), "silu");
                    // Calibration capture 4/4: down_proj input (SwiGLU output).
                    if let Some(cal) = calib.as_mut() {
                        self.device.synchronize()?;
                        CalibAccum::absorb(
                            self.device.as_ref(),
                            &pb.act,
                            &mut cal.down_in[l],
                            t,
                            &mut cal.scratch,
                        )?;
                    }
                    if let Some(wl) = w4a8_layer {
                        self.gemm_w4a8(&pb.down, &wl.down, &pb.act, t, stream)?;
                    } else if let Some(fl) = fp8_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, t, stream)?;
                    } else if let Some(fl) = fp8_ffn_layer {
                        self.gemm_fp8(&pb.down, &fl.down, &pb.act, t, stream)?;
                    } else {
                        self.gemm(&pb.down, &dffn.down, &pb.act, t, stream)?;
                    }
                    trace.mark(self.device.as_ref(), "gemm_down");
                }
                LayerFfn::Moe(moe) => {
                    // Per-token routed experts written into pb.down [t, hidden].
                    self.moe_prefill_ffn(moe, t, hidden, stream)?;
                    trace.mark(self.device.as_ref(), "moe_ffn");
                }
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            // This norm feeds the NEXT layer's q/k/v (or, for the last layer, the
            // logit head — never fused, keeps the f16 hidden state).
            if l + 1 < n_layers && fp8mod_fuse {
                kernels.rmsnorm_residual_fp8_shared(
                    &pb.x, &pb.h, &pb.down, next_norm, t, hidden, eps, stream,
                )?;
            } else {
                kernels.rmsnorm_residual_f16(
                    &pb.x, &pb.h, &pb.down, next_norm, t, hidden, eps, stream,
                )?;
            }
            trace.mark(self.device.as_ref(), "norm_res2");
        }

        // The stream is drained here so callers can read the hidden states or
        // launch the logits/pool tail without an additional sync of their own.
        self.device.synchronize()?;
        if let (Some(tier), Some(t0)) = (&self.tier, tier_t0) {
            // Measured prefill rate feeds the transfer-vs-recompute estimate.
            tier.note_prefill(t, t0.elapsed().as_secs_f64());
        }
        trace.report(t);
        self.calib = calib;
        Ok(t)
    }

    /// Build the W4A8 SmoothQuant packs from a one-time calibration pass over
    /// `calib_tokens` (a fixed built-in passage tokenized by the caller). Runs
    /// the coherent Q4_K prefill path collecting per-input-channel activation
    /// abs-max at the four linear inputs, then repacks every dense projection
    /// from the resident GGUF weights with per-channel migration folded into the
    /// weight (and the reciprocal into the GEMM's activation quantizer). Must be
    /// called once after load when `FORGE_GEMM=w4a8`; before it runs `w4a8` is
    /// `None` and prefill stays on the Q4_K path.
    pub fn calibrate_w4a8(&mut self, path: &Path, calib_tokens: &[u32]) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "W4A8 calibration supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        if calib_tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty W4A8 calibration input".into()));
        }
        let p = &self.weights.descriptor.params;
        let n_layers = self.weights.layers.len();
        let (hidden, q_dim, inter) = (p.hidden_size, p.n_heads * p.head_dim, p.intermediate_size);
        // Default is the identity requant (no SmoothQuant): measured best on the
        // Q4_K→W4A8 path, where the two-level requant error dominates and
        // migrating activation outliers only inflates the weights (see
        // docs/BENCH_COMPARISON.md). `FORGE_W4A8_ALPHA=<0..1>` opts into
        // SmoothQuant and triggers the calibration forward.
        let alpha = std::env::var("FORGE_W4A8_ALPHA")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(-1.0);

        // Ensure the Q4_K path runs during calibration (packs not built yet).
        self.weights.w4a8 = None;
        let stats = if alpha >= 0.0 {
            self.calib = Some(CalibAccum::new(n_layers, hidden, q_dim, inter));
            let mut seq = self.new_seq();
            let mut res = Ok(());
            for chunk in calib_tokens.chunks(MAX_PREFILL_CHUNK) {
                if let Err(e) = self.prefill_forward(&mut seq, chunk) {
                    res = Err(e);
                    break;
                }
            }
            self.release_seq(&mut seq);
            res?;
            let acc = self.calib.take().expect("calib accumulator set above");
            CalibStats {
                attn_in: acc.attn_in,
                attn_out: acc.attn_out,
                ffn_in: acc.ffn_in,
                down_in: acc.down_in,
                alpha,
            }
        } else {
            // Identity: smoothing_scale ignores the (unused) stats and returns 1.
            CalibStats {
                attn_in: vec![vec![0.0; hidden]; n_layers],
                attn_out: vec![vec![0.0; q_dim]; n_layers],
                ffn_in: vec![vec![0.0; hidden]; n_layers],
                down_in: vec![vec![0.0; inter]; n_layers],
                alpha,
            }
        };
        let layers = self
            .weights
            .rebuild_w4a8_smoothed(self.device.as_ref(), path, &stats)?;
        self.weights.w4a8 = Some(layers);
        Ok(())
    }

    /// Build the fp8 (e4m3) prefill packs from the resident GGUF weights. No
    /// calibration pass is needed (e4m3's exponent captures the per-row range),
    /// so this just dequantizes every dense projection and repacks it to e4m3
    /// with a per-row scale. Must be called once after load when
    /// `FORGE_GEMM=fp8`; before it runs `fp8` is `None` and prefill stays on the
    /// resident (Q4_K MMQ) path.
    pub fn build_fp8(&mut self, path: &Path) -> Result<()> {
        if self.is_hybrid() || self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "fp8 prefill supports dense (non-MoE, non-hybrid) models only".into(),
            ));
        }
        self.weights.fp8 = None;
        let layers = self.weights.rebuild_fp8(self.device.as_ref(), path)?;
        self.weights.fp8 = Some(layers);
        self.weights.fp8_modular = crate::weights::fp8_modular_enabled();
        Ok(())
    }

    fn pack_nvfp4_rows(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Result<Fp8Weight> {
        let DevWeight::NvFp4 {
            packed,
            scales,
            inv_global_scale,
            rows: source_rows,
            cols,
        } = weight
        else {
            return Err(ForgeError::Unsupported(
                "fp8mod-ffn wymaga rezydentnych wag NVFP4".into(),
            ));
        };
        let row_end = row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Format("przepełnienie zakresu wierszy FP8".into()))?;
        if row_end > *source_rows {
            return Err(ForgeError::Format(format!(
                "zakres wierszy FP8 {}..{} przekracza {source_rows}",
                row_offset, row_end
            )));
        }
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let output_scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        self.kernels.pack_nvfp4_fp8(
            &qweight,
            &output_scales,
            packed,
            scales,
            *cols,
            row_offset,
            rows,
            *inv_global_scale,
            &self.stream,
        )?;
        Ok(Fp8Weight {
            qweight,
            scales: output_scales,
            rows,
            cols: *cols,
        })
    }

    fn pack_f16_weight(&self, weight: &DevWeight) -> Result<Fp8Weight> {
        let DevWeight::F16 { buf, rows, cols } = weight else {
            return Err(ForgeError::Unsupported(
                "przepakowanie lm_head FP8 wymaga źródła F16".into(),
            ));
        };
        let weight_bytes = rows
            .checked_mul(*cols)
            .ok_or_else(|| ForgeError::OutOfMemory {
                requested: usize::MAX,
                available: self.device.pool_available(Pool::Weights).unwrap_or(0),
            })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| ForgeError::OutOfMemory {
            requested: usize::MAX,
            available: self.device.pool_available(Pool::Weights).unwrap_or(0),
        })?;
        let qweight = self
            .device
            .alloc(weight_bytes, MemKind::Device, Pool::Weights)?;
        let scales = self
            .device
            .alloc(scale_bytes, MemKind::Device, Pool::Weights)?;
        self.kernels
            .pack_f16_fp8(&qweight, &scales, buf, *cols, *rows, &self.stream)?;
        Ok(Fp8Weight {
            qweight,
            scales,
            rows: *rows,
            cols: *cols,
        })
    }

    fn fp8_pack_allocation_bytes(rows: usize, cols: usize) -> Option<usize> {
        let weight = rows
            .checked_mul(cols)?
            .max(1)
            .checked_next_multiple_of(256)?;
        let scales = rows.checked_mul(4)?.max(1).checked_next_multiple_of(256)?;
        weight.checked_add(scales)
    }

    fn preflight_nvfp4_pack(
        &self,
        weight: &DevWeight,
        row_offset: usize,
        rows: usize,
    ) -> Option<usize> {
        let DevWeight::NvFp4 {
            rows: source_rows,
            cols,
            ..
        } = weight
        else {
            return None;
        };
        let row_end = row_offset.checked_add(rows)?;
        if rows == 0
            || row_end > *source_rows
            || !cols.is_multiple_of(16)
            || !self.kernels.supports_fp8_modular_shape(rows, *cols)
        {
            return None;
        }
        Self::fp8_pack_allocation_bytes(rows, *cols)
    }

    /// Buduje na GPU opt-in paczki FP8 dla Q/O oraz projekcji FFN checkpointu NVFP4.
    pub fn build_fp8_ffn(&mut self) -> Result<bool> {
        if self.weights.fp8_ffn.is_some() {
            return Ok(true);
        }
        if !self.device.caps().fp8_native
            || !self.kernels.supports_fp8_hybrid_packers()
            || !self.kernels.supports_fp8_logits()
        {
            return Ok(false);
        }
        if self.is_hybrid() || self.weights.is_moe() {
            return Ok(false);
        }
        let mut required_bytes = 0usize;
        let mut add_required = |bytes: Option<usize>| -> Option<()> {
            required_bytes = required_bytes.checked_add(bytes?)?;
            Some(())
        };
        let params = &self.weights.descriptor.params;
        let Some(q_rows) = params.n_heads.checked_mul(params.head_dim) else {
            return Ok(false);
        };
        for layer in &self.weights.layers {
            let q_source = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => weight,
                QkvWeights::Split { q, .. } => q,
            };
            if add_required(self.preflight_nvfp4_pack(q_source, 0, q_rows)).is_none()
                || add_required(self.preflight_nvfp4_pack(
                    &layer.attn().attn_o,
                    0,
                    layer.attn().attn_o.rows(),
                ))
                .is_none()
            {
                return Ok(false);
            }
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                return Ok(false);
            };
            match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    if weight.rows() % 2 != 0 {
                        return Ok(false);
                    }
                    let rows = weight.rows() / 2;
                    if add_required(self.preflight_nvfp4_pack(weight, 0, rows)).is_none()
                        || add_required(self.preflight_nvfp4_pack(weight, rows, rows)).is_none()
                    {
                        return Ok(false);
                    }
                }
                GateUpWeights::Split { gate, up } => {
                    if add_required(self.preflight_nvfp4_pack(gate, 0, gate.rows())).is_none()
                        || add_required(self.preflight_nvfp4_pack(up, 0, up.rows())).is_none()
                    {
                        return Ok(false);
                    }
                }
            }
            if add_required(self.preflight_nvfp4_pack(&ffn.down, 0, ffn.down.rows())).is_none() {
                return Ok(false);
            }
        }
        let fp8_head_supported = match &self.weights.lm_head {
            DevWeight::F16 { rows, cols, .. } => {
                if !cols.is_multiple_of(256) {
                    false
                } else if add_required(Self::fp8_pack_allocation_bytes(*rows, *cols)).is_none() {
                    return Ok(false);
                } else {
                    true
                }
            }
            _ => false,
        };
        let Some(available) = self.device.pool_available(Pool::Weights) else {
            return Ok(false);
        };
        if required_bytes > available {
            return Ok(false);
        }

        self.device.synchronize()?;
        self.weights.fp8_lm_head = None;
        let mut layers = Vec::with_capacity(self.weights.layers.len());
        for layer in &self.weights.layers {
            let q = match &layer.attn().attn_qkv {
                QkvWeights::Fused(weight) | QkvWeights::FusedQk { qk: weight, .. } => {
                    self.pack_nvfp4_rows(weight, 0, q_rows)?
                }
                QkvWeights::Split { q, .. } => self.pack_nvfp4_rows(q, 0, q.rows())?,
            };
            let attn_o =
                self.pack_nvfp4_rows(&layer.attn().attn_o, 0, layer.attn().attn_o.rows())?;
            let LayerFfn::Dense(ffn) = &layer.ffn else {
                unreachable!("modele MoE zostały odrzucone przed przepakowaniem")
            };
            let (gate, up) = match &ffn.gate_up {
                GateUpWeights::Fused(weight) => {
                    let rows = weight.rows() / 2;
                    (
                        self.pack_nvfp4_rows(weight, 0, rows)?,
                        self.pack_nvfp4_rows(weight, rows, rows)?,
                    )
                }
                GateUpWeights::Split { gate, up } => (
                    self.pack_nvfp4_rows(gate, 0, gate.rows())?,
                    self.pack_nvfp4_rows(up, 0, up.rows())?,
                ),
            };
            let down = self.pack_nvfp4_rows(&ffn.down, 0, ffn.down.rows())?;
            layers.push(crate::weights::Fp8FfnLayer {
                q,
                attn_o,
                gate,
                up,
                down,
            });
        }
        let fp8_lm_head = match (&self.weights.lm_head, fp8_head_supported) {
            (DevWeight::F16 { .. }, true) => Some(self.pack_f16_weight(&self.weights.lm_head)?),
            _ => None,
        };
        self.stream.synchronize()?;
        self.weights.fp8_lm_head = fp8_lm_head;
        self.weights.fp8_ffn = Some(layers);
        self.weights.fp8_modular = crate::weights::fp8_ffn_modular_enabled();
        self.decode_graph = None;
        self.decode_moe_graph = None;
        self.decode_rot_graph = None;
        self.batch_graphs.clear();
        Ok(true)
    }

    /// Mean next-token negative log-likelihood over `tokens` (natural log),
    /// i.e. `ln(perplexity)`. Runs the forward pass through whatever prefill
    /// GEMM is active (W4A8 when calibrated, else Q4_K), then maps every
    /// position's final-norm hidden state through the lm_head and scores the
    /// actual next token with a numerically stable log-softmax. Used by the
    /// W4A8 quality gate to compare requant paths on a fixed held-out passage.
    pub fn perplexity(&mut self, tokens: &[u32]) -> Result<(f64, usize)> {
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "perplexity harness supports dense models only".into(),
            ));
        }
        if tokens.len() < 2 {
            return Err(ForgeError::Scheduler("perplexity needs >= 2 tokens".into()));
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let mut seq = self.new_seq();
        let mut nll_sum = 0.0f64;
        let mut count = 0usize;
        let mut result = Ok(());
        'outer: for (ci, chunk) in tokens.chunks(MAX_PREFILL_CHUNK).enumerate() {
            let base = ci * MAX_PREFILL_CHUNK;
            let t = match self.prefill_forward(&mut seq, chunk) {
                Ok(t) => t,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            for i in 0..t {
                let global = base + i;
                if global + 1 >= tokens.len() {
                    break 'outer;
                }
                let next = tokens[global + 1] as usize;
                let stream = &self.stream;
                let pb = self
                    .prefill_bufs
                    .as_ref()
                    .expect("prefill_forward allocated");
                if let Err(e) = self
                    .device
                    .copy(&pb.x, i * hidden * 2, &self.bufs.x, 0, hidden * 2, stream)
                    .and_then(|_| self.logits_gemv(&self.bufs.logits, &self.bufs.x, stream))
                    .and_then(|_| {
                        self.device.copy(
                            &self.bufs.logits,
                            0,
                            &self.bufs.pinned_logits,
                            0,
                            vocab * 4,
                            stream,
                        )
                    })
                    .and_then(|_| self.device.synchronize())
                {
                    result = Err(e);
                    break 'outer;
                }
                let lp =
                    self.bufs
                        .pinned_logits
                        .host_ptr()
                        .expect("pinned buffer has host mapping") as *const f32;
                let logits = unsafe { std::slice::from_raw_parts(lp, vocab) };
                // logsumexp for a stable NLL = logsumexp(logits) - logits[next].
                let maxv = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let mut sum = 0.0f64;
                for &v in logits {
                    sum += ((v - maxv) as f64).exp();
                }
                let logz = (maxv as f64) + sum.ln();
                nll_sum += logz - logits[next] as f64;
                count += 1;
            }
        }
        self.release_seq(&mut seq);
        result?;
        Ok((nll_sum / count.max(1) as f64, count))
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through the model in one
    /// batched pass, appending to `seq`, and return the last token's logits.
    /// Not graph-captured: T varies per call and prefill launches are large
    /// enough that launch overhead is immaterial.
    pub fn prefill_chunk(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        if self.is_hybrid() {
            return self.prefill_hybrid(seq, tokens);
        }
        let t = self.prefill_forward(seq, tokens)?;
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let stream = &self.stream;
        let pb = self
            .prefill_bufs
            .as_ref()
            .expect("prefill_forward allocated");
        // Only the last token's logits matter; route its hidden state through
        // the decode logits path (same GEMV + pinned landing).
        self.device.copy(
            &pb.x,
            (t - 1) * hidden * 2,
            &self.bufs.x,
            0,
            hidden * 2,
            stream,
        )?;
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, stream)?;
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            stream,
        )?;
        self.device.synchronize()?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
        Ok(logits)
    }

    /// Pooling declared by this model's metadata, falling back to `Mean` for
    /// models that declare none (the neutral choice for a generative model
    /// asked to produce an embedding).
    pub fn embedding_pooling(&self) -> PoolingType {
        match self.weights.descriptor.params.pooling_type {
            PoolingType::None => PoolingType::Mean,
            other => other,
        }
    }

    /// Encode `tokens` into a single sentence embedding. Runs the causal
    /// forward pass over the whole sequence (chunked at MAX_PREFILL_CHUNK,
    /// appending to a private KV sequence so later chunks attend to earlier
    /// ones), pools the final-norm hidden states with `pooling`, and — when
    /// `normalize` — L2-normalizes the result.
    ///
    /// The v0 arches (qwen3/llama/mistral) are decoder transformers, so the
    /// pass is causal: `Last` pooling reads the final token (which has
    /// attended to the whole sequence) and `Cls` the first. A bidirectional
    /// encoder arch would need the non-causal attention path (`attn_full`,
    /// already used by the Whisper encoder) wired in behind an arch flag; no
    /// such arch is in the registry yet, so that path is intentionally absent.
    pub fn embed(
        &mut self,
        tokens: &[u32],
        pooling: PoolingType,
        normalize: bool,
    ) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Err(ForgeError::Scheduler("empty embedding input".into()));
        }
        let hidden = self.weights.descriptor.params.hidden_size;
        let mut seq = self.new_seq();
        let out = self.embed_pooled(&mut seq, tokens, pooling, hidden);
        self.release_seq(&mut seq);
        let mut v = out?;
        if normalize {
            l2_normalize(&mut v);
        }
        Ok(v)
    }

    /// Forward + pool over a freshly grown sequence. Split out so `embed` can
    /// always release the sequence even on a mid-chunk error.
    fn embed_pooled(
        &mut self,
        seq: &mut SeqKv,
        tokens: &[u32],
        pooling: PoolingType,
        hidden: usize,
    ) -> Result<Vec<f32>> {
        let mut sum = vec![0f32; hidden];
        let mut last = vec![0f32; hidden];
        let mut cls: Option<Vec<f32>> = None;
        let mut total: usize = 0;
        let mut scratch = vec![0u8; MAX_PREFILL_CHUNK * hidden * 2];
        for chunk in tokens.chunks(MAX_PREFILL_CHUNK) {
            let t = self.prefill_forward(seq, chunk)?;
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("prefill_forward allocated");
            let bytes = &mut scratch[..t * hidden * 2];
            self.device.read(&pb.x, 0, bytes)?;
            let rows: &[f16] = bytemuck::cast_slice(bytes);
            if cls.is_none() {
                cls = Some(rows[..hidden].iter().map(|h| h.to_f32()).collect());
            }
            for ti in 0..t {
                let row = &rows[ti * hidden..(ti + 1) * hidden];
                match pooling {
                    PoolingType::Mean | PoolingType::None => {
                        for (s, h) in sum.iter_mut().zip(row) {
                            *s += h.to_f32();
                        }
                    }
                    PoolingType::Last => {
                        for (dst, h) in last.iter_mut().zip(row) {
                            *dst = h.to_f32();
                        }
                    }
                    PoolingType::Cls => {}
                }
            }
            total += t;
        }
        Ok(match pooling {
            PoolingType::Mean | PoolingType::None => {
                let inv = 1.0 / total as f32;
                sum.iter().map(|s| s * inv).collect()
            }
            PoolingType::Last => last,
            PoolingType::Cls => cls.expect("at least one chunk processed"),
        })
    }

    /// Run one token through the model, appending to `seq`, and return the
    /// f32 logits for the next-token distribution.
    pub fn step(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<Vec<f32>> {
        let vocab = self.weights.descriptor.params.vocab_size;
        self.step_launch(seq, token_id)?;
        // Land logits in pinned memory on the same stream, then one sync.
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            vocab * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();

        Ok(logits)
    }

    /// Enqueue one decode step (graph replay) on the model stream WITHOUT
    /// downloading logits or synchronizing. The next-token logits are left
    /// in the device logits buffer for either the pinned D2H (`step`) or the
    /// on-GPU sampler (`step_and_sample`).
    fn step_launch(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let pos = seq.len;

        if pos >= p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {pos} exceeds model context {}",
                p.max_position_embeddings
            )));
        }

        self.tier_ensure_capacity(seq, 1)?;
        if self.tier.is_some() {
            if !seq.spilled.is_empty() {
                if self.tier_can_restore(seq) {
                    // The whole sequence fits again with the watermark reserve
                    // intact: bring it back and take the graphed fast path.
                    self.tier_restore_or_recompute(seq)?;
                } else {
                    return self.step_streamed(seq, token_id);
                }
            }
            seq.tokens.push(token_id);
        }

        let page_boundary = seq.len.is_multiple_of(self.kv.cfg.page_size);
        if page_boundary {
            // A new page is about to be allocated; reclaim a cached prefix page
            // if the free stack is empty so decode growth never starves behind
            // the prefix cache (no-op when the cache is inactive/empty).
            self.ensure_free_pages(1);
        }
        self.kv.grow(seq)?;
        self.upload_decode_inputs(token_id, pos)?;

        // The page table changes when a page is appended — and goes stale when
        // another sequence used the single-stream path, or batched growth /
        // tier restores rewrote this sequence's pages.
        if page_boundary || self.pt_seq != seq.id {
            self.upload_page_table(seq)?;
        }

        // Hybrid attention/DeltaNet decode: per-token recurrent scan with a
        // resident SSM state, not graph-capturable (host readbacks per layer).
        if self.is_hybrid() {
            self.ensure_hybrid_bufs()?;
            if pos == 0 {
                self.zero_ssm()?;
            }
            return self.hybrid_forward_token(token_id, true, AttnSrc::Paged);
        }

        // Routed MoE decode: the device-side grouped expert dispatch keeps the
        // router selection on-device (no host readback), so a fully-gidx model
        // records into a replayable graph like the dense path. A model with a
        // fallback expert quant (e.g. Q8_0) still reads back per layer and runs
        // the explicit chain each step.
        if self.weights.is_moe() {
            if self.moe_fully_gidx() {
                if self.decode_moe_graph.is_none() {
                    let graph = self.capture_step_moe()?;
                    self.decode_moe_graph = Some(graph);
                }
                let graph = self
                    .decode_moe_graph
                    .as_ref()
                    .expect("captured above")
                    .clone();
                return self.device.launch_graph(&graph, &self.stream);
            }
            return self.run_step_moe();
        }

        // Rot decode commits the current token into the packed store + ring and
        // reads it back through the split-K attn_decode_rot. The pack kernel
        // takes the token position from `bufs.pos` (device-resident), so the
        // chain is position-independent and captured once like the f16 path.
        if self.kv.cfg.quant.is_rot() {
            if self.decode_rot_graph.is_none() {
                let graph = self.capture_decode_rot()?;
                self.decode_rot_graph = Some(graph);
            }
            let graph = self
                .decode_rot_graph
                .as_ref()
                .expect("captured above")
                .clone();
            return self.device.launch_graph(&graph, &self.stream);
        }

        if self.decode_graph.is_none() {
            let graph = self.capture_step()?;
            self.decode_graph = Some(graph);
        }
        let graph = self.decode_graph.as_ref().expect("captured above").clone();
        self.device.launch_graph(&graph, &self.stream)
    }

    /// Stage [token, pos, seq_len] in pinned memory and push them with async
    /// copies on the compute stream — pinned H2D avoids the pageable
    /// legacy-stream drain that plain write() must perform.
    fn upload_decode_inputs(&self, token_id: u32, pos: usize) -> Result<()> {
        let host = self
            .bufs
            .pinned_in
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            let vals = [token_id as i32, pos as i32, (pos + 1) as i32];
            std::ptr::copy_nonoverlapping(vals.as_ptr() as *const u8, host, 12);
        }
        self.device
            .copy(&self.bufs.pinned_in, 0, &self.bufs.ids, 0, 4, &self.stream)?;
        self.device
            .copy(&self.bufs.pinned_in, 4, &self.bufs.pos, 0, 4, &self.stream)?;
        self.device.copy(
            &self.bufs.pinned_in,
            8,
            &self.seq_len_dev,
            0,
            4,
            &self.stream,
        )?;
        Ok(())
    }

    /// Upload `seq`'s page table (pinned staging + async H2D) and mark it as
    /// the one resident in `page_table_dev`.
    fn upload_page_table(&mut self, seq: &SeqKv) -> Result<()> {
        let pt_host = self
            .bufs
            .pinned_pt
            .host_ptr()
            .expect("pinned buffer has host mapping");
        let mut pt = vec![-1i32; self.max_pages_per_seq];
        pt[..seq.pages.len()].copy_from_slice(&seq.pages);
        unsafe {
            std::ptr::copy_nonoverlapping(
                pt.as_ptr() as *const u8,
                pt_host,
                self.max_pages_per_seq * 4,
            );
        }
        self.device.copy(
            &self.bufs.pinned_pt,
            0,
            &self.page_table_dev,
            0,
            self.max_pages_per_seq * 4,
            &self.stream,
        )?;
        self.pt_seq = seq.id;
        Ok(())
    }

    /// One decode step for a sequence whose spilled KV cannot be restored into
    /// VRAM: the canonical paged slabs keep the resident tail while each
    /// layer's attention runs over the staging slabs holding the FULL context
    /// for that layer (spilled chunks streamed in from RAM/NVMe, resident
    /// pages copied D2D). Never graph-captured; the kernels and their order
    /// match the resident chains exactly, so greedy tokens are bit-identical
    /// to an untiered run.
    fn step_streamed(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<()> {
        let pos = seq.len;
        seq.tokens.push(token_id);
        self.kv.grow(seq)?;
        self.upload_decode_inputs(token_id, pos)?;
        self.tier
            .as_mut()
            .expect("streamed path requires tiering")
            .prepare_streaming(seq)?;
        // Hybrid decode: the per-token recurrent forward runs each attention
        // layer over the tier staging slabs (its full context), while the
        // resident DeltaNet state advances untouched. kv_append needs the
        // device page table for the resident tail write.
        if self.is_hybrid() {
            self.ensure_hybrid_bufs()?;
            self.upload_page_table(seq)?;
            return self.hybrid_forward_token(token_id, true, AttnSrc::Staged(seq));
        }
        if self.kv.cfg.quant.is_rot() {
            // kv_pack_rot commits the token into the canonical packed store
            // through the device page table (tail pages are resident), so the
            // per-layer staging picks it up like the separate f16 chain.
            self.upload_page_table(seq)?;
            self.run_step_rot(AttnSrc::Staged(seq))
        } else if Self::fused_decode_supported(&self.weights) {
            self.run_step_fused(AttnSrc::Staged(seq))
        } else {
            // The separate chain's qkv_post / kv_append write the new token
            // into the canonical paged slab through the device page table.
            self.upload_page_table(seq)?;
            self.run_step_separate(AttnSrc::Staged(seq))
        }
    }

    /// Capture the rotational decode step into a replayable graph. The recorded
    /// launches read all per-step inputs (token id, position, seq len, page
    /// table) from device buffers refreshed before each replay, so one capture
    /// serves every token.
    fn capture_decode_rot(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_rot(AttnSrc::Paged);
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step for the rotational KV modes. Mirrors the non-fused
    /// decode chain (explicit rmsnorm → qkv → norm/rope) but commits the
    /// appended token into the packed low-bit store + residual ring and reads
    /// it back through the split-K attn_decode_rot / attn_decode_combine_rot
    /// pair (rotate q once, score in rotated space, inverse-rotate the V
    /// accumulator). The pack kernel takes the position from `bufs.pos`, so the
    /// paged variant records cleanly into a CUDA graph. `src` selects the
    /// attention's store: the paged packed regions (captured) or the tier
    /// staging slabs carrying the sequence's full packed context per layer
    /// (streamed path, never captured; the residual ring is a global overlay
    /// and always reads in place).
    fn run_step_rot(&self, src: AttnSrc) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;
        let bits = self.kv.cfg.quant.bits().expect("rot mode has bits");

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        let ring_slots = self
            .kv
            .cfg
            .quant
            .ring_slots()
            .expect("rot mode has ring_slots");
        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            // Produce the rope'd q (attention query) plus the rope'd K/V as
            // LINEAR buffers so the pack kernel rotates them into the packed
            // store + residual ring. No paged f16 append (there is no f16 slab).
            // Returned tuple: (q_buf, q_off, k_src, k_off, v_src, v_off).
            let (q_buf, q_off, k_src, k_off, v_src, v_off): (
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
                &DevBuffer,
                usize,
            ) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemv(&b.qkv, w, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels
                            .rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, p.head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv,
                            k_byte_off,
                            kn,
                            p.n_kv_heads,
                            p.head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        p.n_kv_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemv(&b.qkv, qk, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels
                            .rmsnorm_f16_at(&b.qkv, 0, qn, p.n_heads, p.head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16_at(
                            &b.qkv,
                            k_byte_off,
                            kn,
                            p.n_kv_heads,
                            p.head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        0,
                        &b.pos,
                        1,
                        p.n_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    kernels.rope_neox_f16_at(
                        &b.qkv,
                        k_byte_off,
                        &b.pos,
                        1,
                        p.n_kv_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    (&b.qkv, 0, &b.qkv, k_byte_off, &b.v, 0)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                    if let Some(qn) = &layer.attn().q_norm {
                        kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                    }
                    if let Some(kn) = &layer.attn().k_norm {
                        kernels.rmsnorm_f16(
                            &b.k,
                            &b.k,
                            kn,
                            p.n_kv_heads,
                            p.head_dim,
                            eps,
                            stream,
                        )?;
                    }
                    kernels.rope_neox_f16(
                        &b.q,
                        &b.pos,
                        1,
                        p.n_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    kernels.rope_neox_f16(
                        &b.k,
                        &b.pos,
                        1,
                        p.n_kv_heads,
                        p.head_dim,
                        p.rope_theta,
                        stream,
                    )?;
                    (&b.q, 0, &b.k, 0, &b.v, 0)
                }
            };

            // Rotate+quant the token into the packed store + residual ring, then
            // attend over the dual region (ring for the recent window, packed
            // for older). q_buf's q head occupies head_dim*n_heads at q_off.
            kernels.kv_pack_rot(
                &self.kv.k_packed[l],
                &self.kv.v_packed[l],
                &self.kv.k_scale[l],
                &self.kv.v_scale[l],
                &self.kv.k[l],
                &self.kv.v[l],
                k_src,
                k_off,
                v_src,
                v_off,
                &self.page_table_dev,
                &self.bufs.pos,
                1,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                p.head_dim,
                ring_slots,
                bits,
                stream,
            )?;
            match &src {
                AttnSrc::Paged => {
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &self.kv.k_packed[l],
                        &self.kv.v_packed[l],
                        &self.kv.k_scale[l],
                        &self.kv.v_scale[l],
                        &self.kv.k[l],
                        &self.kv.v[l],
                        &self.page_table_dev,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
                AttnSrc::Staged(seq) => {
                    // The pack above landed this token in the canonical packed
                    // store's resident tail page; staging materializes the full
                    // packed history (spilled chunks + resident pages) for this
                    // layer behind the identity page table.
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let slot = &tb.slots[0];
                    tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                    kernels.attn_decode_rot(
                        &b.attn_parts,
                        q_buf,
                        q_off,
                        &slot.stage[0],
                        &slot.stage[1],
                        &slot.stage[2],
                        &slot.stage[3],
                        &self.kv.k[l],
                        &self.kv.v[l],
                        &tb.identity_pt,
                        &self.seq_len_dev,
                        1,
                        p.n_heads,
                        p.n_kv_heads,
                        p.head_dim,
                        self.kv.cfg.page_size,
                        self.max_pages_per_seq,
                        ATTN_DECODE_SPLITS,
                        ring_slots,
                        bits,
                        scale,
                        stream,
                    )?;
                }
            }
            kernels.attn_decode_combine_rot(
                &b.attn_out,
                &b.attn_parts,
                1,
                p.n_heads,
                p.head_dim,
                ATTN_DECODE_SPLITS,
                stream,
            )?;

            self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
            kernels.rmsnorm_residual_f16(
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv(&b.gate_up, w, &b.x, stream)?;
                    kernels.silu_mul_f16_at(&b.act, &b.gate_up, 0, inter * 2, inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv(&b.gate, gate, &b.x, stream)?;
                    self.gemv(&b.up, up, &b.x, stream)?;
                    kernels.silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
                }
            }
            self.gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;
        }

        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Whether requests with these sampling params can sample on the GPU:
    /// greedy always fits; a categorical draw needs a bounded top-k and a
    /// vocab within the kernel's merge capacity.
    pub fn gpu_sampling_supported(&self, params: &SamplingParams) -> bool {
        let vocab = self.weights.descriptor.params.vocab_size;
        GpuSampler::compatible(params)
            && (params.clone().sanitized().temperature <= 0.0
                || vocab <= forge_kernels::SAMPLE_MAX_VOCAB)
    }

    /// Run one token through the model and sample its successor on the GPU;
    /// only the 8-byte result crosses PCIe instead of the vocab-sized logits.
    pub fn step_and_sample(
        &mut self,
        seq: &mut SeqKv,
        token_id: u32,
        sampler: &mut GpuSampler,
    ) -> Result<u32> {
        self.step_launch(seq, token_id)?;
        self.sample_last_logits(sampler)
    }

    /// Sample from the logits currently resident in the device logits buffer
    /// (valid right after `step_launch`/`step`/`prefill_chunk` — before any
    /// other sequence runs). Launches ride the model stream, so this also
    /// works back-to-back with an un-synced `step_launch`.
    pub fn sample_last_logits(&mut self, sampler: &mut GpuSampler) -> Result<u32> {
        let p = &self.weights.descriptor.params;
        let b = &self.bufs;
        let sp = sampler.params().clone();

        let penalized = sampler.penalized();
        if sp.repetition_penalty != 1.0 && !penalized.is_empty() {
            if penalized.len() * 4 > b.pinned_penalty.len() {
                return Err(ForgeError::Scheduler(format!(
                    "penalty list {} exceeds staging capacity",
                    penalized.len()
                )));
            }
            let host = b
                .pinned_penalty
                .host_ptr()
                .expect("pinned buffer has host mapping");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    penalized.as_ptr() as *const u8,
                    host,
                    penalized.len() * 4,
                );
            }
            self.device.copy(
                &b.pinned_penalty,
                0,
                &b.penalty_ids,
                0,
                penalized.len() * 4,
                &self.stream,
            )?;
            self.kernels.sample_penalize_f32(
                &b.logits,
                &b.penalty_ids,
                penalized.len(),
                sp.repetition_penalty,
                &self.stream,
            )?;
        }

        if sp.temperature <= 0.0 {
            self.kernels.sample_argmax_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                &self.stream,
            )?;
        } else {
            let k = sp.top_k.min(p.vocab_size);
            self.kernels.sample_topk_f32(
                &b.sample_out,
                &b.sample_vals,
                &b.sample_idx,
                &b.logits,
                p.vocab_size,
                k,
                1.0 / sp.temperature,
                sp.top_p,
                sp.min_p,
                sampler.seed(),
                sampler.next_step(),
                &self.stream,
            )?;
        }

        self.device
            .copy(&b.sample_out, 0, &b.pinned_sample, 0, 8, &self.stream)?;
        self.device.synchronize()?;

        let sp_host = b
            .pinned_sample
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const i32;
        let id = unsafe { *sp_host };
        if id < 0 || id as usize >= p.vocab_size {
            return Err(ForgeError::Kernel(format!(
                "GPU sampler returned out-of-range token {id}"
            )));
        }
        Ok(id as u32)
    }

    /// Zwraca, czy checkpoint zawiera kompletny blok NextN gotowy do smoke MTP.
    pub fn has_native_mtp(&self) -> bool {
        self.weights
            .mtp
            .as_ref()
            .is_some_and(|mtp| mtp.runtime_supported())
            && self.mtp_state.is_some()
    }

    pub fn mtp_host_embedding_gathers(&self) -> u64 {
        self.mtp_state
            .as_ref()
            .map_or(0, MtpDraftState::host_embedding_gathers)
    }

    pub fn mtp_embedding_mode(&self) -> Option<&'static str> {
        self.weights
            .mtp
            .as_ref()
            .map(|weights| weights.embedding.mode())
    }

    fn mtp_propose_pending(&mut self, fed: u32, k: usize) -> Result<Vec<u32>> {
        if k != 2 && k != 3 {
            return Err(ForgeError::Unsupported(
                "MTP propose_k obsługuje wyłącznie K=2 lub K=3".into(),
            ));
        }
        if !self.has_native_mtp() {
            return Err(ForgeError::Unsupported(
                "checkpoint MTP nie spełnia ograniczeń natywnego runtime".into(),
            ));
        }
        self.ensure_hybrid_bufs()?;
        let mut state = self.mtp_state.take().ok_or_else(|| {
            ForgeError::Unsupported("stan wykonania MTP nie został zaalokowany".into())
        })?;
        let result: Result<()> = (|| {
            self.device.copy(
                &self.bufs.x,
                0,
                &state.recurrent_hidden,
                0,
                state.recurrent_hidden.len(),
                &self.stream,
            )?;
            state.checkpoint(&self.stream)?;
            self.device
                .write(&0i32.to_le_bytes(), &state.token_ids, 4 * 4)?;
            self.kernels.sample_argmax_f32(
                &self.bufs.sample_out,
                &self.bufs.sample_vals,
                &self.bufs.sample_idx,
                &self.bufs.logits,
                self.weights.descriptor.params.vocab_size,
                &self.stream,
            )?;
            self.device.copy(
                &self.bufs.sample_out,
                0,
                &state.token_ids,
                0,
                4,
                &self.stream,
            )?;

            for step in 0..=k {
                self.mtp_gather_embedding(&mut state, step)?;
                state.stage_step(&self.stream)?;
                self.mtp_forward_one(&mut state, step < k)?;
                state.save_step_hidden(step, &self.stream)?;
                if step == k {
                    continue;
                }
                self.kernels.sample_argmax_f32(
                    &self.bufs.sample_out,
                    &self.bufs.sample_vals,
                    &self.bufs.sample_idx,
                    &state.logits,
                    self.weights.descriptor.params.vocab_size,
                    &self.stream,
                )?;
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.token_ids,
                    (step + 1) * 4,
                    4,
                    &self.stream,
                )?;
            }
            self.device.copy(
                &state.token_ids,
                0,
                &state.pinned_token_ids,
                0,
                5 * 4,
                &self.stream,
            )?;
            Ok(())
        })();

        if result.is_err() && state.checkpoint_len().is_some() {
            let _ = state.rollback(&self.stream);
        }
        self.pt_seq = 0;
        self.mtp_state = Some(state);
        result?;
        self.device.synchronize()?;
        let state = self.mtp_state.as_ref().expect("stan przywrócony powyżej");
        let host = state
            .pinned_token_ids
            .host_ptr()
            .expect("pinned token IDs mają mapowanie hosta");
        let ids = unsafe { std::slice::from_raw_parts(host as *const i32, k + 1) };
        let gather_status = unsafe { *(host as *const i32).add(4) };
        let draft = if gather_status != 0 {
            Err(ForgeError::Kernel(
                "MTP GPU gather odrzucił token poza zakresem słownika".into(),
            ))
        } else if ids[0] as u32 != fed {
            Err(ForgeError::Scheduler(format!(
                "token wejściowy MTP {fed} nie odpowiada bieżącemu argmaxowi targetu {}",
                ids[0]
            )))
        } else {
            Ok(ids[1..].iter().map(|&id| id as u32).collect())
        };
        if draft.is_err() {
            self.rollback_mtp_pending()?;
        }
        draft
    }

    /// Buduje liniowy draft K=2/3 poza normalnym server flow i nie zmienia
    /// trwałego stanu KV/hidden bloku MTP.
    pub fn mtp_propose_k(&mut self, fed: u32, k: usize) -> Result<Vec<u32>> {
        let draft = self.mtp_propose_pending(fed, k)?;
        self.rollback_mtp_pending()?;
        Ok(draft)
    }

    fn rollback_mtp_pending(&mut self) -> Result<()> {
        let state = self.mtp_state.as_mut().ok_or_else(|| {
            ForgeError::Unsupported("stan wykonania MTP nie został zaalokowany".into())
        })?;
        state.rollback(&self.stream)?;
        self.device.synchronize()
    }

    fn mtp_catchup_token(&mut self, token: u32) -> Result<()> {
        if self.mtp_state.is_none() {
            return Ok(());
        }
        let mut state = self.mtp_state.take().expect("stan MTP sprawdzony powyżej");
        let result = (|| {
            self.device.copy(
                &self.bufs.x,
                0,
                &state.catchup_hidden,
                0,
                state.catchup_hidden.len(),
                &self.stream,
            )?;
            self.device
                .write(&(token as i32).to_le_bytes(), &self.bufs.sample_out, 0)?;
            self.mtp_gather_embedding(&mut state, 0)?;
            state.stage_step(&self.stream)?;
            self.mtp_forward_one(&mut state, false)?;
            self.device.copy(
                &state.catchup_hidden,
                0,
                &state.recurrent_hidden,
                0,
                state.recurrent_hidden.len(),
                &self.stream,
            )?;
            self.device.copy(
                &state.catchup_hidden,
                0,
                &self.bufs.x,
                0,
                state.catchup_hidden.len(),
                &self.stream,
            )
        })();
        self.mtp_state = Some(state);
        result
    }

    fn mtp_gather_embedding(&self, state: &mut MtpDraftState, token_index: usize) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        let p = &self.weights.descriptor.params;
        match &mtp.embedding {
            MtpEmbedding::Device(DevWeight::F16 { buf, .. }) => self.kernels.gather_f16_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::Q8_0 { buf, .. }) => self.kernels.gather_q8_0_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                &self.stream,
            ),
            MtpEmbedding::Device(DevWeight::NvFp4Gguf {
                buf, output_scale, ..
            }) => self.kernels.gather_nvfp4_gguf_row_f16(
                &mtp.token_embedding,
                buf,
                &self.bufs.sample_out,
                &state.token_ids,
                4 * 4,
                p.vocab_size,
                p.hidden_size,
                *output_scale,
                &self.stream,
            ),
            MtpEmbedding::Device(_) => Err(ForgeError::Unsupported(
                "GPU MTP wymaga embeddingu F16, Q8_0 lub GGUF NVFP4".into(),
            )),
            MtpEmbedding::HostF16 => {
                self.device.copy(
                    &self.bufs.sample_out,
                    0,
                    &state.pinned_token_ids,
                    token_index * 4,
                    4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let ids = state
                    .pinned_token_ids
                    .host_ptr()
                    .expect("pinned token IDs mają mapowanie hosta") as *const i32;
                let token_id = unsafe { *ids.add(token_index) };
                if token_id < 0 || token_id as usize >= p.vocab_size {
                    return Err(ForgeError::Kernel(format!(
                        "MTP argmax zwrócił token poza zakresem: {token_id}"
                    )));
                }
                let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                    ForgeError::Unsupported("brak hostowego embeddingu dla MTP low-memory".into())
                })?;
                let base = token_id as usize * p.hidden_size;
                let row = table.get(base..base + p.hidden_size).ok_or_else(|| {
                    ForgeError::Format("wiersz embeddingu MTP wykracza poza tabelę".into())
                })?;
                let staging = self
                    .hybrid_bufs
                    .as_ref()
                    .expect("bufory hybrid zaalokowane")
                    .pinned_embed
                    .host_ptr()
                    .expect("pinned embedding ma mapowanie hosta");
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        row.as_ptr() as *const u8,
                        staging,
                        p.hidden_size * 2,
                    );
                }
                self.device.copy(
                    &self
                        .hybrid_bufs
                        .as_ref()
                        .expect("bufory hybrid zaalokowane")
                        .pinned_embed,
                    0,
                    &mtp.token_embedding,
                    0,
                    p.hidden_size * 2,
                    &self.stream,
                )?;
                state.record_host_embedding_gather();
                Ok(())
            }
        }
    }

    fn mtp_forward_one(&self, state: &mut MtpDraftState, want_logits: bool) -> Result<()> {
        let mtp = self
            .weights
            .mtp
            .as_ref()
            .expect("sprawdzone przez propose_k");
        if mtp.layers.len() != 1 {
            return Err(ForgeError::Unsupported(format!(
                "runtime MTP obsługuje jeden blok, otrzymano {}",
                mtp.layers.len()
            )));
        }
        let layer = &mtp.layers[0];
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let head_dim = p.head_dim;
        let q_dim = p.n_heads * head_dim;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let b = &self.bufs;
        let hb = self
            .hybrid_bufs
            .as_ref()
            .expect("bufory hybrid zaalokowane");
        let stream = &self.stream;
        let DevWeight::Q8_0 { buf: eh_proj, .. } = &layer.eh_proj else {
            return Err(ForgeError::Unsupported(
                "mtp_prepare wymaga eh_proj w Q8_0".into(),
            ));
        };
        self.kernels.mtp_prepare_f16(
            &state.prepared_hidden,
            &mtp.token_embedding,
            &state.recurrent_hidden,
            &layer.enorm,
            &layer.hnorm,
            eh_proj,
            hidden,
            eps,
            stream,
        )?;
        self.kernels.rmsnorm_f16(
            &b.x,
            &state.prepared_hidden,
            &layer.block.attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let attention = layer.block.attn();
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        let qkv_grouped = self.gemv_nvfp4_gguf_group(
            &[(&hb.q_full, q), (&b.k, k), (&b.v, v)],
            &b.x,
            stream,
        )?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, q, &b.x, stream)?;
            self.gemv(&b.k, k, &b.x, stream)?;
            self.gemv(&b.v, v, &b.x, stream)?;
        }
        self.kernels
            .deinterleave_gate_f16(&hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream)?;
        if let Some(norm) = &attention.q_norm {
            self.kernels
                .rmsnorm_f16(&hb.qc, &hb.qc, norm, p.n_heads, head_dim, eps, stream)?;
        }
        if let Some(norm) = &attention.k_norm {
            self.kernels
                .rmsnorm_f16(&b.k, &b.k, norm, p.n_kv_heads, head_dim, eps, stream)?;
        }
        let n_rot = self.hybrid_n_rot();
        self.kernels.rope_neox_partial_f16(
            &hb.qc,
            &state.position,
            1,
            p.n_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.rope_neox_partial_f16(
            &b.k,
            &state.position,
            1,
            p.n_kv_heads,
            head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        self.kernels.kv_append_f16(
            &state.kv.k[0],
            &state.kv.v[0],
            &b.k,
            &b.v,
            &state.page_table,
            &state.seq_len,
            p.n_kv_heads,
            state.kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        self.kernels.attn_decode_f16(
            &b.attn_out,
            &hb.qc,
            &state.kv.k[0],
            &state.kv.v[0],
            &state.page_table,
            &state.seq_len,
            1,
            p.n_heads,
            p.n_kv_heads,
            head_dim,
            state.kv.cfg.page_size,
            state.kv.cfg.max_pages_per_seq,
            1.0 / (head_dim as f32).sqrt(),
            stream,
        )?;
        self.kernels
            .sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.gemv(&b.o_out, &attention.attn_o, &hb.gated, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.o_out,
            &layer.block.ffn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        let ffn = layer.block.dense_ffn()?;
        let GateUpWeights::Split { gate, up } = &ffn.gate_up else {
            return Err(ForgeError::Unsupported(
                "MTP wymaga rozdzielonych gate/up".into(),
            ));
        };
        self.gemv(&b.gate, gate, &b.x, stream)?;
        self.gemv(&b.up, up, &b.x, stream)?;
        self.kernels
            .silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
        self.gemv(&b.down, &ffn.down, &b.act, stream)?;
        self.kernels.rmsnorm_residual_f16(
            &b.x,
            &state.prepared_hidden,
            &b.down,
            &layer.shared_head_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.device.copy(
            &b.x,
            0,
            &state.recurrent_hidden,
            0,
            state.recurrent_hidden.len(),
            stream,
        )?;
        if want_logits {
            self.logits_weight_gemv(&state.logits, &b.x, &mtp.output, stream)?;
        }
        Ok(())
    }

    /// Sprawdza obsługę liniowej spekulacji przez gęsty model z cache F16.
    /// Weryfikacja dopisuje KV draftu i je wycofuje, dlatego wyklucza SSM,
    /// MoE, tiering i cache prefiksu. Głowa logitów musi być F16 lub Q8_0.
    pub fn validate_speculation_target(&self) -> Result<()> {
        if self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support hybrid SSM models".into(),
            ));
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support routed MoE models".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 KV cache".into(),
            ));
        }
        if self.tier.is_some() {
            return Err(ForgeError::Unsupported(
                "speculative verification does not support KV tiering".into(),
            ));
        }
        if self.prefix_cache.is_some() {
            return Err(ForgeError::Unsupported(
                "speculative verification requires the prefix cache to be disabled".into(),
            ));
        }
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. } | DevWeight::Q8_0 { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "speculative verification requires an F16 or Q8_0 language-model head".into(),
            ));
        }
        Ok(())
    }

    /// Zapewnia scratch logitów weryfikatora dla `cap` pozycji.
    fn ensure_verify_bufs(&mut self, cap: usize) -> Result<()> {
        if self.verify_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let vocab = self.weights.descriptor.params.vocab_size;
        self.verify_bufs = Some(VerifyBufs {
            cap,
            logits: self
                .device
                .alloc(cap * vocab * 4, MemKind::Device, Pool::Activations)?,
            ids: self
                .device
                .alloc(cap * 4, MemKind::Device, Pool::Activations)?,
            pinned_ids: self
                .device
                .alloc(cap * 4, MemKind::PinnedHost, Pool::Activations)?,
        });
        Ok(())
    }

    fn ensure_hybrid_verify_bufs(&mut self, cap: usize) -> Result<()> {
        if self
            .hybrid_verify_bufs
            .as_ref()
            .is_some_and(|bufs| bufs.cap >= cap)
        {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p
            .ssm
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("target MTP nie jest hybrydowy".into()))?;
        let q_dim = p.n_heads * p.head_dim;
        let conv_dim = ssm.conv_dim();
        let key_dim = ssm.key_dim();
        let value_dim = ssm.value_dim();
        let n_v = ssm.n_v_heads();
        let conv_elems = conv_dim.checked_mul(ssm.d_conv - 1).ok_or_else(|| {
            ForgeError::Scheduler("przepełnienie okna conv verifiera MTP".into())
        })?;
        let state_elems = n_v
            .checked_mul(ssm.d_state)
            .and_then(|value| value.checked_mul(ssm.d_state))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie stanu verifiera MTP".into())
            })?;
        let device = self.device.clone();
        let a16 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 2, MemKind::Device)
        };
        let a32 = |name: &str, dims: &[usize]| {
            alloc_checked(device.as_ref(), name, dims, 4, MemKind::Device)
        };
        let delta_count = self
            .weights
            .layers
            .iter()
            .filter(|layer| matches!(layer.mixer, LayerMixer::DeltaNet(_)))
            .count();
        let q_full = a16("mtp q full", &[cap, q_dim, 2])?;
        let qc = a16("mtp q", &[cap, q_dim])?;
        let gatec = a16("mtp q gate", &[cap, q_dim])?;
        let gated = a16("mtp gated attention", &[cap, q_dim])?;
        let qkv_mixed = a16("mtp mixed qkv", &[cap, conv_dim])?;
        let z = a16("mtp z", &[cap, value_dim])?;
        let conv_out = a16("mtp conv output", &[cap, conv_dim])?;
        let q16 = a16("mtp q16", &[cap, key_dim])?;
        let k16 = a16("mtp k16", &[cap, key_dim])?;
        let q32 = a16("mtp q32", &[cap, value_dim])?;
        let k32 = a16("mtp k32", &[cap, value_dim])?;
        let vtok = a16("mtp v", &[cap, value_dim])?;
        let alpha = a16("mtp alpha", &[cap, n_v])?;
        let beta_raw = a16("mtp beta raw", &[cap, n_v])?;
        let g = a32("mtp g", &[cap, n_v])?;
        let beta_f = a32("mtp beta", &[cap, n_v])?;
        let o = a16("mtp recurrence output", &[cap, value_dim])?;
        let normed = a16("mtp recurrence norm", &[cap, value_dim])?;
        let state_checkpoints = a32("mtp state checkpoints", &[cap, state_elems])?;
        let accepted = a32("mtp accepted", &[1])?;
        let pinned_embed = alloc_checked(
            device.as_ref(),
            "mtp pinned embedding",
            &[cap, p.hidden_size],
            2,
            MemKind::PinnedHost,
        )?;
        let delta_base = self
            .weights
            .layers
            .iter()
            .map(|layer| match &layer.mixer {
                LayerMixer::DeltaNet(_) => Ok(Some((
                    a16("mtp conv initial", &[conv_elems])?,
                    a16("mtp conv checkpoints", &[cap, conv_elems])?,
                    a32("mtp state initial", &[state_elems])?,
                ))),
                LayerMixer::Attention(_) => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        let checkpoint_stride = cap
            .checked_mul(state_elems)
            .and_then(|elements| elements.checked_mul(4))
            .ok_or_else(|| {
                ForgeError::Scheduler("przepełnienie offsetu checkpointów MTP".into())
            })?;
        let retained_state_checkpoints = match a32(
            "mtp retained state checkpoints",
            &[delta_count, cap, state_elems],
        ) {
            Ok(buffer) => Some(buffer),
            Err(ForgeError::OutOfMemory { .. }) => None,
            Err(error) => return Err(error),
        };
        let retain_checkpoints = retained_state_checkpoints.is_some();
        let mut delta_index = 0usize;
        let delta = delta_base
            .into_iter()
            .map(|base| match base {
                Some((conv_initial, conv_checkpoints, state_initial)) => {
                    let checkpoint_byte_offset = delta_index
                        .checked_mul(checkpoint_stride)
                        .ok_or_else(|| {
                            ForgeError::Scheduler(
                                "przepełnienie offsetu warstwy DeltaNet MTP".into(),
                            )
                        })?;
                    delta_index += 1;
                    let commit = if retain_checkpoints {
                        DeltaVerifyCommit::Retained {
                            checkpoint_byte_offset,
                        }
                    } else {
                        DeltaVerifyCommit::Recompute {
                            q: a16("mtp delta q", &[cap, value_dim])?,
                            k: a16("mtp delta k", &[cap, value_dim])?,
                            v: a16("mtp delta v", &[cap, value_dim])?,
                            g: a32("mtp delta g", &[cap, n_v])?,
                            beta: a32("mtp delta beta", &[cap, n_v])?,
                        }
                    };
                    Ok(Some(DeltaVerifyCache {
                        commit,
                        conv_initial,
                        conv_checkpoints,
                        state_initial,
                    }))
                }
                None => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        self.hybrid_verify_bufs = Some(HybridVerifyBufs {
            cap,
            q_full,
            qc,
            gatec,
            gated,
            qkv_mixed,
            z,
            conv_out,
            q16,
            k16,
            q32,
            k32,
            vtok,
            alpha,
            beta_raw,
            g,
            beta_f,
            o,
            normed,
            state_checkpoints,
            retained_state_checkpoints,
            accepted,
            pinned_embed,
            delta,
        });
        Ok(())
    }

    pub fn validate_native_mtp_target(&self) -> Result<()> {
        if !self.is_hybrid() {
            return Err(ForgeError::Unsupported(
                "natywny verifier MTP wymaga hybrydowego targetu".into(),
            ));
        }
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "natywny verifier MTP nie obsługuje jeszcze targetu MoE".into(),
            ));
        }
        if !matches!(self.kv.cfg.quant, KvQuant::F16) {
            return Err(ForgeError::Unsupported(
                "natywny verifier MTP wymaga cache KV F16".into(),
            ));
        }
        if self.tier.is_some() || self.prefix_cache.is_some() {
            return Err(ForgeError::Unsupported(
                "natywny verifier MTP wymaga wyłączonego tieringu i prefix cache".into(),
            ));
        }
        if !matches!(
            self.weights.lm_head,
            DevWeight::F16 { .. } | DevWeight::Q8_0 { .. }
        ) {
            return Err(ForgeError::Unsupported(
                "batchowy head targetu MTP wymaga F16 lub Q8_0".into(),
            ));
        }
        Ok(())
    }

    /// Zwraca dostępny budget 0/2/3 po sprawdzeniu targetu, kontekstu i stron
    /// dla `fed` oraz draftu. Żądane K=3 może zostać przycięte do K=2.
    pub fn native_mtp_available_budget(&self, seq: &SeqKv, requested: usize) -> usize {
        if self.validate_native_mtp_target().is_err() || requested < 2 {
            return 0;
        }
        for budget in (2..=requested.min(3)).rev() {
            let Some(end) = seq
                .len
                .checked_add(1)
                .and_then(|length| length.checked_add(budget))
            else {
                continue;
            };
            if end > self.weights.descriptor.params.max_position_embeddings {
                continue;
            }
            let required_pages = end
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len());
            if required_pages <= self.available_pages() {
                return budget;
            }
        }
        0
    }

    fn hybrid_verify_delta_layer(
        &self,
        layer_index: usize,
        delta: &DeltaNetWeights,
        t: usize,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
        let conv_dim = ssm.conv_dim();
        let key_dim = ssm.key_dim();
        let value_dim = ssm.value_dim();
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let d_state = ssm.d_state;
        let conv_elems = conv_dim * (ssm.d_conv - 1);
        let rep = n_v / n_k;
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self.prefill_bufs.as_ref().expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let cache = hv.delta[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma cache verifiera");
        let state = self.ssm[layer_index]
            .as_ref()
            .expect("warstwa DeltaNet ma stan");

        self.gemm(&hv.qkv_mixed, &delta.in_proj, &pb.x, t, stream)?;
        self.gemm(&hv.z, &delta.gate_proj, &pb.x, t, stream)?;
        self.gemm(&hv.alpha, &delta.alpha_proj, &pb.x, t, stream)?;
        self.gemm(&hv.beta_raw, &delta.beta_proj, &pb.x, t, stream)?;

        self.device.copy(
            &state.conv,
            0,
            &cache.conv_initial,
            0,
            conv_elems * 2,
            stream,
        )?;
        let conv_result: Result<()> = (|| {
            for token in 0..t {
                kernels.deltanet_conv_silu_f16_at(
                    &hv.conv_out,
                    token * conv_dim * 2,
                    &state.conv,
                    &hv.qkv_mixed,
                    token * conv_dim * 2,
                    &delta.conv1d,
                    conv_dim,
                    ssm.d_conv,
                    stream,
                )?;
                self.device.copy(
                    &state.conv,
                    0,
                    &cache.conv_checkpoints,
                    token * conv_elems * 2,
                    conv_elems * 2,
                    stream,
                )?;
            }
            Ok(())
        })();
        self.device.copy(
            &cache.conv_initial,
            0,
            &state.conv,
            0,
            conv_elems * 2,
            stream,
        )?;
        conv_result?;

        for token in 0..t {
            let conv_base = token * conv_dim * 2;
            self.device.copy(
                &hv.conv_out,
                conv_base,
                &hv.q16,
                token * key_dim * 2,
                key_dim * 2,
                stream,
            )?;
            self.device.copy(
                &hv.conv_out,
                conv_base + key_dim * 2,
                &hv.k16,
                token * key_dim * 2,
                key_dim * 2,
                stream,
            )?;
            self.device.copy(
                &hv.conv_out,
                conv_base + 2 * key_dim * 2,
                &hv.vtok,
                token * value_dim * 2,
                value_dim * 2,
                stream,
            )?;
        }
        kernels.l2norm_heads_f16(&hv.q16, &hv.q16, t * n_k, d_state, p.rms_norm_eps, stream)?;
        kernels.l2norm_heads_f16(&hv.k16, &hv.k16, t * n_k, d_state, p.rms_norm_eps, stream)?;
        let key_bytes = n_k * d_state * 2;
        for token in 0..t {
            for repeat in 0..rep {
                let source = token * key_bytes;
                let target = (token * n_v + repeat * n_k) * d_state * 2;
                self.device
                    .copy(&hv.q16, source, &hv.q32, target, key_bytes, stream)?;
                self.device
                    .copy(&hv.k16, source, &hv.k32, target, key_bytes, stream)?;
            }
            kernels.deltanet_log_decay_f32_at(
                &hv.g,
                token * n_v * 4,
                &hv.alpha,
                token * n_v * 2,
                &delta.dt_bias,
                &delta.a,
                n_v,
                stream,
            )?;
        }
        kernels.deltanet_beta_sigmoid_f32(&hv.beta_f, &hv.beta_raw, t * n_v, stream)?;
        let (state_checkpoints, checkpoint_byte_offset) = match &cache.commit {
            DeltaVerifyCommit::Retained {
                checkpoint_byte_offset,
            } => (
                hv.retained_state_checkpoints
                    .as_ref()
                    .expect("retained checkpointy DeltaNet są zaalokowane"),
                *checkpoint_byte_offset,
            ),
            DeltaVerifyCommit::Recompute { .. } => (&hv.state_checkpoints, 0),
        };
        kernels.deltanet_gated_scan_f16_at(
            &hv.o,
            state_checkpoints,
            checkpoint_byte_offset,
            &state.state,
            &hv.q32,
            &hv.k32,
            &hv.vtok,
            &hv.g,
            &hv.beta_f,
            t,
            n_v,
            d_state,
            stream,
        )?;
        if let DeltaVerifyCommit::Recompute { q, k, v, g, beta } = &cache.commit {
            self.device
                .copy(&hv.q32, 0, q, 0, t * value_dim * 2, stream)?;
            self.device
                .copy(&hv.k32, 0, k, 0, t * value_dim * 2, stream)?;
            self.device
                .copy(&hv.vtok, 0, v, 0, t * value_dim * 2, stream)?;
            self.device.copy(&hv.g, 0, g, 0, t * n_v * 4, stream)?;
            self.device
                .copy(&hv.beta_f, 0, beta, 0, t * n_v * 4, stream)?;
        }
        kernels.deltanet_gated_rmsnorm_f16(
            &hv.normed,
            &hv.o,
            &hv.z,
            &delta.ssm_norm,
            t * n_v,
            d_state,
            p.rms_norm_eps,
            stream,
        )?;
        self.gemm(&pb.o_out, &delta.out_proj, &hv.normed, t, stream)
    }

    fn hybrid_verify_attention_layer(
        &self,
        layer_index: usize,
        attention: &AttnWeights,
        base_pos: usize,
        t: usize,
    ) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let stream = &self.stream;
        let kernels = &self.kernels;
        let pb = self.prefill_bufs.as_ref().expect("bufory prefill są gotowe");
        let hv = self
            .hybrid_verify_bufs
            .as_ref()
            .expect("bufory hybrid verify są gotowe");
        let QkvWeights::Split { q, k, v } = &attention.attn_qkv else {
            return Err(ForgeError::Unsupported(
                "hybrydowy verifier MTP wymaga rozdzielonych Q/K/V".into(),
            ));
        };
        self.gemm(&hv.q_full, q, &pb.x, t, stream)?;
        kernels.deinterleave_gate_f16(
            &hv.qc,
            &hv.gatec,
            &hv.q_full,
            p.head_dim,
            t * q_dim,
            stream,
        )?;
        if let Some(norm) = &attention.q_norm {
            kernels.rmsnorm_f16(
                &hv.qc,
                &hv.qc,
                norm,
                t * p.n_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        self.gemm(&pb.k, k, &pb.x, t, stream)?;
        self.gemm(&pb.v, v, &pb.x, t, stream)?;
        if let Some(norm) = &attention.k_norm {
            kernels.rmsnorm_f16(
                &pb.k,
                &pb.k,
                norm,
                t * p.n_kv_heads,
                p.head_dim,
                p.rms_norm_eps,
                stream,
            )?;
        }
        let n_rot = self.hybrid_n_rot();
        kernels.rope_neox_partial_f16(
            &hv.qc,
            &pb.positions,
            t,
            p.n_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.rope_neox_partial_f16(
            &pb.k,
            &pb.positions,
            t,
            p.n_kv_heads,
            p.head_dim,
            n_rot,
            p.rope_theta,
            stream,
        )?;
        kernels.kv_append_batch(
            &self.kv.k[layer_index],
            &self.kv.v[layer_index],
            &pb.k,
            &pb.v,
            &self.page_table_dev,
            base_pos,
            t,
            p.n_kv_heads,
            self.kv.cfg.page_size,
            p.head_dim,
            self.kv.cfg.dtype(),
            stream,
        )?;
        kernels.attn_prefill(
            &pb.attn_out,
            &hv.qc,
            &self.kv.k[layer_index],
            &self.kv.v[layer_index],
            &self.page_table_dev,
            base_pos,
            t,
            p.n_heads,
            p.n_kv_heads,
            p.head_dim,
            self.kv.cfg.page_size,
            self.kv.cfg.dtype(),
            1.0 / (p.head_dim as f32).sqrt(),
            stream,
        )?;
        kernels.sigmoid_mul_f16(&hv.gated, &pb.attn_out, &hv.gatec, t * q_dim, stream)?;
        debug_assert_eq!(attention.attn_o.cols(), q_dim);
        debug_assert!(pb.k.len() >= t * kv_dim * 2);
        self.gemm(&pb.o_out, &attention.attn_o, &hv.gated, t, stream)
    }

    fn verify_native_mtp_hybrid(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        let t = draft.len() + 1;
        self.validate_native_mtp_target()?;
        self.ensure_prefill_bufs()?;
        self.ensure_verify_bufs(t)?;
        self.ensure_hybrid_verify_bufs(t)?;
        let p = self.weights.descriptor.params.clone();
        let base = seq.len;
        if base + t > p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {} exceeds model context {}",
                base + t - 1,
                p.max_position_embeddings
            )));
        }
        self.ensure_free_pages(
            (base + t)
                .div_ceil(self.kv.cfg.page_size)
                .saturating_sub(seq.pages.len()),
        );
        let mut snapshot_ready = false;
        let result = (|| {
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            for (layer_index, cache) in hv.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                let state = self.ssm[layer_index]
                    .as_ref()
                    .expect("warstwa DeltaNet ma stan");
                self.device.copy(
                    &state.state,
                    0,
                    &cache.state_initial,
                    0,
                    state.state.len(),
                    &self.stream,
                )?;
                self.device.copy(
                    &state.conv,
                    0,
                    &cache.conv_initial,
                    0,
                    state.conv.len(),
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            snapshot_ready = true;
            for _ in 0..t {
                self.kv.grow(seq)?;
            }
            let mut page_table = vec![-1i32; self.max_pages_per_seq];
            page_table[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.device
                .write(bytemuck::cast_slice(&page_table), &self.page_table_dev, 0)?;
            self.pt_seq = seq.id;
            let pb = self.prefill_bufs.as_ref().expect("bufory prefill są gotowe");
            let hv = self
                .hybrid_verify_bufs
                .as_ref()
                .expect("bufory hybrid verify są gotowe");
            let tokens: Vec<u32> = std::iter::once(fed).chain(draft.iter().copied()).collect();
            let ids: Vec<i32> = tokens.iter().map(|&id| id as i32).collect();
            let positions: Vec<i32> = (base..base + t).map(|pos| pos as i32).collect();
            self.device.write(bytemuck::cast_slice(&ids), &pb.ids, 0)?;
            self.device
                .write(bytemuck::cast_slice(&positions), &pb.positions, 0)?;
            let table = self.weights.token_embd_host.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("hybrydowy target nie ma hostowego embeddingu".into())
            })?;
            let staging = hv
                .pinned_embed
                .host_ptr()
                .expect("pinned embedding ma mapowanie hosta");
            for (row_index, &token) in tokens.iter().enumerate() {
                let source = table
                    .get(token as usize * p.hidden_size..(token as usize + 1) * p.hidden_size)
                    .ok_or_else(|| {
                        ForgeError::Scheduler(format!(
                            "token id {token} wykracza poza embedding targetu"
                        ))
                    })?;
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        source.as_ptr() as *const u8,
                        staging.add(row_index * p.hidden_size * 2),
                        p.hidden_size * 2,
                    );
                }
            }
            self.device.copy(
                &hv.pinned_embed,
                0,
                &pb.h,
                0,
                t * p.hidden_size * 2,
                &self.stream,
            )?;
            self.kernels.rmsnorm_f16(
                &pb.x,
                &pb.h,
                &self.weights.layers[0].attn_norm,
                t,
                p.hidden_size,
                p.rms_norm_eps,
                &self.stream,
            )?;

            for (layer_index, layer) in self.weights.layers.iter().enumerate() {
                match &layer.mixer {
                    LayerMixer::Attention(attention) => {
                        self.hybrid_verify_attention_layer(layer_index, attention, base, t)?
                    }
                    LayerMixer::DeltaNet(delta) => {
                        self.hybrid_verify_delta_layer(layer_index, delta, t)?
                    }
                }
                self.kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.o_out,
                    &layer.ffn_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
                let LayerFfn::Dense(ffn) = &layer.ffn else {
                    return Err(ForgeError::Unsupported(
                        "hybrydowy verifier MTP nie obsługuje jeszcze targetu MoE".into(),
                    ));
                };
                match &ffn.gate_up {
                    GateUpWeights::Fused(weight) => {
                        self.gemm_rows(&pb.gate, weight, &pb.x, t, 0, p.intermediate_size, &self.stream)?;
                        self.gemm_rows(
                            &pb.up,
                            weight,
                            &pb.x,
                            t,
                            p.intermediate_size,
                            p.intermediate_size,
                            &self.stream,
                        )?;
                    }
                    GateUpWeights::Split { gate, up } => {
                        self.gemm(&pb.gate, gate, &pb.x, t, &self.stream)?;
                        self.gemm(&pb.up, up, &pb.x, t, &self.stream)?;
                    }
                }
                self.kernels.silu_mul_f16(
                    &pb.act,
                    &pb.gate,
                    &pb.up,
                    t * p.intermediate_size,
                    &self.stream,
                )?;
                self.gemm(&pb.down, &ffn.down, &pb.act, t, &self.stream)?;
                let next_norm = if layer_index + 1 < self.weights.layers.len() {
                    &self.weights.layers[layer_index + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                self.kernels.rmsnorm_residual_f16(
                    &pb.x,
                    &pb.h,
                    &pb.down,
                    next_norm,
                    t,
                    p.hidden_size,
                    p.rms_norm_eps,
                    &self.stream,
                )?;
            }

            let vb = self.verify_bufs.as_ref().expect("bufory verify są gotowe");
            self.logits_gemm(&vb.logits, &pb.x, t, &self.stream)?;
            self.kernels.sample_batched_argmax_f32(
                &vb.ids,
                &vb.logits,
                t,
                p.vocab_size,
                &self.stream,
            )?;
            self.device
                .copy(&vb.ids, 0, &vb.pinned_ids, 0, t * 4, &self.stream)?;
            self.device.synchronize()?;
            let ids = unsafe {
                std::slice::from_raw_parts(
                    vb.pinned_ids
                        .host_ptr()
                        .expect("pinned IDs mają mapowanie hosta") as *const i32,
                    t,
                )
            };
            if ids
                .iter()
                .any(|&token| token < 0 || token as usize >= p.vocab_size)
            {
                return Err(ForgeError::Kernel(
                    "batchowy argmax MTP zwrócił token poza słownikiem".into(),
                ));
            }
            let (accepted, correction) = native_mtp_greedy_decision(draft, ids);
            let retained = accepted + 1;
            self.device
                .write(&(retained as i32).to_le_bytes(), &hv.accepted, 0)?;
            self.device.copy(
                &pb.h,
                accepted * p.hidden_size * 2,
                &self.bufs.h,
                0,
                p.hidden_size * 2,
                &self.stream,
            )?;
            self.device.copy(
                &pb.x,
                accepted * p.hidden_size * 2,
                &self.bufs.x,
                0,
                p.hidden_size * 2,
                &self.stream,
            )?;
            self.device.copy(
                &vb.logits,
                accepted * p.vocab_size * 4,
                &self.bufs.logits,
                0,
                p.vocab_size * 4,
                &self.stream,
            )?;

            let ssm = p.ssm.as_ref().expect("hybrydowy target ma parametry SSM");
            let conv_bytes = ssm.conv_dim() * (ssm.d_conv - 1) * 2;
            for (layer_index, cache) in hv.delta.iter().enumerate() {
                let Some(cache) = cache else { continue };
                let state = self.ssm[layer_index]
                    .as_ref()
                    .expect("warstwa DeltaNet ma stan");
                match &cache.commit {
                    DeltaVerifyCommit::Retained {
                        checkpoint_byte_offset,
                    } => self.kernels.deltanet_commit_checkpoint_f32_at(
                        &state.state,
                        hv.retained_state_checkpoints
                            .as_ref()
                            .expect("retained checkpointy DeltaNet są zaalokowane"),
                        *checkpoint_byte_offset,
                        &hv.accepted,
                        t,
                        ssm.n_v_heads(),
                        ssm.d_state,
                        &self.stream,
                    )?,
                    DeltaVerifyCommit::Recompute { q, k, v, g, beta } => {
                        self.kernels.deltanet_gated_scan_f16(
                            &hv.o,
                            &hv.state_checkpoints,
                            &state.state,
                            q,
                            k,
                            v,
                            g,
                            beta,
                            t,
                            ssm.n_v_heads(),
                            ssm.d_state,
                            &self.stream,
                        )?;
                        self.kernels.deltanet_commit_checkpoint_f32(
                            &state.state,
                            &hv.state_checkpoints,
                            &hv.accepted,
                            t,
                            ssm.n_v_heads(),
                            ssm.d_state,
                            &self.stream,
                        )?;
                    }
                }
                self.device.copy(
                    &cache.conv_checkpoints,
                    accepted * conv_bytes,
                    &state.conv,
                    0,
                    conv_bytes,
                    &self.stream,
                )?;
            }
            self.device.synchronize()?;
            Ok((accepted, correction))
        })();
        match result {
            Ok((accepted, correction)) => {
                self.kv.rollback(seq, base + accepted + 1);
                self.pt_seq = 0;
                Ok((accepted, correction))
            }
            Err(error) => {
                self.kv.rollback(seq, base);
                self.pt_seq = 0;
                if snapshot_ready {
                    let restore = (|| {
                        let hv = self
                            .hybrid_verify_bufs
                            .as_ref()
                            .expect("bufory hybrid verify są gotowe");
                        for (layer_index, cache) in hv.delta.iter().enumerate() {
                            let Some(cache) = cache else { continue };
                            let state = self.ssm[layer_index]
                                .as_ref()
                                .expect("warstwa DeltaNet ma stan");
                            self.device.copy(
                                &cache.state_initial,
                                0,
                                &state.state,
                                0,
                                state.state.len(),
                                &self.stream,
                            )?;
                            self.device.copy(
                                &cache.conv_initial,
                                0,
                                &state.conv,
                                0,
                                state.conv.len(),
                                &self.stream,
                            )?;
                        }
                        self.device.synchronize()
                    })();
                    if let Err(restore_error) = restore {
                        return Err(ForgeError::Scheduler(format!(
                            "błąd verifiera MTP: {error}; błąd odtworzenia SSM: {restore_error}"
                        )));
                    }
                }
                Err(error)
            }
        }
    }

    /// Wykonuje jeden cykl natywnego MTP. Bieżące logity targetu przewidziały
    /// już `fed`, więc draft powstaje bez osobnego kroku targetu, a target
    /// zatwierdza `[fed, draft...]` przebiegiem T=3/4. Zwracany correction
    /// pozostaje tokenem do podania w następnym cyklu i nie jest jeszcze
    /// zapisany w stanie targetu.
    pub fn native_mtp_step(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        budget: usize,
    ) -> Result<(Vec<u32>, usize, u32)> {
        if budget != 2 && budget != 3 {
            return Err(ForgeError::Unsupported(
                "natywny MTP obsługuje budget 2 lub 3".into(),
            ));
        }
        self.validate_native_mtp_target()?;
        if self.native_mtp_available_budget(seq, budget) != budget {
            return Err(ForgeError::Scheduler(format!(
                "brak pojemności targetu dla MTP K={budget}"
            )));
        }
        let draft = self.mtp_propose_pending(fed, budget)?;
        match self.verify_native_mtp_hybrid(seq, fed, &draft) {
            Ok((accepted, correction)) => {
                let state = self.mtp_state.as_mut().expect("stan MTP istnieje");
                state.commit_prefix(accepted + 1, &self.stream)?;
                self.device.copy(
                    &self.bufs.x,
                    0,
                    &state.recurrent_hidden,
                    0,
                    state.recurrent_hidden.len(),
                    &self.stream,
                )?;
                self.device.synchronize()?;
                Ok((draft, accepted, correction))
            }
            Err(error) => {
                if let Err(rollback_error) = self.rollback_mtp_pending() {
                    return Err(ForgeError::Scheduler(format!(
                        "błąd verifiera MTP: {error}; błąd rollbacku draftu: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// Verify one greedy speculative draft in a single forward (SPEC §6, linear
    /// path). Runs the model over `[fed, draft…]` as a mini-prefill chunk
    /// appended after the current position, greedy-argmaxes the logits at every
    /// query position, and accepts the longest draft prefix whose token equals
    /// the model's own argmax at the preceding position. The rejected draft
    /// positions' K/V are rolled back, leaving `fed` + the accepted drafts
    /// resident. Returns `(accepted, correction)`: the number of accepted draft
    /// tokens and the model's argmax token at the first unaccepted position
    /// (the correction when `accepted < draft.len()`, else the bonus token).
    /// Wywołujący musi wcześniej użyć `validate_speculation_target()` oraz
    /// próbkowania greedy, aby wynik był zgodny z dekodowaniem sekwencyjnym.
    pub fn verify_greedy_draft(
        &mut self,
        seq: &mut SeqKv,
        fed: u32,
        draft: &[u32],
    ) -> Result<(usize, u32)> {
        debug_assert!(!draft.is_empty(), "verify called with an empty draft");
        debug_assert!(
            draft.len() <= MAX_SPEC_DRAFT,
            "draft exceeds MAX_SPEC_DRAFT"
        );
        let vocab = self.weights.descriptor.params.vocab_size;
        let t = draft.len() + 1;
        self.ensure_verify_bufs(t)?;

        let base = seq.len;
        let mut batch = Vec::with_capacity(t);
        batch.push(fed);
        batch.extend_from_slice(draft);
        let result = (|| {
            self.prefill_forward(seq, &batch)?;

            let stream = &self.stream;
            let vb = self.verify_bufs.as_ref().expect("ensured above");
            let pb = self
                .prefill_bufs
                .as_ref()
                .expect("prefill_forward allocated");
            self.logits_gemm(&vb.logits, &pb.x, t, stream)?;
            self.kernels
                .sample_batched_argmax_f32(&vb.ids, &vb.logits, t, vocab, stream)?;
            self.device
                .copy(&vb.ids, 0, &vb.pinned_ids, 0, t * 4, stream)?;
            self.device.synchronize()?;

            let ptr = vb
                .pinned_ids
                .host_ptr()
                .expect("pinned buffer has host mapping") as *const i32;
            let argmax = unsafe { std::slice::from_raw_parts(ptr, t) };
            let mut accepted = 0usize;
            let mut correction = 0u32;
            for i in 0..t {
                let am = argmax[i] as u32;
                if i < draft.len() && am == draft[i] {
                    accepted += 1;
                } else {
                    correction = am;
                    break;
                }
            }
            Ok((accepted, correction))
        })();

        finish_greedy_verification(&mut self.kv, &mut self.pt_seq, seq, base, result)
    }

    /// Record every launch of one decode step into a replayable graph.
    /// Stream capture does not execute the work, so buffer contents during
    /// capture are irrelevant — only addresses and launch geometry matter.
    fn capture_step(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = if Self::fused_decode_supported(&self.weights) {
            self.run_step_fused(AttnSrc::Paged)
        } else {
            self.run_step_separate(AttnSrc::Paged)
        };
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                // Abort the capture so the stream is usable again.
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// Whether every routed-MoE layer supports the device-side grouped dispatch
    /// (no host readback anywhere in the forward), so `run_step_moe` records
    /// cleanly into a replayable graph. False if any layer has a fallback quant
    /// (e.g. Q8_0 experts) that still needs a per-layer router readback.
    fn moe_fully_gidx(&self) -> bool {
        self.weights.layers.iter().all(|l| match &l.ffn {
            LayerFfn::Moe(moe) => Self::moe_gidx_capable(moe),
            LayerFfn::Dense(_) => true,
        })
    }

    /// Record the non-hybrid MoE decode step into a replayable graph. Only valid
    /// when `moe_fully_gidx()`: the expert dispatch reads the router selection on
    /// device (no readback), and all per-token inputs (token id, position, page
    /// table, seq len) come from device buffers refreshed before each replay.
    fn capture_step_moe(&self) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        let recorded = self.run_step_moe();
        match recorded {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// One decode step of the non-fused (separate-kernel) chain: explicit
    /// rmsnorm → qkv GEMVs → qkv_post (norm/rope/paged append) → attention →
    /// ffn. `src` selects the attention's K/V source: the paged cache
    /// (recorded into the replayable graph) or the tier staging slabs holding
    /// the sequence's full context per layer (streamed path, never captured).
    fn run_step_separate(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;

        {
            kernels.gather_rows_f16(
                &b.h,
                &self.weights.token_embd_f16,
                &b.ids,
                1,
                hidden,
                stream,
            )?;
            kernels.rmsnorm_f16(
                &b.x,
                &b.h,
                &self.weights.layers[0].attn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            let scale = 1.0 / (p.head_dim as f32).sqrt();
            // Byte offsets of the K and V sections inside the fused q|k|v
            // decode buffer (q occupies rows 0..q_dim, so its offset is 0).
            let q_dim = p.n_heads * p.head_dim;
            let kv_dim = p.n_kv_heads * p.head_dim;
            let k_byte_off = q_dim * 2;
            let v_byte_off = (q_dim + kv_dim) * 2;
            let n_layers = self.weights.layers.len();
            for l in 0..n_layers {
                let layer = &self.weights.layers[l];

                // Fused layers project q|k|v with ONE GEMV into one buffer,
                // then qkv_post fuses the whole q/k-norm + RoPE + kv-append
                // stretch into a second single launch (sections resolved via
                // host-computed byte offsets; rotated K lands directly in the
                // cache, so the K section of b.qkv is left un-rotated —
                // nothing reads it after this point).
                let q_buf = match &layer.attn().attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemv(&b.qkv, w, &b.x, stream)?;
                        kernels.qkv_post_f16(
                            &b.qkv,
                            0,
                            &b.qkv,
                            k_byte_off,
                            &b.qkv,
                            v_byte_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &b.pos,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            eps,
                            p.rope_theta,
                            stream,
                        )?;
                        &b.qkv
                    }
                    QkvWeights::FusedQk { qk, v } => {
                        // q|k land at the front of b.qkv (same section
                        // offsets as the fully fused layout); v is projected
                        // into its own buffer and handed to qkv_post by
                        // pointer.
                        self.gemv(&b.qkv, qk, &b.x, stream)?;
                        self.gemv(&b.v, v, &b.x, stream)?;
                        kernels.qkv_post_f16(
                            &b.qkv,
                            0,
                            &b.qkv,
                            k_byte_off,
                            &b.v,
                            0,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &b.pos,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            eps,
                            p.rope_theta,
                            stream,
                        )?;
                        &b.qkv
                    }
                    QkvWeights::Split { q, k, v } => {
                        self.gemv(&b.q, q, &b.x, stream)?;
                        self.gemv(&b.k, k, &b.x, stream)?;
                        self.gemv(&b.v, v, &b.x, stream)?;
                        if let Some(qn) = &layer.attn().q_norm {
                            kernels
                                .rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                        }
                        if let Some(kn) = &layer.attn().k_norm {
                            kernels.rmsnorm_f16(
                                &b.k,
                                &b.k,
                                kn,
                                p.n_kv_heads,
                                p.head_dim,
                                eps,
                                stream,
                            )?;
                        }
                        kernels.rope_neox_f16(
                            &b.q,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.head_dim,
                            p.rope_theta,
                            stream,
                        )?;
                        kernels.rope_neox_f16(
                            &b.k,
                            &b.pos,
                            1,
                            p.n_kv_heads,
                            p.head_dim,
                            p.rope_theta,
                            stream,
                        )?;
                        kernels.kv_append_f16(
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &b.k,
                            &b.v,
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            p.head_dim,
                            stream,
                        )?;
                        &b.q
                    }
                };

                match &src {
                    AttnSrc::Paged => {
                        kernels.attn_decode_f16(
                            &b.attn_out,
                            q_buf,
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    }
                    AttnSrc::Staged(seq) => {
                        // qkv_post / kv_append above already committed the new
                        // token to the canonical paged slab; staging picks it
                        // up through the resident-page D2D copies.
                        let tier = self
                            .tier
                            .as_ref()
                            .expect("staged attention requires tiering");
                        let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                        let slot = &tb.slots[0];
                        tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                        kernels.attn_decode_f16(
                            &b.attn_out,
                            q_buf,
                            &slot.stage[0],
                            &slot.stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            scale,
                            stream,
                        )?;
                    }
                }

                self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
                kernels.rmsnorm_residual_f16(
                    &b.x,
                    &b.h,
                    &b.o_out,
                    &layer.ffn_norm,
                    1,
                    hidden,
                    eps,
                    stream,
                )?;

                match &layer.dense_ffn()?.gate_up {
                    GateUpWeights::Fused(w) => {
                        self.gemv(&b.gate_up, w, &b.x, stream)?;
                        kernels.silu_mul_f16_at(&b.act, &b.gate_up, 0, inter * 2, inter, stream)?;
                    }
                    GateUpWeights::Split { gate, up } => {
                        self.gemv(&b.gate, gate, &b.x, stream)?;
                        self.gemv(&b.up, up, &b.x, stream)?;
                        kernels.silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
                    }
                }
                self.gemv(&b.down, &layer.dense_ffn()?.down, &b.act, stream)?;

                let next_norm = if l + 1 < n_layers {
                    &self.weights.layers[l + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                kernels
                    .rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;
            }

            self.logits_gemv(&b.logits, &b.x, stream)
        }
    }

    /// One decode step for a Mixture-of-Experts model (single token, paged f16
    /// cache). Attention mirrors the explicit separate chain but applies the
    /// model's QK-norm granularity (per-head for Qwen3-MoE, whole-vector for
    /// OLMoE); the FFN is replaced by `moe_decode_ffn`. Graph-captured when the
    /// model is fully gidx-capable (the routed experts are dispatched entirely
    /// on device); a fallback expert quant falls back to per-step launches.
    fn run_step_moe(&self) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Project q/k/v into the separate b.q/b.k/b.v buffers regardless of
            // weight fusion (a fused matrix is read as three row-window GEMVs).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemm_rows(&b.q, w, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, w, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemm_rows(&b.v, w, &b.x, 1, q_dim + kv_dim, kv_dim, stream)?;
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&b.q, qk, &b.x, 1, 0, q_dim, stream)?;
                    self.gemm_rows(&b.k, qk, &b.x, 1, q_dim, kv_dim, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv(&b.q, q, &b.x, stream)?;
                    self.gemv(&b.k, k, &b.x, stream)?;
                    self.gemv(&b.v, v, &b.x, stream)?;
                }
            }

            if let Some(qn) = &layer.attn().q_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, 1, q_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                }
            }
            if let Some(kn) = &layer.attn().k_norm {
                if p.qk_norm_over_hidden {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, 1, kv_dim, eps, stream)?;
                } else {
                    kernels.rmsnorm_f16(&b.k, &b.k, kn, p.n_kv_heads, p.head_dim, eps, stream)?;
                }
            }
            kernels.rope_neox_f16(&b.q, &b.pos, 1, p.n_heads, p.head_dim, p.rope_theta, stream)?;
            kernels.rope_neox_f16(
                &b.k,
                &b.pos,
                1,
                p.n_kv_heads,
                p.head_dim,
                p.rope_theta,
                stream,
            )?;
            kernels.kv_append_f16(
                &self.kv.k[l],
                &self.kv.v[l],
                &b.k,
                &b.v,
                &self.page_table_dev,
                &self.seq_len_dev,
                p.n_kv_heads,
                self.kv.cfg.page_size,
                p.head_dim,
                stream,
            )?;
            kernels.attn_decode_f16(
                &b.attn_out,
                &b.q,
                &self.kv.k[l],
                &self.kv.v[l],
                &self.page_table_dev,
                &self.seq_len_dev,
                1,
                p.n_heads,
                p.n_kv_heads,
                p.head_dim,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                scale,
                stream,
            )?;

            self.gemv(&b.o_out, &layer.attn().attn_o, &b.attn_out, stream)?;
            kernels.rmsnorm_residual_f16(
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;

            match &layer.ffn {
                LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, hidden, stream)?,
                LayerFfn::Dense(_) => {
                    return Err(ForgeError::Unsupported(
                        "dense layer inside a MoE forward pass".into(),
                    ))
                }
            }

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;
        }

        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Apply the routed experts for one token: `b.x` holds the FFN-normed
    /// input, `b.down` receives the weighted sum of the selected experts'
    /// SwiGLU outputs (plus the shared expert if present). The top-k experts
    /// are read back to the host to index the stacked expert weights.
    fn moe_decode_ffn(&self, moe: &MoeFfn, hidden: usize, stream: &Stream) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;
        let DevWeight::F16 {
            buf: router_buf, ..
        } = &moe.router
        else {
            return Err(ForgeError::Unsupported("MoE router must be f16".into()));
        };

        // Device-side grouped dispatch: the router's selected ids/weights stay
        // ON the device and drive the expert GEMVs + accumulate through the
        // `_gidx` kernels, so the whole per-layer FFN runs as queued stream work
        // with ZERO host readback / synchronize. The expert count `k` is a fixed
        // model constant (not data-dependent), so the launch sequence is static.
        if Self::moe_gidx_capable(moe) {
            if let Some(sg) = &moe.shared_gate {
                self.gemv(&mb.tmp, sg, &b.x, stream)?;
                self.kernels
                    .moe_sigmoid_f16_to_f32(&mb.shared_scale, &mb.tmp, stream)?;
            }
            self.kernels.moe_router_f16(
                &mb.ids,
                &mb.weights,
                &b.x,
                router_buf,
                1,
                hidden,
                moe.n_experts,
                k,
                moe.norm_topk,
                stream,
            )?;
            return self
                .moe_experts_accumulate_device(moe, &b.x, &b.down, 0, inter, hidden, k, stream);
        }

        // Fallback (expert quant without a `_gidx` kernel, e.g. Q8_0 down
        // projections): route on device but read the top-k selection back to
        // the host to launch the byte-offset expert GEMVs — one sync per layer.
        // Enqueue the shared-expert gate GEMV (when the arch has one) BEFORE the
        // router readback so its logit rides the SAME single sync as the top-k,
        // rather than forcing a second per-layer host round-trip.
        if let Some(sg) = &moe.shared_gate {
            self.gemv(&mb.tmp, sg, &b.x, stream)?;
            self.device
                .copy(&mb.tmp, 0, &mb.pinned_shared, 0, 2, stream)?;
        }
        self.kernels.moe_router_f16(
            &mb.ids,
            &mb.weights,
            &b.x,
            router_buf,
            1,
            hidden,
            moe.n_experts,
            k,
            moe.norm_topk,
            stream,
        )?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                k,
            )
        };
        // Per-token sigmoid gate for the shared expert (`ffn_gate_inp_shexp · x`);
        // 1.0 when the arch declares no shared-expert gate (OLMoE / Qwen3-MoE).
        let shared_scale = if moe.shared_gate.is_some() {
            let sp = mb.pinned_shared.host_ptr().expect("pinned host mapping");
            let bytes = unsafe { *(sp as *const [u8; 2]) };
            let logit = f16::from_le_bytes(bytes).to_f32();
            1.0 / (1.0 + (-logit).exp())
        } else {
            1.0
        };
        self.moe_experts_accumulate(
            moe,
            &b.x,
            &b.down,
            0,
            inter,
            hidden,
            ids,
            weights,
            shared_scale,
            stream,
        )
    }

    /// Device-side grouped expert dispatch for a single decode token: identical
    /// SwiGLU math to `moe_experts_accumulate`, but every routed expert's row
    /// window and routing weight are read ON DEVICE from `mb.ids`/`mb.weights`
    /// through the `_gidx` kernels — no host readback, no `synchronize`. The
    /// loop over `k` is over a fixed model constant, so the launch sequence is
    /// static and stream-ordered. The shared expert (row offset 0, host-known)
    /// reuses the ordinary GEMVs and folds in with the device-resident sigmoid
    /// gate scale. Bit-identical to the readback path for the routed experts;
    /// the only difference is the shared-gate sigmoid is computed on-GPU.
    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate_device(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        k: usize,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        for j in 0..k {
            self.gemv_rows_gidx(
                &b.gate,
                &moe.gate_exps,
                x_in,
                &mb.ids,
                j,
                inter,
                inter,
                stream,
            )?;
            self.gemv_rows_gidx(&b.up, &moe.up_exps, x_in, &mb.ids, j, inter, inter, stream)?;
            self.kernels
                .silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
            self.gemv_rows_gidx(
                &mb.tmp,
                &moe.down_exps,
                &b.act,
                &mb.ids,
                j,
                hidden,
                hidden,
                stream,
            )?;
            self.kernels.moe_scale_add_gidx_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                &mb.weights,
                j,
                j == 0,
                stream,
            )?;
        }
        if let Some(sh) = &moe.shared {
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .silu_mul_f16(&b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            // mb.shared_scale holds this layer's device sigmoid gate scale when
            // the arch has a shared gate; for a gate-less shared expert it stays
            // at the 1.0 seeded once at load, so no per-layer write is needed.
            self.kernels.moe_scale_add_gidx_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                &mb.shared_scale,
                0,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Run each selected expert's SwiGLU over the single-token activation
    /// `x_in` (contiguous [hidden] at offset 0) and accumulate
    /// `weight * expert_out` into `out` at byte offset `out_off`. Reuses the
    /// quant GEMV machinery indexed by expert row-offset; the shared expert (if
    /// any) is folded in last. Scratch (`b.gate/up/act`, `mb.tmp`) is
    /// single-token sized, so this serves both the decode and prefill loops.
    #[allow(clippy::too_many_arguments)]
    fn moe_experts_accumulate(
        &self,
        moe: &MoeFfn,
        x_in: &DevBuffer,
        out: &DevBuffer,
        out_off: usize,
        inter: usize,
        hidden: usize,
        ids: &[i32],
        weights: &[f32],
        shared_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let b = &self.bufs;
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        // A single-token GEMV over an expert = a row window of the stacked
        // expert matrix, i.e. gemm_rows at the expert row-offset (rows-per-
        // expert = inter for gate/up, hidden for down).
        for (j, (&e, &wt)) in ids.iter().zip(weights).enumerate() {
            let e = e as usize;
            if e >= moe.n_experts {
                return Err(ForgeError::Kernel(format!(
                    "router selected out-of-range expert {e}"
                )));
            }
            self.gemv_rows(&b.gate, &moe.gate_exps, x_in, e * inter, inter, stream)?;
            self.gemv_rows(&b.up, &moe.up_exps, x_in, e * inter, inter, stream)?;
            self.kernels
                .silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
            self.gemv_rows(&mb.tmp, &moe.down_exps, &b.act, e * hidden, hidden, stream)?;
            self.kernels
                .moe_scale_add_f16(out, out_off, &mb.tmp, 0, hidden, wt, j == 0, stream)?;
        }
        // Shared always-on expert: a dense SwiGLU added on top, scaled by the
        // per-token sigmoid gate (`shared_scale`; 1.0 when the arch has no
        // shared-expert gate).
        if let Some(sh) = &moe.shared {
            // Shared expert down is [hidden, shared_inter], so cols = its width.
            let sh_inter = sh.down.cols();
            match &sh.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_rows(&b.gate, w, x_in, 0, sh_inter, stream)?;
                    self.gemv_rows(&b.up, w, x_in, sh_inter, sh_inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemv_rows(&b.gate, gate, x_in, 0, gate.rows(), stream)?;
                    self.gemv_rows(&b.up, up, x_in, 0, up.rows(), stream)?;
                }
            }
            self.kernels
                .silu_mul_f16(&b.act, &b.gate, &b.up, sh_inter, stream)?;
            self.gemv_rows(&mb.tmp, &sh.down, &b.act, 0, sh.down.rows(), stream)?;
            self.kernels.moe_scale_add_f16(
                out,
                out_off,
                &mb.tmp,
                0,
                hidden,
                shared_scale,
                false,
                stream,
            )?;
        }
        Ok(())
    }

    /// Routed experts for a prefill chunk: route all `t` tokens at once, then
    /// apply each token's top-k experts, writing `[t, hidden]` into `pb.down`.
    /// Correctness-first per-token loop (grouped-GEMM permute/unpermute is a
    /// tracked perf follow-up); the router readback is one sync per layer.
    fn moe_prefill_ffn(
        &self,
        moe: &MoeFfn,
        t: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        let mb = self.moe_bufs.as_ref().expect("MoE model has moe_bufs");
        let pb = self.prefill_bufs.as_ref().expect("prefill bufs allocated");
        let inter = moe.moe_inter;
        let k = moe.n_experts_used;
        let DevWeight::F16 {
            buf: router_buf, ..
        } = &moe.router
        else {
            return Err(ForgeError::Unsupported("MoE router must be f16".into()));
        };
        self.kernels.moe_router_f16(
            &mb.ids,
            &mb.weights,
            &pb.x,
            router_buf,
            t,
            hidden,
            moe.n_experts,
            k,
            moe.norm_topk,
            stream,
        )?;
        self.device
            .copy(&mb.ids, 0, &mb.pinned_ids, 0, t * k * 4, stream)?;
        self.device
            .copy(&mb.weights, 0, &mb.pinned_weights, 0, t * k * 4, stream)?;
        self.device.synchronize()?;
        let ids = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_ids.host_ptr().expect("pinned host mapping") as *const i32,
                t * k,
            )
        };
        let weights = unsafe {
            std::slice::from_raw_parts(
                mb.pinned_weights.host_ptr().expect("pinned host mapping") as *const f32,
                t * k,
            )
        };
        for ti in 0..t {
            // Copy this token's normed hidden into a contiguous scratch row so
            // the single-token expert GEMVs read from offset 0.
            self.device
                .copy(&pb.x, ti * hidden * 2, &mb.xrow, 0, hidden * 2, stream)?;
            self.moe_experts_accumulate(
                moe,
                &mb.xrow,
                &pb.down,
                ti * hidden * 2,
                inter,
                hidden,
                &ids[ti * k..(ti + 1) * k],
                &weights[ti * k..(ti + 1) * k],
                1.0,
                stream,
            )?;
        }
        Ok(())
    }

    /// Whether this is the hybrid attention/Gated-DeltaNet MoE arch (qwen35moe).
    fn is_hybrid(&self) -> bool {
        self.weights.descriptor.params.ssm.is_some()
    }

    /// NEOX partial-rotary width for the hybrid attention layers: M-RoPE over
    /// text positions rotates the first `2*Σ sections` dims of each head.
    fn hybrid_n_rot(&self) -> usize {
        let p = &self.weights.descriptor.params;
        p.rope_sections
            .map(|s| s.iter().sum::<u32>() as usize * 2)
            .unwrap_or(p.head_dim)
    }

    /// Allocate the hybrid single-token scratch (gated-attention de-interleave +
    /// DeltaNet conv/recurrence buffers) on first use.
    fn ensure_hybrid_bufs(&mut self) -> Result<()> {
        if self.hybrid_bufs.is_some() {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.clone().expect("hybrid model has ssm params");
        let q_dim = p.n_heads * p.head_dim;
        let q_full = q_dim * 2;
        let conv_dim = ssm.conv_dim();
        let value_dim = ssm.value_dim();
        let key_dim = ssm.key_dim();
        let nv = ssm.n_v_heads();
        let device = self.device.clone();
        let a16 = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let a32 = |elems: usize| device.alloc(elems * 4, MemKind::Device, Pool::Activations);
        self.hybrid_bufs = Some(HybridBufs {
            q_full: a16(q_full)?,
            qc: a16(q_dim)?,
            gatec: a16(q_dim)?,
            gated: a16(q_dim)?,
            qkv_mixed: a16(conv_dim)?,
            z: a16(value_dim)?,
            conv_out: a16(conv_dim)?,
            q16: a16(key_dim)?,
            k16: a16(key_dim)?,
            q16src: a16(key_dim)?,
            k16src: a16(key_dim)?,
            q32: a16(value_dim)?,
            k32: a16(value_dim)?,
            vtok: a16(value_dim)?,
            alpha: a16(nv)?,
            beta_raw: a16(nv)?,
            g: a32(nv)?,
            beta_f: a32(nv)?,
            o: a16(value_dim)?,
            normed: a16(value_dim)?,
            pinned_embed: device.alloc(
                p.hidden_size * 2,
                MemKind::PinnedHost,
                Pool::Activations,
            )?,
        });
        Ok(())
    }

    /// Zero every DeltaNet layer's recurrent state (conv window + state matrix)
    /// at the start of a new sequence.
    fn zero_ssm(&self) -> Result<()> {
        let Some(ssm) = &self.weights.descriptor.params.ssm else {
            return Ok(());
        };
        let conv_bytes = ssm.conv_dim() * (ssm.d_conv - 1) * 2;
        let state_bytes = ssm.n_v_heads() * ssm.d_state * ssm.d_state * 4;
        let zc = vec![0u8; conv_bytes];
        let zs = vec![0u8; state_bytes];
        for s in self.ssm.iter().flatten() {
            self.device.write(&zc, &s.conv, 0)?;
            self.device.write(&zs, &s.state, 0)?;
        }
        Ok(())
    }

    /// One token through the hybrid (gated-attention / Gated-DeltaNet + MoE)
    /// stack. Mirrors `run_step_moe`'s residual/norm skeleton, dispatching the
    /// token mixer by layer kind and folding in the gated shared expert. Inputs
    /// (`b.ids`/`b.pos`/`seq_len_dev`/page table) must be uploaded by the
    /// caller; the next-token logits land in `b.logits` when `want_logits`.
    fn hybrid_forward_token(&self, token_id: u32, want_logits: bool, src: AttnSrc) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let n_layers = self.weights.layers.len();

        // Host-side embedding gather: stage this token's f16 row in pinned host
        // memory and push it to the device residual buffer with an async H2D on
        // the compute stream (the table lives in host RAM to keep VRAM for
        // weights). Stream ordering serializes this after the previous token's
        // tail, so no blocking sync is needed to avoid a race on `b.h`.
        let host = self
            .weights
            .token_embd_host
            .as_ref()
            .expect("hybrid model has host embedding");
        let base = token_id as usize * hidden;
        let row = host.get(base..base + hidden).ok_or_else(|| {
            ForgeError::Scheduler(format!("token id {token_id} out of embedding range"))
        })?;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let dst = hb
            .pinned_embed
            .host_ptr()
            .expect("pinned buffer has host mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(row.as_ptr() as *const u8, dst, hidden * 2);
        }
        self.device
            .copy(&hb.pinned_embed, 0, &b.h, 0, hidden * 2, stream)?;
        kernels.rmsnorm_f16(
            &b.x,
            &b.h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            match &layer.mixer {
                LayerMixer::Attention(a) => self.hybrid_attn_mixer(l, a, &src)?,
                LayerMixer::DeltaNet(d) => self.hybrid_delta_mixer(l, d)?,
            }
            // Residual add (mixer output) + post-attention norm for the FFN.
            kernels.rmsnorm_residual_f16(
                &b.x,
                &b.h,
                &b.o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                stream,
            )?;
            match &layer.ffn {
                LayerFfn::Moe(moe) => self.moe_decode_ffn(moe, hidden, stream)?,
                LayerFfn::Dense(ffn) => {
                    match &ffn.gate_up {
                        GateUpWeights::Fused(weight) => {
                            self.gemv(&b.gate_up, weight, &b.x, stream)?;
                            kernels.silu_mul_f16_at(
                                &b.act,
                                &b.gate_up,
                                0,
                                inter * 2,
                                inter,
                                stream,
                            )?;
                        }
                        GateUpWeights::Split { gate, up } => {
                            self.gemv(&b.gate, gate, &b.x, stream)?;
                            self.gemv(&b.up, up, &b.x, stream)?;
                            kernels.silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
                        }
                    }
                    self.gemv(&b.down, &ffn.down, &b.act, stream)?;
                }
            }
            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;

            if self.hybrid_debug {
                self.device.synchronize()?;
                let mut hb = vec![0u8; hidden * 2];
                self.device.read(&b.h, 0, &mut hb)?;
                let hf: &[f16] = bytemuck::cast_slice(&hb);
                let norm: f32 = hf.iter().map(|v| v.to_f32().powi(2)).sum::<f32>().sqrt();
                let kind = if matches!(layer.mixer, LayerMixer::DeltaNet(_)) {
                    "delta"
                } else {
                    "attn"
                };
                eprintln!("  layer {l:2} [{kind}] ||h|| = {norm:.4}");
            }
        }

        if want_logits {
            self.logits_gemv(&b.logits, &b.x, stream)?;
        }
        Ok(())
    }

    /// Gated softmax-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. The Q
    /// projection is gated (`[q, gate]` interleaved per head), so q/gate are
    /// de-interleaved, per-head QK-norm + partial RoPE applied, causal decode
    /// attention run, then `out = attn ⊙ sigmoid(gate)` before the O projection.
    fn hybrid_attn_mixer(&self, l: usize, a: &AttnWeights, src: &AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let head_dim = p.head_dim;
        let n_heads = p.n_heads;
        let n_kv = p.n_kv_heads;
        let q_dim = n_heads * head_dim;
        let eps = p.rms_norm_eps;
        let theta = p.rope_theta;
        let n_rot = self.hybrid_n_rot();
        let scale = 1.0 / (head_dim as f32).sqrt();
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let (wq, wk, wv) = match &a.attn_qkv {
            QkvWeights::Split { q, k, v } => (q, k, v),
            _ => {
                return Err(ForgeError::Unsupported(
                    "hybrid attention expects split q/k/v weights".into(),
                ))
            }
        };
        // Gated Q projection [2*q_dim], then de-interleave per head: q at
        // h*2*head_dim, gate at h*2*head_dim + head_dim.
        let qkv_grouped = self.gemv_nvfp4_gguf_group(
            &[(&hb.q_full, wq), (&b.k, wk), (&b.v, wv)],
            &b.x,
            stream,
        )?;
        if !qkv_grouped {
            self.gemv(&hb.q_full, wq, &b.x, stream)?;
            self.gemv(&b.k, wk, &b.x, stream)?;
            self.gemv(&b.v, wv, &b.x, stream)?;
        }
        kernels.deinterleave_gate_f16(&hb.qc, &hb.gatec, &hb.q_full, head_dim, q_dim, stream)?;
        if let Some(qn) = &a.q_norm {
            kernels.rmsnorm_f16(&hb.qc, &hb.qc, qn, n_heads, head_dim, eps, stream)?;
        }
        if let Some(kn) = &a.k_norm {
            kernels.rmsnorm_f16(&b.k, &b.k, kn, n_kv, head_dim, eps, stream)?;
        }
        kernels
            .rope_neox_partial_f16(&hb.qc, &b.pos, 1, n_heads, head_dim, n_rot, theta, stream)?;
        kernels.rope_neox_partial_f16(&b.k, &b.pos, 1, n_kv, head_dim, n_rot, theta, stream)?;
        kernels.kv_append_f16(
            &self.kv.k[l],
            &self.kv.v[l],
            &b.k,
            &b.v,
            &self.page_table_dev,
            &self.seq_len_dev,
            n_kv,
            self.kv.cfg.page_size,
            head_dim,
            stream,
        )?;
        match src {
            AttnSrc::Paged => {
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &hb.qc,
                    &self.kv.k[l],
                    &self.kv.v[l],
                    &self.page_table_dev,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    stream,
                )?;
            }
            AttnSrc::Staged(seq) => {
                // Spilled sequence: kv_append above committed this token into
                // the resident tail of the canonical paged slab; staging then
                // materializes the FULL context for this attention layer (cold
                // pages streamed from RAM/NVMe, resident pages copied D2D) and
                // attention runs over it via the identity page table. Same
                // kernel + order as the paged path, so greedy tokens are
                // bit-identical to an untiered run.
                let tier = self
                    .tier
                    .as_ref()
                    .expect("staged attention requires tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_f16(
                    &b.attn_out,
                    &hb.qc,
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    1,
                    n_heads,
                    n_kv,
                    head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    scale,
                    stream,
                )?;
            }
        }
        // Output gate: out = attn ⊙ sigmoid(gate), applied on-device so the
        // whole mixer stays on the compute stream (no per-layer host sync).
        kernels.sigmoid_mul_f16(&hb.gated, &b.attn_out, &hb.gatec, q_dim, stream)?;
        self.gemv(&b.o_out, &a.attn_o, &hb.gated, stream)?;
        Ok(())
    }

    /// Gated-DeltaNet linear-attention mixer for one hybrid layer. `b.x` is the
    /// pre-attention normed input; the mixer output lands in `b.o_out`. Advances
    /// this layer's resident conv window + recurrent state by one token.
    fn hybrid_delta_mixer(&self, l: usize, d: &DeltaNetWeights) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let ssm = p.ssm.as_ref().expect("hybrid has ssm params");
        let eps = p.rms_norm_eps;
        let conv_dim = ssm.conv_dim();
        let d_conv = ssm.d_conv;
        let key_dim = ssm.key_dim();
        let value_dim = ssm.value_dim();
        let d_state = ssm.d_state;
        let n_k = ssm.n_k_heads();
        let n_v = ssm.n_v_heads();
        let rep = n_v / n_k;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let hb = self.hybrid_bufs.as_ref().expect("hybrid bufs allocated");
        let st = self.ssm[l].as_ref().expect("DeltaNet layer has ssm state");

        // Input projections: mixed q|k|v conv stream, output gate z, and the
        // per-head decay/write-gate projections.
        self.gemv(&hb.qkv_mixed, &d.in_proj, &b.x, stream)?;
        self.gemv(&hb.z, &d.gate_proj, &b.x, stream)?;
        self.gemv(&hb.alpha, &d.alpha_proj, &b.x, stream)?;
        self.gemv(&hb.beta_raw, &d.beta_proj, &b.x, stream)?;

        // Causal depthwise conv + SiLU (advances the conv window in place).
        kernels.deltanet_conv_silu_f16(
            &hb.conv_out,
            &st.conv,
            &hb.qkv_mixed,
            &d.conv1d,
            conv_dim,
            d_conv,
            stream,
        )?;
        // Split conv output into q/k (key_dim each) and v (value_dim).
        self.device
            .copy(&hb.conv_out, 0, &hb.q16src, 0, key_dim * 2, stream)?;
        self.device.copy(
            &hb.conv_out,
            key_dim * 2,
            &hb.k16src,
            0,
            key_dim * 2,
            stream,
        )?;
        self.device.copy(
            &hb.conv_out,
            2 * key_dim * 2,
            &hb.vtok,
            0,
            value_dim * 2,
            stream,
        )?;
        // Per-head L2 norm on the key-head q/k (n_k heads over d_state).
        kernels.l2norm_heads_f16(&hb.q16, &hb.q16src, n_k, d_state, eps, stream)?;
        kernels.l2norm_heads_f16(&hb.k16, &hb.k16src, n_k, d_state, eps, stream)?;
        // Expand key-heads to value-heads. llama.cpp's qwen35moe graph uses
        // `ggml_repeat` (block-tiled: v-head j uses k-head j % n_k), and this
        // build runs that non-fused path (fused GDN is disabled for Qwen3.6),
        // so the key block is tiled `rep` times: [k0..k15, k0..k15].
        let key_bytes = n_k * d_state * 2;
        for r in 0..rep {
            self.device
                .copy(&hb.q16, 0, &hb.q32, r * key_bytes, key_bytes, stream)?;
            self.device
                .copy(&hb.k16, 0, &hb.k32, r * key_bytes, key_bytes, stream)?;
        }
        // Per-head log-decay g = softplus(alpha + dt_bias)·a and beta gate.
        kernels.deltanet_log_decay_f32(&hb.g, &hb.alpha, &d.dt_bias, &d.a, n_v, stream)?;
        kernels.deltanet_beta_sigmoid_f32(&hb.beta_f, &hb.beta_raw, n_v, stream)?;
        // Rank-1 gated-delta recurrence (advances the state matrix in place).
        kernels.deltanet_gated_step_f16(
            &hb.o, &st.state, &hb.q32, &hb.k32, &hb.vtok, &hb.g, &hb.beta_f, n_v, d_state, stream,
        )?;
        // Output gated RMSNorm then the value-dim → hidden out projection.
        kernels.deltanet_gated_rmsnorm_f16(
            &hb.normed,
            &hb.o,
            &hb.z,
            &d.ssm_norm,
            n_v,
            d_state,
            eps,
            stream,
        )?;
        self.gemv(&b.o_out, &d.out_proj, &hb.normed, stream)?;
        Ok(())
    }

    /// Prefill a prompt chunk for the hybrid arch as a sequential per-token
    /// recurrent scan (the DeltaNet state carries token-to-token). Returns the
    /// last token's next-token logits. Tier-aware: each token first spills the
    /// coldest attention KV if the hot pool is full, so a long prompt beyond the
    /// VRAM pool prefills by streaming older attention KV back per layer while
    /// the resident DeltaNet state advances untouched.
    fn prefill_hybrid(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        self.ensure_hybrid_bufs()?;
        let p = self.weights.descriptor.params.clone();
        let vocab = p.vocab_size;
        let page_size = self.kv.cfg.page_size;
        let tier_t0 = self.tier.is_some().then(std::time::Instant::now);
        let mut last_logits = Vec::new();
        for (i, &tok) in tokens.iter().enumerate() {
            let pos = seq.len;
            if pos == 0 {
                self.zero_ssm()?;
            }
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            // Free VRAM pages for this token before growing; may spill the
            // coldest attention KV to RAM/NVMe. Retain the tokens (recompute
            // path) and track the still-purely-prefilled prefix.
            self.tier_ensure_capacity(seq, 1)?;
            if self.tier.is_some() {
                if seq.tokens.len() == seq.prefilled_len {
                    seq.prefilled_len += 1;
                }
                seq.tokens.push(tok);
            }
            let staged = self.tier.is_some() && !seq.spilled.is_empty();
            let page_boundary = seq.len.is_multiple_of(page_size);
            self.kv.grow(seq)?;
            self.upload_decode_inputs(tok, pos)?;
            let want = i + 1 == tokens.len();
            if staged {
                self.tier
                    .as_mut()
                    .expect("staged implies tiering")
                    .prepare_streaming(seq)?;
                self.upload_page_table(seq)?;
                self.hybrid_forward_token(tok, want, AttnSrc::Staged(seq))?;
            } else {
                if page_boundary || self.pt_seq != seq.id {
                    self.upload_page_table(seq)?;
                }
                self.hybrid_forward_token(tok, want, AttnSrc::Paged)?;
            }
            self.mtp_catchup_token(tok)?;
            if want {
                self.device.copy(
                    &self.bufs.logits,
                    0,
                    &self.bufs.pinned_logits,
                    0,
                    vocab * 4,
                    &self.stream,
                )?;
                self.device.synchronize()?;
                let lp =
                    self.bufs
                        .pinned_logits
                        .host_ptr()
                        .expect("pinned buffer has host mapping") as *const f32;
                last_logits = unsafe { std::slice::from_raw_parts(lp, vocab) }.to_vec();
            }
        }
        // Feed the measured prefill rate into the tier's transfer-vs-recompute
        // estimate (bit-identical recompute is eligible only for prefill KV).
        if let (Some(t0), Some(tier)) = (tier_t0, self.tier.as_ref()) {
            if !tokens.is_empty() {
                tier.note_prefill(tokens.len(), t0.elapsed().as_secs_f64());
            }
        }
        Ok(last_logits)
    }

    /// Fused decode step: six launches per layer instead of nine. The
    /// residual stream is carried as the (h f16, h32 f32) pair — every
    /// norm-consuming kernel recomputes the RMSNorm per block from that pair
    /// (bit-identical to the separate rmsnorm kernels, see decode_fused.mojo)
    /// and attn_decode_split folds the whole qkv_post stage into the
    /// attention prologue (the split/combine pair fills the GPU where one
    /// block per head could not). Layer 0 sums squares from h directly (h32
    /// is only materialized by the first gemv_residual of the step).
    ///
    /// `src` selects the attention's K/V home: the paged cache (recorded into
    /// the replayable decode graph) or the tier staging slabs carrying the
    /// sequence's full context per layer (streamed path, never captured). On
    /// the staged path attn_decode_split appends the new token INTO staging
    /// and the tail page is mirrored back to the canonical paged slab.
    fn run_step_fused(&self, src: AttnSrc) -> Result<()> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let k_byte_off = q_dim * 2;
        let v_byte_off = (q_dim + kv_dim) * 2;

        kernels.gather_rows_f16(
            &b.h,
            &self.weights.token_embd_f16,
            &b.ids,
            1,
            hidden,
            stream,
        )?;

        let n_layers = self.weights.layers.len();
        if let AttnSrc::Staged(seq) = &src {
            // Ping-pong staging: layer l+1 restores on the tier's transfer
            // stream while layer l computes. Both slots start "free" relative
            // to any prior compute work, and slot 0 prestages layer 0.
            let tier = self
                .tier
                .as_ref()
                .expect("staged attention requires tiering");
            let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
            let xfer = tier.xfer_stream();
            for slot in &tb.slots {
                self.device.record_event(&slot.free, stream)?;
            }
            self.device.wait_event(xfer, &tb.slots[0].free)?;
            tier.stage_layer(&self.kv, seq, 0, &tb.slots[0].stage, 0, xfer)?;
            self.device.record_event(&tb.slots[0].ready, xfer)?;
        }
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Fused QKV projects with one gemv_norm into the fused buffer;
            // split layers (mixed formats) run one gemv_norm per projection —
            // per-row math is identical, only the block-level norm recompute
            // repeats. Both feed attn_decode_split via buffer + byte offset.
            let (q_buf, q_off, k_buf, k_off, v_buf, v_off) = match &layer.attn().attn_qkv {
                QkvWeights::Fused(w_qkv) => {
                    self.gemv_norm(&b.qkv, w_qkv, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.qkv, v_byte_off)
                }
                QkvWeights::FusedQk { qk, v } => {
                    // The fused q|k rows land at the front of b.qkv, exactly
                    // where the Fused layout puts them; v goes to its own
                    // buffer.
                    self.gemv_norm(&b.qkv, qk, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.qkv, 0usize, &b.qkv, k_byte_off, &b.v, 0usize)
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemv_norm(&b.q, q, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.k, k, &layer.attn_norm, l == 0, eps, stream)?;
                    self.gemv_norm(&b.v, v, &layer.attn_norm, l == 0, eps, stream)?;
                    (&b.q, 0usize, &b.k, 0usize, &b.v, 0usize)
                }
            };
            let gqa_q_heads = p.n_kv_heads.checked_mul(4);
            let use_gqa = std::env::var("FORGE_ATTN_GQA").ok().as_deref() != Some("0")
                && self.device.caps().vendor == Vendor::Nvidia
                && kernels.supports_attn_decode_gqa4_f16_hd128()
                && self.kv.cfg.dtype() == forge_types::DType::F16
                && p.head_dim == 128
                && gqa_q_heads == Some(p.n_heads)
                && layer.attn().q_norm.is_none()
                && layer.attn().k_norm.is_none();
            let attn_splits = if use_gqa {
                ATTN_DECODE_GQA_SPLITS
            } else {
                ATTN_DECODE_SPLITS
            };
            match &src {
                AttnSrc::Paged => {
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &self.kv.k[l],
                            &self.kv.v[l],
                            &self.page_table_dev,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
                AttnSrc::Staged(seq) => {
                    let tier = self
                        .tier
                        .as_ref()
                        .expect("staged attention requires tiering");
                    let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                    let xfer = tier.xfer_stream();
                    let s = l % STAGE_SLOTS;
                    // Prestage the NEXT layer into the other slot on the
                    // transfer stream while this layer computes.
                    if l + 1 < n_layers {
                        let ns = (l + 1) % STAGE_SLOTS;
                        self.device.wait_event(xfer, &tb.slots[ns].free)?;
                        tier.stage_layer(&self.kv, seq, l + 1, &tb.slots[ns].stage, ns, xfer)?;
                        self.device.record_event(&tb.slots[ns].ready, xfer)?;
                    }
                    self.device.wait_event(stream, &tb.slots[s].ready)?;
                    if use_gqa {
                        kernels.attn_decode_split_gqa4_f16_hd128(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    } else {
                        kernels.attn_decode_split(
                            &b.attn_parts,
                            q_buf,
                            q_off,
                            k_buf,
                            k_off,
                            v_buf,
                            v_off,
                            layer.attn().q_norm.as_ref(),
                            layer.attn().k_norm.as_ref(),
                            &tb.slots[s].stage[0],
                            &tb.slots[s].stage[1],
                            &tb.identity_pt,
                            &self.seq_len_dev,
                            &b.pos,
                            1,
                            p.n_heads,
                            p.n_kv_heads,
                            p.head_dim,
                            self.kv.cfg.page_size,
                            self.max_pages_per_seq,
                            attn_splits,
                            self.kv.cfg.dtype(),
                            eps,
                            p.rope_theta,
                            scale,
                            stream,
                        )?;
                    }
                }
            }
            if use_gqa {
                kernels.attn_decode_combine_gqa2_f16_hd128(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    attn_splits,
                    stream,
                )?;
            } else {
                kernels.attn_decode_combine_f16(
                    &b.attn_out,
                    &b.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    attn_splits,
                    stream,
                )?;
            }
            if let AttnSrc::Staged(seq) = &src {
                // The kernel appended this token's rope'd K/V into the staging
                // tail page; mirror that page back into the canonical paged
                // cache so future steps (and spills) see it, then mark the
                // slot free for the transfer stream to restage.
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let s = l % STAGE_SLOTS;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &tb.slots[s].stage[0],
                    lp * rb,
                    &self.kv.k[l],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &tb.slots[s].stage[1],
                    lp * rb,
                    &self.kv.v[l],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.record_event(&tb.slots[s].free, stream)?;
            }
            self.gemv_residual(&layer.attn().attn_o, &b.attn_out, stream)?;
            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemv_norm_silu(&b.act, w, &layer.ffn_norm, eps, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    // Mixed-format gate/up: two gemv_norm launches (same
                    // per-row math as the fused silu kernels, the norm
                    // recompute repeats) + the elementwise SwiGLU combine.
                    // Rounding matches gemv_norm_silu: both projections are
                    // stored as f16 before silu_mul reads them.
                    self.gemv_norm(&b.gate, gate, &layer.ffn_norm, false, eps, stream)?;
                    self.gemv_norm(&b.up, up, &layer.ffn_norm, false, eps, stream)?;
                    kernels.silu_mul_f16(&b.act, &b.gate, &b.up, p.intermediate_size, stream)?;
                }
            }
            self.gemv_residual(&layer.dense_ffn()?.down, &b.act, stream)?;
        }

        kernels.rmsnorm_h32_f16(
            &b.x,
            &b.h,
            &b.h32,
            &self.weights.output_norm,
            1,
            hidden,
            eps,
            stream,
        )?;
        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Batched logit head: y[b, vocab] f32 = lm_head · x[b, hidden].
    /// Dodatkowa paczka FP8 obsługuje tylko single-stream GEMV; batching świadomie
    /// pozostaje na bazowym F16/Q8_0, aby nie zmieniać jego sprawdzonego GEMM.
    fn logits_gemm(
        &self,
        y_f32: &DevBuffer,
        x: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        match &self.weights.lm_head {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemm_f16_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemm_q8_0_out_f32_at(y_f32, buf, 0, x, *rows, *cols, n_tokens, stream),
            _ => Err(ForgeError::Unsupported(
                "batched logits head must be f16 or q8_0".into(),
            )),
        }
    }

    /// Smallest captured bucket >= `n`: a power of two, capped at `batch_cap`.
    /// A live batch replays the smallest bucket that holds it (dead lanes pad
    /// up to the bucket and are never sampled).
    fn bucket_for(&self, n: usize) -> usize {
        let mut s = 1;
        while s < n {
            s *= 2;
        }
        s.min(self.batch_cap).max(1)
    }

    /// Provision the continuous-batching decode scratch for up to `cap`
    /// sequences. Idempotent; a larger `cap` than a previous call reallocates.
    pub fn ensure_batch(&mut self, cap: usize) -> Result<()> {
        let cap = cap.max(1);
        if self.batch_bufs.as_ref().is_some_and(|b| b.cap >= cap) {
            return Ok(());
        }
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let vocab = p.vocab_size;
        let mpp = self.max_pages_per_seq;
        let max_seq = self.kv.cfg.max_pages_per_seq * self.kv.cfg.page_size;
        let dev = &self.device;
        let f16 = |elems: usize| dev.alloc(elems * 2, MemKind::Device, Pool::Activations);
        let f32b = |elems: usize| dev.alloc(elems * 4, MemKind::Device, Pool::Activations);
        let pin = |bytes: usize| dev.alloc(bytes, MemKind::PinnedHost, Pool::Activations);
        self.batch_bufs = Some(BatchBufs {
            cap,
            h: f16(cap * hidden)?,
            x: f16(cap * hidden)?,
            q: f16(cap * q_dim)?,
            k: f16(cap * kv_dim)?,
            v: f16(cap * kv_dim)?,
            attn_parts: f32b(cap * p.n_heads * ATTN_DECODE_SPLITS * (p.head_dim + 2))?,
            attn_out: f16(cap * q_dim)?,
            o_out: f16(cap * hidden)?,
            gate: f16(cap * inter)?,
            up: f16(cap * inter)?,
            act: f16(cap * inter)?,
            down: f16(cap * hidden)?,
            logits: f32b(cap * vocab)?,
            ids: f32b(cap)?,
            positions: f32b(cap)?,
            seq_lens: f32b(cap)?,
            page_table: f32b(cap * mpp)?,
            pinned_meta: pin(cap * 3 * 4)?,
            pinned_pt: pin(cap * mpp * 4)?,
            samp_k: f32b(cap)?,
            samp_inv_t: f32b(cap)?,
            samp_top_p: f32b(cap)?,
            samp_min_p: f32b(cap)?,
            samp_seed: dev.alloc(cap * 8, MemKind::Device, Pool::Activations)?,
            samp_step: dev.alloc(cap * 8, MemKind::Device, Pool::Activations)?,
            pinned_samp: pin(cap * (4 * 4 + 2 * 8))?,
            pen_ids: f32b(cap * max_seq)?,
            pen_offsets: f32b(cap + 1)?,
            pen_vals: f32b(cap)?,
            pinned_pen_ids: pin(cap * max_seq * 4)?,
            pinned_pen_offsets: pin((cap + 1) * 4)?,
            pinned_pen_vals: pin(cap * 4)?,
            out_ids: f32b(cap)?,
            pinned_out: pin(cap * 4)?,
        });
        // Fresh scratch invalidates any graph captured against the old buffers.
        self.batch_graphs.clear();
        self.batch_cap = cap;
        Ok(())
    }

    /// Record one batched forward + logit head over `n` rows into the model
    /// stream (no sampling — that runs param-dependent, outside the graph).
    /// Mirrors the prefill dataflow (rmsnorm rows=n, batched GEMM projections,
    /// row-batched silu/residual) but swaps causal prefill attention for the
    /// per-sequence paged flash-decode. Lanes `0..resident` attend through
    /// their page tables in one launch; `streamed` lanes (packed at the tail
    /// of the batch: spilled KV that exceeds free VRAM) attend one at a time
    /// over the tier staging slabs holding their full context per layer. A
    /// batch with streamed lanes is never graph-captured; pure-resident
    /// buckets stay captured (`streamed` empty, `resident == n`).
    fn record_batch_forward(
        &self,
        n: usize,
        resident: usize,
        streamed: &[(usize, &SeqKv)],
    ) -> Result<()> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let eps = p.rms_norm_eps;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let kernels = &self.kernels;
        let stream = &self.stream;
        let bb = self.batch_bufs.as_ref().expect("batch bufs provisioned");
        let n_layers = self.weights.layers.len();

        kernels.gather_rows_f16(
            &bb.h,
            &self.weights.token_embd_f16,
            &bb.ids,
            n,
            hidden,
            stream,
        )?;
        kernels.rmsnorm_f16(
            &bb.x,
            &bb.h,
            &self.weights.layers[0].attn_norm,
            n,
            hidden,
            eps,
            stream,
        )?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            // Raw q/k/v projections (no norm/rope here — attn_decode_split folds
            // the q/k-norm + RoPE + paged append into its per-seq prologue).
            match &layer.attn().attn_qkv {
                QkvWeights::Fused(w) => {
                    self.gemm_rows(&bb.q, w, &bb.x, n, 0, q_dim, stream)?;
                    self.gemm_rows(&bb.k, w, &bb.x, n, q_dim, kv_dim, stream)?;
                    self.gemm_rows(&bb.v, w, &bb.x, n, q_dim + kv_dim, kv_dim, stream)?;
                }
                QkvWeights::FusedQk { qk, v } => {
                    self.gemm_rows(&bb.q, qk, &bb.x, n, 0, q_dim, stream)?;
                    self.gemm_rows(&bb.k, qk, &bb.x, n, q_dim, kv_dim, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
                QkvWeights::Split { q, k, v } => {
                    self.gemm(&bb.q, q, &bb.x, n, stream)?;
                    self.gemm(&bb.k, k, &bb.x, n, stream)?;
                    self.gemm(&bb.v, v, &bb.x, n, stream)?;
                }
            }
            if resident > 0 {
                kernels.attn_decode_split(
                    &bb.attn_parts,
                    &bb.q,
                    0,
                    &bb.k,
                    0,
                    &bb.v,
                    0,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &self.kv.k[l],
                    &self.kv.v[l],
                    &bb.page_table,
                    &bb.seq_lens,
                    &bb.positions,
                    resident,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &bb.attn_out,
                    &bb.attn_parts,
                    resident,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
            }
            for &(lane, seq) in streamed {
                // Lane-scalar pos/len land in the single-seq buffers the
                // n_seqs=1 launch reads at index 0; the lane's q/k/v rows are
                // addressed by byte offset. The attention appends the token
                // into staging and the tail page mirrors back to the canonical
                // slab, exactly like the single-stream staged step. All copies
                // and launches ride the compute stream, so slab reuse across
                // lanes is stream-ordered.
                let tier = self.tier.as_ref().expect("streamed lanes require tiering");
                let tb = self.tier_bufs.as_ref().expect("tier staging allocated");
                let slot = &tb.slots[0];
                let db = &self.bufs;
                self.device
                    .copy(&bb.positions, lane * 4, &db.pos, 0, 4, stream)?;
                self.device
                    .copy(&bb.seq_lens, lane * 4, &self.seq_len_dev, 0, 4, stream)?;
                tier.stage_layer(&self.kv, seq, l, &slot.stage, 0, stream)?;
                kernels.attn_decode_split(
                    &db.attn_parts,
                    &bb.q,
                    lane * q_dim * 2,
                    &bb.k,
                    lane * kv_dim * 2,
                    &bb.v,
                    lane * kv_dim * 2,
                    layer.attn().q_norm.as_ref(),
                    layer.attn().k_norm.as_ref(),
                    &slot.stage[0],
                    &slot.stage[1],
                    &tb.identity_pt,
                    &self.seq_len_dev,
                    &db.pos,
                    1,
                    p.n_heads,
                    p.n_kv_heads,
                    p.head_dim,
                    self.kv.cfg.page_size,
                    self.max_pages_per_seq,
                    ATTN_DECODE_SPLITS,
                    self.kv.cfg.dtype(),
                    eps,
                    p.rope_theta,
                    scale,
                    stream,
                )?;
                kernels.attn_decode_combine_f16(
                    &db.attn_out,
                    &db.attn_parts,
                    1,
                    p.n_heads,
                    p.head_dim,
                    ATTN_DECODE_SPLITS,
                    stream,
                )?;
                self.device.copy(
                    &db.attn_out,
                    0,
                    &bb.attn_out,
                    lane * q_dim * 2,
                    q_dim * 2,
                    stream,
                )?;
                let rb = tb.region_bytes[0];
                let lp = seq.pages.len() - 1;
                let phys = seq.pages[lp] as usize;
                self.device.copy(
                    &slot.stage[0],
                    lp * rb,
                    &self.kv.k[l],
                    phys * rb,
                    rb,
                    stream,
                )?;
                self.device.copy(
                    &slot.stage[1],
                    lp * rb,
                    &self.kv.v[l],
                    phys * rb,
                    rb,
                    stream,
                )?;
            }
            self.gemm(&bb.o_out, &layer.attn().attn_o, &bb.attn_out, n, stream)?;
            kernels.rmsnorm_residual_f16(
                &bb.x,
                &bb.h,
                &bb.o_out,
                &layer.ffn_norm,
                n,
                hidden,
                eps,
                stream,
            )?;

            match &layer.dense_ffn()?.gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemm_rows(&bb.gate, w, &bb.x, n, 0, inter, stream)?;
                    self.gemm_rows(&bb.up, w, &bb.x, n, inter, inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&bb.gate, gate, &bb.x, n, stream)?;
                    self.gemm(&bb.up, up, &bb.x, n, stream)?;
                }
            }
            kernels.silu_mul_f16(&bb.act, &bb.gate, &bb.up, n * inter, stream)?;
            self.gemm(&bb.down, &layer.dense_ffn()?.down, &bb.act, n, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels
                .rmsnorm_residual_f16(&bb.x, &bb.h, &bb.down, next_norm, n, hidden, eps, stream)?;
        }

        self.logits_gemm(&bb.logits, &bb.x, n, stream)
    }

    /// Capture `record_batch_forward(bucket)` into a replayable graph.
    fn capture_batch_forward(&self, bucket: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.record_batch_forward(bucket, bucket, &[]) {
            Ok(()) => self.device.end_capture(&self.stream),
            Err(e) => {
                let _ = self.device.end_capture(&self.stream);
                Err(e)
            }
        }
    }

    /// Run one batched decode step: advance every sequence in `seqs` by its
    /// input token in `tokens`, sampling each successor on the GPU with its own
    /// params. Returns the `B` next-token ids. The forward+logit head replays a
    /// per-bucket CUDA graph (dead lanes padded to the bucket, never sampled);
    /// sampling runs after the replay so per-seq params (and the greedy/top-k
    /// mix) need no re-capture.
    pub fn batched_decode(
        &mut self,
        seqs: &mut [&mut SeqKv],
        tokens: &[u32],
        params: &[SeqSampleParams],
    ) -> Result<Vec<u32>> {
        let b = seqs.len();
        if b == 0 {
            return Ok(Vec::new());
        }
        if tokens.len() != b || params.len() != b {
            return Err(ForgeError::Scheduler(
                "batched_decode: seqs/tokens/params length mismatch".into(),
            ));
        }
        // Rot modes commit each appended token into the packed low-bit store on
        // the single-stream decode path only; the batched path would append to
        // the f16 slab without packing, leaving the packed store stale. Refuse
        // rather than read a stale store. (Batched rot decode is a follow-up.)
        if self.kv.cfg.quant.is_rot() {
            return Err(ForgeError::Unsupported(
                "rotational KV (rot4/rot3) supports single-stream decode only; \
                 disable batching for this model"
                    .into(),
            ));
        }
        // MoE routing chooses experts per token from a host readback, so the
        // batched forward cannot be graph-captured; MoE decodes one sequence at
        // a time (batched grouped-GEMM MoE is a tracked follow-up).
        if self.weights.is_moe() {
            return Err(ForgeError::Unsupported(
                "MoE models support single-stream decode only; disable batching".into(),
            ));
        }
        // Batched growth appends pages without refreshing the single-stream
        // page table; invalidate it so the next single-stream step re-uploads.
        self.pt_seq = 0;
        if self.tier.is_some() {
            // Spilled sequences that fit back into free pages are restored
            // (plain fits-check, no reserve: restoring beats streaming when
            // possible); the rest stay streamed and join the batch through
            // the tier staging attention. The balance pass then guarantees a
            // free page per lane's potential boundary growth, spilling the
            // globally coldest prefixes — after it, lane residency is fixed.
            for seq in seqs.iter_mut() {
                if !seq.spilled.is_empty() && seq.spilled_page_count() <= self.kv.free_page_count()
                {
                    self.tier_restore_or_recompute(seq)?;
                }
            }
            self.tier_balance(seqs, b)?;
            for (seq, &tok) in seqs.iter_mut().zip(tokens) {
                seq.tokens.push(tok);
            }
        }
        self.ensure_batch(b)?;
        let p = self.weights.descriptor.params.clone();
        // Streamed lanes (spilled KV) pack at the tail of the lane order: the
        // batch-wide paged attention launch covers exactly the leading
        // resident lanes, and each streamed lane attends over the staging
        // slabs. A mixed batch runs uncaptured at its exact size; a
        // pure-resident batch replays the per-bucket graph (dead lanes
        // padded).
        let mut order: Vec<usize> = (0..b).collect();
        order.sort_by_key(|&i| !seqs[i].spilled.is_empty());
        let resident = seqs.iter().filter(|s| s.spilled.is_empty()).count();
        let mixed = resident < b;
        let bucket = if mixed { b } else { self.bucket_for(b) };
        if b > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "batch {b} exceeds provisioned cap {}",
                self.batch_cap
            )));
        }

        // Reclaim cached prefix pages if the free stack cannot cover a boundary
        // page for every lane (no-op when the prefix cache is inactive/empty).
        self.ensure_free_pages(b);

        // Grow each sequence by one token and gather its position/page table
        // in lane order. Streamed lanes' page tables keep -1 for spilled
        // pages; only the identity-table staging path reads their context.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; bucket * 3]; // [ids | positions | seq_lens]
        let mut pt = vec![-1i32; bucket * mpp];
        for (lane, &i) in order.iter().enumerate() {
            let seq = &mut *seqs[i];
            let pos = seq.len;
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            self.kv.grow(seq)?;
            meta[lane] = tokens[i] as i32;
            meta[bucket + lane] = pos as i32;
            meta[2 * bucket + lane] = (pos + 1) as i32;
            pt[lane * mpp..lane * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        // Dead lanes replay sequence 0's inputs so they compute harmlessly
        // (captured path only; the mixed path runs at its exact size).
        if !mixed {
            let lane0_pt: Vec<i32> = pt[..mpp].to_vec();
            for i in b..bucket {
                meta[i] = meta[0];
                meta[bucket + i] = meta[bucket];
                meta[2 * bucket + i] = meta[2 * bucket];
                pt[i * mpp..i * mpp + mpp].copy_from_slice(&lane0_pt);
            }
        }

        let bb = self.batch_bufs.as_ref().expect("provisioned above");
        // Upload meta (ids/positions/seq_lens) and the page table via pinned H2D.
        let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, bucket * 3 * 4);
        }
        self.device
            .copy(&bb.pinned_meta, 0, &bb.ids, 0, bucket * 4, &self.stream)?;
        self.device.copy(
            &bb.pinned_meta,
            bucket * 4,
            &bb.positions,
            0,
            bucket * 4,
            &self.stream,
        )?;
        self.device.copy(
            &bb.pinned_meta,
            2 * bucket * 4,
            &bb.seq_lens,
            0,
            bucket * 4,
            &self.stream,
        )?;
        let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, bucket * mpp * 4);
        }
        self.device.copy(
            &bb.pinned_pt,
            0,
            &bb.page_table,
            0,
            bucket * mpp * 4,
            &self.stream,
        )?;

        if mixed {
            let tier = self.tier.as_mut().expect("mixed batch requires tiering");
            for &i in &order[resident..] {
                tier.prepare_streaming(seqs[i])?;
            }
            let streamed: Vec<(usize, &SeqKv)> = order[resident..]
                .iter()
                .enumerate()
                .map(|(j, &i)| (resident + j, &*seqs[i]))
                .collect();
            self.record_batch_forward(b, resident, &streamed)?;
        } else {
            // Replay the bucket's forward+logits graph (capture on first use).
            if !self.batch_graphs.contains_key(&bucket) {
                let g = self.capture_batch_forward(bucket)?;
                self.batch_graphs.insert(bucket, g);
            }
            let graph = self.batch_graphs.get(&bucket).expect("captured").clone();
            self.device.launch_graph(&graph, &self.stream)?;
        }

        // Sample the B live rows on the GPU (outside the graph so the per-seq
        // param mix is free), in lane order. Greedy-only batches take the
        // argmax fast path.
        let lane_params: Vec<SeqSampleParams> = order.iter().map(|&i| params[i].clone()).collect();
        self.batch_sample(b, &lane_params)?;

        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device
            .copy(&bb.out_ids, 0, &bb.pinned_out, 0, b * 4, &self.stream)?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(op, b) };
        let mut out = vec![0u32; b];
        for (lane, &i) in order.iter().enumerate() {
            let id = ids[lane];
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "batched sampler returned out-of-range token {id} for seq {i}"
                )));
            }
            out[i] = id as u32;
        }
        Ok(out)
    }

    /// GPU sampling over the `b` live logit rows currently in `batch_bufs`.
    fn batch_sample(&mut self, b: usize, params: &[SeqSampleParams]) -> Result<()> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        let stream = &self.stream;

        // Repetition penalty (skipped when no sequence has one active).
        let any_penalty = params
            .iter()
            .any(|p| p.penalty != 1.0 && !p.penalty_ids.is_empty());
        if any_penalty {
            let mut ids_flat: Vec<i32> = Vec::new();
            let mut offsets: Vec<i32> = Vec::with_capacity(b + 1);
            let mut vals: Vec<f32> = Vec::with_capacity(b);
            offsets.push(0);
            for p in params.iter() {
                if p.penalty != 1.0 {
                    ids_flat.extend_from_slice(&p.penalty_ids);
                }
                offsets.push(ids_flat.len() as i32);
                vals.push(if p.penalty_ids.is_empty() {
                    1.0
                } else {
                    p.penalty
                });
            }
            if ids_flat.len() * 4 > bb.pinned_pen_ids.len() {
                return Err(ForgeError::Scheduler("penalty id staging overflow".into()));
            }
            Self::stage(
                &self.device,
                &bb.pinned_pen_ids,
                &bb.pen_ids,
                bytemuck::cast_slice(&ids_flat),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_offsets,
                &bb.pen_offsets,
                bytemuck::cast_slice(&offsets),
                stream,
            )?;
            Self::stage(
                &self.device,
                &bb.pinned_pen_vals,
                &bb.pen_vals,
                bytemuck::cast_slice(&vals),
                stream,
            )?;
            self.kernels.sample_batched_penalize_f32(
                &bb.logits,
                vocab,
                &bb.pen_ids,
                &bb.pen_offsets,
                &bb.pen_vals,
                b,
                stream,
            )?;
        }

        if params.iter().all(|p| p.greedy) {
            self.kernels
                .sample_batched_argmax_f32(&bb.out_ids, &bb.logits, b, vocab, stream)?;
            return Ok(());
        }

        // Mixed / sampled batch: per-seq top-k (k = 1 lanes reproduce argmax).
        let mut ks = Vec::with_capacity(b);
        let mut inv_t = Vec::with_capacity(b);
        let mut top_p = Vec::with_capacity(b);
        let mut min_p = Vec::with_capacity(b);
        let mut seed = Vec::with_capacity(b);
        let mut step = Vec::with_capacity(b);
        for p in params.iter() {
            ks.push(p.k);
            inv_t.push(p.inv_t);
            top_p.push(p.top_p);
            min_p.push(p.min_p);
            seed.push(p.seed);
            step.push(p.step);
        }
        // Params staged into one pinned block, then copied per array.
        let host = bb.pinned_samp.host_ptr().expect("pinned mapping");
        let mut off = 0usize;
        let put = |bytes: &[u8], off: &mut usize| unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host.add(*off), bytes.len());
            *off += bytes.len();
        };
        put(bytemuck::cast_slice(&ks), &mut off);
        put(bytemuck::cast_slice(&inv_t), &mut off);
        put(bytemuck::cast_slice(&top_p), &mut off);
        put(bytemuck::cast_slice(&min_p), &mut off);
        put(bytemuck::cast_slice(&seed), &mut off);
        put(bytemuck::cast_slice(&step), &mut off);
        let mut o = 0usize;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_k, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_inv_t, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_top_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_min_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_seed, 0, b * 8, stream)?;
        o += b * 8;
        self.device
            .copy(&bb.pinned_samp, o, &bb.samp_step, 0, b * 8, stream)?;
        self.kernels.sample_batched_topk_f32(
            &bb.out_ids,
            &bb.logits,
            b,
            vocab,
            &bb.samp_k,
            &bb.samp_inv_t,
            &bb.samp_top_p,
            &bb.samp_min_p,
            &bb.samp_seed,
            &bb.samp_step,
            stream,
        )
    }

    /// Copy `bytes` into a pinned staging buffer and enqueue the H2D to its
    /// device buffer on `stream`.
    fn stage(
        device: &Arc<dyn Device>,
        pinned: &DevBuffer,
        dev: &DevBuffer,
        bytes: &[u8],
        stream: &Stream,
    ) -> Result<()> {
        let host = pinned.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host, bytes.len());
        }
        device.copy(pinned, 0, dev, 0, bytes.len(), stream)
    }
}

#[cfg(test)]
mod verify_rollback_tests {
    use super::{
        finish_greedy_verification, native_mtp_greedy_decision, ForgeError, KvCache, KvConfig,
        KvQuant, Result, SeqKv,
    };
    use forge_hal::cpu::CpuDevice;

    fn fail_after_kv_growth(
        kv: &mut KvCache,
        page_table_seq: &mut u64,
        seq: &mut SeqKv,
    ) -> Result<()> {
        let base = seq.len;
        let result: Result<(usize, u32)> = (|| {
            for _ in 0..6 {
                kv.grow(seq)?;
            }
            Err(ForgeError::Kernel("wymuszony błąd weryfikacji".into()))
        })();
        finish_greedy_verification(kv, page_table_seq, seq, base, result).map(|_| ())
    }

    #[test]
    fn error_after_kv_growth_rolls_back_every_page() {
        let device = CpuDevice::new();
        let mut kv = KvCache::new(
            device.as_ref(),
            KvConfig {
                n_layers: 1,
                n_kv_heads: 1,
                head_dim: 8,
                page_size: 4,
                n_pages: 8,
                max_pages_per_seq: 8,
                quant: KvQuant::F16,
            },
        )
        .expect("cache KV powinien powstać");
        let mut seq = kv.new_seq();
        for _ in 0..3 {
            kv.grow(&mut seq)
                .expect("wzrost sekwencji powinien się udać");
        }
        let base_pages = seq.pages.len();
        let base_free_pages = kv.free_page_count();
        let mut page_table_seq = 17;

        assert!(fail_after_kv_growth(&mut kv, &mut page_table_seq, &mut seq).is_err());
        assert_eq!(seq.len, 3);
        assert_eq!(seq.pages.len(), base_pages);
        assert_eq!(kv.free_page_count(), base_free_pages);
        assert_eq!(page_table_seq, 0);
    }

    #[test]
    fn native_mtp_acceptance_excludes_fed_and_uses_bonus_row() {
        assert_eq!(native_mtp_greedy_decision(&[11, 12, 13], &[11, 12, 13, 14]), (3, 14));
        assert_eq!(native_mtp_greedy_decision(&[11, 12, 13], &[10, 99, 99, 99]), (0, 10));
        assert_eq!(native_mtp_greedy_decision(&[11, 12, 13], &[11, 20, 99, 99]), (1, 20));
    }
}
