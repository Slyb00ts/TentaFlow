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

use forge_hal::{DevBuffer, Device, ExecGraph, Pool, Stream};
use forge_formats::PoolingType;
use forge_kernels::Kernels;
use forge_types::{DType, ForgeError, MemKind, Result};
use half::f16;

use crate::kv::{KvCache, KvConfig, SeqKv};
use crate::sample::{GpuSampler, SamplingParams, SeqSampleParams};
use crate::weights::{DevWeight, GateUpWeights, ModelWeights, QkvWeights};

/// Largest token count `prefill_chunk` accepts per call; callers split longer
/// prompts. Bounds the persistent prefill scratch allocation.
pub const MAX_PREFILL_CHUNK: usize = 1024;

/// Context splits for decode attention. Splitting shortens each warp's
/// sequential online-softmax chain by this factor (decode runs one block per
/// head — heavily latency-bound; 8 splits cut the attention kernel from
/// ~24 us to ~7 us per layer on RTX 4090), at the cost of a regrouped
/// softmax whose rounding differs slightly from the single-block order.
/// Measured drift vs splits=1 over 16 greedy steps: logit max-abs-diff
/// 0.087 (Bielik NVFP4) / 0.042 (Qwen3 Q8_0) with the argmax identical at
/// every step. 1 reproduces the unsplit arithmetic bit-exactly.
const ATTN_DECODE_SPLITS: usize = 8;

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
    /// KV cache element type: F16 (default, bit-exact canonical path) or
    /// F8E4M3 (halves KV memory + bandwidth; per-value scale-free e4m3 —
    /// its ±448 range with 2^-9 denormals covers post-norm K/V magnitudes).
    /// FP8 requires the fused decode path (validated at load).
    pub kv_dtype: DType,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            kv_page_size: 32,
            kv_pages: 512,
            max_seq_len: 8192,
            kv_dtype: DType::F16,
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
    /// Captured decode step; replayed per token (inputs are device-resident).
    decode_graph: Option<ExecGraph>,
    /// Continuous-batching decode scratch (sized for `batch_cap` sequences),
    /// allocated on the first `ensure_batch`.
    batch_bufs: Option<BatchBufs>,
    /// Per-bucket captured batched forward+logits graphs (bucket = padded
    /// batch size). Replayed for any live batch that rounds up to the bucket.
    batch_graphs: HashMap<usize, ExecGraph>,
    /// Largest batch the scratch is provisioned for (0 until `ensure_batch`).
    batch_cap: usize,
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

impl Model {
    pub fn load_gguf(device: Arc<dyn Device>, path: &Path, cfg: ModelConfig) -> Result<Self> {
        let weights = ModelWeights::load_gguf(&device, path)?;
        Self::finish(device, weights, cfg)
    }

    pub fn load_safetensors_dir(
        device: Arc<dyn Device>,
        dir: &Path,
        cfg: ModelConfig,
    ) -> Result<Self> {
        let weights = ModelWeights::load_safetensors_dir(&device, dir)?;
        Self::finish(device, weights, cfg)
    }

    fn finish(device: Arc<dyn Device>, weights: ModelWeights, cfg: ModelConfig) -> Result<Self> {
        let p = &weights.descriptor.params;
        if p.head_dim != 64 && p.head_dim != 128 {
            return Err(ForgeError::Unsupported(format!(
                "head_dim {} has no attention specialization",
                p.head_dim
            )));
        }
        match cfg.kv_dtype {
            DType::F16 => {}
            DType::F8E4M3 => {
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
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "kv_dtype {other} is not a supported KV cache element type (f16 | f8e4m3)"
                )))
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
                dtype: cfg.kv_dtype,
            },
        )?;
        let stream = device.create_stream()?;
        let page_table_dev = device.alloc(max_pages_per_seq * 4, MemKind::Device, Pool::Weights)?;
        let seq_len_dev = device.alloc(4, MemKind::Device, Pool::Weights)?;
        let hidden = p.hidden_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
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
            attn_parts: device.alloc(
                p.n_heads * ATTN_DECODE_SPLITS * (p.head_dim + 2) * 4,
                MemKind::Device,
                Pool::Activations,
            )?,
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
            pinned_pt: device.alloc(max_pages_per_seq * 4, MemKind::PinnedHost, Pool::Activations)?,
            pinned_logits: device.alloc(p.vocab_size * 4, MemKind::PinnedHost, Pool::Activations)?,
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
            pinned_penalty: device.alloc(cfg.max_seq_len * 4, MemKind::PinnedHost, Pool::Activations)?,
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
            decode_graph: None,
            batch_bufs: None,
            batch_graphs: HashMap::new(),
            batch_cap: 0,
        })
    }

    pub fn new_seq(&self) -> SeqKv {
        self.kv.new_seq()
    }

    pub fn release_seq(&mut self, seq: &mut SeqKv) {
        self.kv.release(seq);
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
                    self.kernels.gemv_q8_0_dp4a_f16(y, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q8_0_f16(y, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q4K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels.gemv_q4_k_dp4a_f16(y, buf, x, *rows, *cols, stream)
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
            DevWeight::Iq4Nl { buf, rows, cols } => {
                self.kernels.gemv_iq4_nl_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq4Xs { buf, rows, cols } => {
                self.kernels.gemv_iq4_xs_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Mxfp4 { buf, rows, cols } => {
                self.kernels.gemv_mxfp4_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xs { buf, rows, cols } => {
                self.kernels.gemv_iq2_xs_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2S { buf, rows, cols } => {
                self.kernels.gemv_iq2_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq3S { buf, rows, cols } => {
                self.kernels.gemv_iq3_s_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq2Xxs { buf, rows, cols } => {
                self.kernels.gemv_iq2_xxs_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Iq3Xxs { buf, rows, cols } => {
                self.kernels.gemv_iq3_xxs_f16(y, buf, x, *rows, *cols, stream)
            }
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
        }
    }

    /// True when `w` can be consumed by the fused decode kernels
    /// (gemv_norm / gemv_norm_silu / gemv_residual format + column coverage).
    fn fused_decode_weight_ok(w: &DevWeight) -> bool {
        match w {
            DevWeight::F16 { cols, .. } => cols.is_multiple_of(8),
            DevWeight::Q8_0 { cols, .. } => cols.is_multiple_of(32),
            DevWeight::NvFp4 { cols, .. } => cols.is_multiple_of(16),
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
            let qkv_ok = match &l.attn_qkv {
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
            let gate_up_ok = match &l.ffn_gate_up {
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
                && Self::fused_decode_weight_ok(&l.attn_o)
                && Self::fused_decode_weight_ok(&l.ffn_down)
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
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_q8_0_dp4a_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
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
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_q6_k_dp4a_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_q5_k_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_q3_k_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_q2_k_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_q4_0_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_q4_1_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_q5_0_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_q5_1_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_iq4_nl_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_iq4_xs_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_mxfp4_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xs_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_iq2_s_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_iq3_s_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq2_xxs_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_iq3_xxs_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_iq1_s_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_iq1_m_f16(
                y, buf, &b.h, &b.h32, norm_w, *rows, *cols, ss_from_h16, eps, stream,
            ),
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
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q8_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q8_0_dp4a_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
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
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q6K { buf, rows, cols } => self.kernels.gemv_norm_silu_q6_k_dp4a_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q5K { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_k_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q3K { buf, rows, cols } => self.kernels.gemv_norm_silu_q3_k_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q2K { buf, rows, cols } => self.kernels.gemv_norm_silu_q2_k_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q4_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_0_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q4_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q4_1_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q5_0 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_0_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Q5_1 { buf, rows, cols } => self.kernels.gemv_norm_silu_q5_1_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq4Nl { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_nl_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq4Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq4_xs_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Mxfp4 { buf, rows, cols } => self.kernels.gemv_norm_silu_mxfp4_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq2Xs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xs_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq2S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_s_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq3S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_s_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq2Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq2_xxs_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq3Xxs { buf, rows, cols } => self.kernels.gemv_norm_silu_iq3_xxs_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq1S { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_s_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
            DevWeight::Iq1M { buf, rows, cols } => self.kernels.gemv_norm_silu_iq1_m_f16(
                act, buf, &b.h, &b.h32, norm_w, rows / 2, *cols, eps, stream,
            ),
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
        }
    }

    fn logits_gemv(&self, y_f32: &DevBuffer, x: &DevBuffer, stream: &Stream) -> Result<()> {
        match &self.weights.lm_head {
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
                    self.kernels.gemv_q4_k_out_f32(y_f32, buf, x, *rows, *cols, stream)
                }
            }
            DevWeight::Q6K { buf, rows, cols } => {
                if *cols <= Kernels::DP4A_MAX_COLS {
                    self.kernels
                        .gemv_q6_k_dp4a_out_f32(y_f32, buf, x, *rows, *cols, stream)
                } else {
                    self.kernels.gemv_q6_k_out_f32(y_f32, buf, x, *rows, *cols, stream)
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
            DevWeight::Q8_0 { buf, cols, .. } => self.kernels.gemm_q8_0_f16_at(
                y,
                buf,
                row_off * (cols / 32) * 34,
                x,
                n_rows,
                *cols,
                n_tokens,
                stream,
            ),
            DevWeight::Q4K { buf, cols, .. } => self.kernels.gemm_q4_k_f16_at(
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

        let mut trace = PrefillTrace::new();
        trace.start(self.device.as_ref());

        kernels.gather_rows_f16(&pb.h, &self.weights.token_embd_f16, &pb.ids, t, hidden, stream)?;
        kernels.rmsnorm_f16(&pb.x, &pb.h, &self.weights.layers[0].attn_norm, t, hidden, eps, stream)?;
        trace.mark(self.device.as_ref(), "embed");

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Prefill outputs must stay [T, dim] contiguous per projection
            // (attention/rope/append index (t*heads+h)*head_dim), so a fused
            // matrix is consumed as three row-window GEMMs into separate
            // buffers — same weight bytes, no second copy in VRAM.
            match &layer.attn_qkv {
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
            trace.mark(self.device.as_ref(), "gemm_qkv");

            if let Some(qn) = &layer.q_norm {
                kernels.rmsnorm_f16(&pb.q, &pb.q, qn, t * p.n_heads, p.head_dim, eps, stream)?;
            }
            if let Some(kn) = &layer.k_norm {
                kernels.rmsnorm_f16(&pb.k, &pb.k, kn, t * p.n_kv_heads, p.head_dim, eps, stream)?;
            }

            kernels.rope_neox_f16(&pb.q, &pb.positions, t, p.n_heads, p.head_dim, p.rope_theta, stream)?;
            kernels.rope_neox_f16(&pb.k, &pb.positions, t, p.n_kv_heads, p.head_dim, p.rope_theta, stream)?;
            trace.mark(self.device.as_ref(), "norm_rope");

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
                self.kv.cfg.dtype,
                stream,
            )?;
            trace.mark(self.device.as_ref(), "kv_append");
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
                self.kv.cfg.dtype,
                scale,
                stream,
            )?;
            trace.mark(self.device.as_ref(), "attn");

            self.gemm(&pb.o_out, &layer.attn_o, &pb.attn_out, t, stream)?;
            trace.mark(self.device.as_ref(), "gemm_o");
            kernels.rmsnorm_residual_f16(&pb.x, &pb.h, &pb.o_out, &layer.ffn_norm, t, hidden, eps, stream)?;
            trace.mark(self.device.as_ref(), "norm_res");

            match &layer.ffn_gate_up {
                GateUpWeights::Fused(w) => {
                    self.gemm_rows(&pb.gate, w, &pb.x, t, 0, inter, stream)?;
                    self.gemm_rows(&pb.up, w, &pb.x, t, inter, inter, stream)?;
                }
                GateUpWeights::Split { gate, up } => {
                    self.gemm(&pb.gate, gate, &pb.x, t, stream)?;
                    self.gemm(&pb.up, up, &pb.x, t, stream)?;
                }
            }
            trace.mark(self.device.as_ref(), "gemm_gateup");
            kernels.silu_mul_f16(&pb.act, &pb.gate, &pb.up, t * inter, stream)?;
            trace.mark(self.device.as_ref(), "silu");
            self.gemm(&pb.down, &layer.ffn_down, &pb.act, t, stream)?;
            trace.mark(self.device.as_ref(), "gemm_down");

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&pb.x, &pb.h, &pb.down, next_norm, t, hidden, eps, stream)?;
            trace.mark(self.device.as_ref(), "norm_res2");
        }

        // The stream is drained here so callers can read the hidden states or
        // launch the logits/pool tail without an additional sync of their own.
        self.device.synchronize()?;
        trace.report(t);
        Ok(t)
    }

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through the model in one
    /// batched pass, appending to `seq`, and return the last token's logits.
    /// Not graph-captured: T varies per call and prefill launches are large
    /// enough that launch overhead is immaterial.
    pub fn prefill_chunk(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
        let t = self.prefill_forward(seq, tokens)?;
        let hidden = self.weights.descriptor.params.hidden_size;
        let vocab = self.weights.descriptor.params.vocab_size;
        let stream = &self.stream;
        let pb = self.prefill_bufs.as_ref().expect("prefill_forward allocated");
        // Only the last token's logits matter; route its hidden state through
        // the decode logits path (same GEMV + pinned landing).
        self.device
            .copy(&pb.x, (t - 1) * hidden * 2, &self.bufs.x, 0, hidden * 2, stream)?;
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, stream)?;
        self.device
            .copy(&self.bufs.logits, 0, &self.bufs.pinned_logits, 0, vocab * 4, stream)?;
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
            let pb = self.prefill_bufs.as_ref().expect("prefill_forward allocated");
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

        let page_boundary = seq.len.is_multiple_of(self.kv.cfg.page_size);
        self.kv.grow(seq)?;

        // Stage [token, pos, seq_len] in pinned memory and push them with
        // async copies on the compute stream — pinned H2D avoids the pageable
        // legacy-stream drain that plain write() must perform.
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
        self.device
            .copy(&self.bufs.pinned_in, 8, &self.seq_len_dev, 0, 4, &self.stream)?;

        // The page table only changes when a page is appended.
        if page_boundary {
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
        }

        if self.decode_graph.is_none() {
            let graph = self.capture_step()?;
            self.decode_graph = Some(graph);
        }
        let graph = self.decode_graph.as_ref().expect("captured above").clone();
        self.device.launch_graph(&graph, &self.stream)
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

    /// Record every launch of one decode step into a replayable graph.
    /// Stream capture does not execute the work, so buffer contents during
    /// capture are irrelevant — only addresses and launch geometry matter.
    fn capture_step(&self) -> Result<ExecGraph> {
        let p = &self.weights.descriptor.params;
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let kernels = &self.kernels;
        let stream = &self.stream;
        let b = &self.bufs;

        self.device.begin_capture(stream)?;
        let record = || -> Result<()> {
            if Self::fused_decode_supported(&self.weights) {
                return self.record_step_fused();
            }
            kernels.gather_rows_f16(&b.h, &self.weights.token_embd_f16, &b.ids, 1, hidden, stream)?;
            kernels.rmsnorm_f16(&b.x, &b.h, &self.weights.layers[0].attn_norm, 1, hidden, eps, stream)?;

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
                let q_buf = match &layer.attn_qkv {
                    QkvWeights::Fused(w) => {
                        self.gemv(&b.qkv, w, &b.x, stream)?;
                        kernels.qkv_post_f16(
                            &b.qkv,
                            0,
                            &b.qkv,
                            k_byte_off,
                            &b.qkv,
                            v_byte_off,
                            layer.q_norm.as_ref(),
                            layer.k_norm.as_ref(),
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
                            layer.q_norm.as_ref(),
                            layer.k_norm.as_ref(),
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
                        if let Some(qn) = &layer.q_norm {
                            kernels.rmsnorm_f16(&b.q, &b.q, qn, p.n_heads, p.head_dim, eps, stream)?;
                        }
                        if let Some(kn) = &layer.k_norm {
                            kernels.rmsnorm_f16(&b.k, &b.k, kn, p.n_kv_heads, p.head_dim, eps, stream)?;
                        }
                        kernels.rope_neox_f16(&b.q, &b.pos, 1, p.n_heads, p.head_dim, p.rope_theta, stream)?;
                        kernels.rope_neox_f16(&b.k, &b.pos, 1, p.n_kv_heads, p.head_dim, p.rope_theta, stream)?;
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

                self.gemv(&b.o_out, &layer.attn_o, &b.attn_out, stream)?;
                kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.o_out, &layer.ffn_norm, 1, hidden, eps, stream)?;

                match &layer.ffn_gate_up {
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
                self.gemv(&b.down, &layer.ffn_down, &b.act, stream)?;

                let next_norm = if l + 1 < n_layers {
                    &self.weights.layers[l + 1].attn_norm
                } else {
                    &self.weights.output_norm
                };
                kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.down, next_norm, 1, hidden, eps, stream)?;
            }

            self.logits_gemv(&b.logits, &b.x, stream)
        };
        let recorded = record();
        match recorded {
            Ok(()) => self.device.end_capture(stream),
            Err(e) => {
                // Abort the capture so the stream is usable again.
                let _ = self.device.end_capture(stream);
                Err(e)
            }
        }
    }

    /// Fused decode step: six launches per layer instead of nine. The
    /// residual stream is carried as the (h f16, h32 f32) pair — every
    /// norm-consuming kernel recomputes the RMSNorm per block from that pair
    /// (bit-identical to the separate rmsnorm kernels, see decode_fused.mojo)
    /// and attn_decode_split folds the whole qkv_post stage into the
    /// attention prologue (the split/combine pair fills the GPU where one
    /// block per head could not). Layer 0 sums squares from h directly (h32
    /// is only materialized by the first gemv_residual of the step).
    fn record_step_fused(&self) -> Result<()> {
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

        kernels.gather_rows_f16(&b.h, &self.weights.token_embd_f16, &b.ids, 1, hidden, stream)?;

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            // Fused QKV projects with one gemv_norm into the fused buffer;
            // split layers (mixed formats) run one gemv_norm per projection —
            // per-row math is identical, only the block-level norm recompute
            // repeats. Both feed attn_decode_split via buffer + byte offset.
            let (q_buf, q_off, k_buf, k_off, v_buf, v_off) = match &layer.attn_qkv {
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
            kernels.attn_decode_split(
                &b.attn_parts,
                q_buf,
                q_off,
                k_buf,
                k_off,
                v_buf,
                v_off,
                layer.q_norm.as_ref(),
                layer.k_norm.as_ref(),
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
                ATTN_DECODE_SPLITS,
                self.kv.cfg.dtype,
                eps,
                p.rope_theta,
                scale,
                stream,
            )?;
            kernels.attn_decode_combine_f16(
                &b.attn_out,
                &b.attn_parts,
                1,
                p.n_heads,
                p.head_dim,
                ATTN_DECODE_SPLITS,
                stream,
            )?;
            self.gemv_residual(&layer.attn_o, &b.attn_out, stream)?;
            match &layer.ffn_gate_up {
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
            self.gemv_residual(&layer.ffn_down, &b.act, stream)?;
        }

        kernels.rmsnorm_h32_f16(&b.x, &b.h, &b.h32, &self.weights.output_norm, 1, hidden, eps, stream)?;
        self.logits_gemv(&b.logits, &b.x, stream)
    }

    /// Batched logit head: y[b, vocab] f32 = lm_head · x[b, hidden]. The head
    /// is always f16 or Q8_0 (NVFP4 heads are materialized as f16 at load), so
    /// the two batched f32-output GEMMs cover every model.
    fn logits_gemm(&self, y_f32: &DevBuffer, x: &DevBuffer, n_tokens: usize, stream: &Stream) -> Result<()> {
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
    /// per-sequence paged flash-decode (attn_decode_split over n_seqs=n).
    fn record_batch_forward(&self, n: usize) -> Result<()> {
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

        kernels.gather_rows_f16(&bb.h, &self.weights.token_embd_f16, &bb.ids, n, hidden, stream)?;
        kernels.rmsnorm_f16(&bb.x, &bb.h, &self.weights.layers[0].attn_norm, n, hidden, eps, stream)?;

        for l in 0..n_layers {
            let layer = &self.weights.layers[l];
            // Raw q/k/v projections (no norm/rope here — attn_decode_split folds
            // the q/k-norm + RoPE + paged append into its per-seq prologue).
            match &layer.attn_qkv {
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
            kernels.attn_decode_split(
                &bb.attn_parts,
                &bb.q,
                0,
                &bb.k,
                0,
                &bb.v,
                0,
                layer.q_norm.as_ref(),
                layer.k_norm.as_ref(),
                &self.kv.k[l],
                &self.kv.v[l],
                &bb.page_table,
                &bb.seq_lens,
                &bb.positions,
                n,
                p.n_heads,
                p.n_kv_heads,
                p.head_dim,
                self.kv.cfg.page_size,
                self.max_pages_per_seq,
                ATTN_DECODE_SPLITS,
                self.kv.cfg.dtype,
                eps,
                p.rope_theta,
                scale,
                stream,
            )?;
            kernels.attn_decode_combine_f16(
                &bb.attn_out,
                &bb.attn_parts,
                n,
                p.n_heads,
                p.head_dim,
                ATTN_DECODE_SPLITS,
                stream,
            )?;
            self.gemm(&bb.o_out, &layer.attn_o, &bb.attn_out, n, stream)?;
            kernels.rmsnorm_residual_f16(&bb.x, &bb.h, &bb.o_out, &layer.ffn_norm, n, hidden, eps, stream)?;

            match &layer.ffn_gate_up {
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
            self.gemm(&bb.down, &layer.ffn_down, &bb.act, n, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&bb.x, &bb.h, &bb.down, next_norm, n, hidden, eps, stream)?;
        }

        self.logits_gemm(&bb.logits, &bb.x, n, stream)
    }

    /// Capture `record_batch_forward(bucket)` into a replayable graph.
    fn capture_batch_forward(&self, bucket: usize) -> Result<ExecGraph> {
        self.device.begin_capture(&self.stream)?;
        match self.record_batch_forward(bucket) {
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
        self.ensure_batch(b)?;
        let p = self.weights.descriptor.params.clone();
        let bucket = self.bucket_for(b);
        if b > self.batch_cap {
            return Err(ForgeError::Scheduler(format!(
                "batch {b} exceeds provisioned cap {}",
                self.batch_cap
            )));
        }

        // Grow each sequence by one token and gather its position/page table.
        let mpp = self.max_pages_per_seq;
        let mut meta = vec![0i32; bucket * 3]; // [ids | positions | seq_lens]
        let mut pt = vec![-1i32; bucket * mpp];
        for (i, seq) in seqs.iter_mut().enumerate() {
            let pos = seq.len;
            if pos >= p.max_position_embeddings {
                return Err(ForgeError::Scheduler(format!(
                    "position {pos} exceeds model context {}",
                    p.max_position_embeddings
                )));
            }
            self.kv.grow(seq)?;
            meta[i] = tokens[i] as i32;
            meta[bucket + i] = pos as i32;
            meta[2 * bucket + i] = (pos + 1) as i32;
            pt[i * mpp..i * mpp + seq.pages.len()].copy_from_slice(&seq.pages);
        }
        // Dead lanes replay sequence 0's inputs so they compute harmlessly.
        let lane0_pt: Vec<i32> = pt[..mpp].to_vec();
        for i in b..bucket {
            meta[i] = meta[0];
            meta[bucket + i] = meta[bucket];
            meta[2 * bucket + i] = meta[2 * bucket];
            pt[i * mpp..i * mpp + mpp].copy_from_slice(&lane0_pt);
        }

        let bb = self.batch_bufs.as_ref().expect("provisioned above");
        // Upload meta (ids/positions/seq_lens) and the page table via pinned H2D.
        let meta_host = bb.pinned_meta.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(meta.as_ptr() as *const u8, meta_host, bucket * 3 * 4);
        }
        self.device.copy(&bb.pinned_meta, 0, &bb.ids, 0, bucket * 4, &self.stream)?;
        self.device
            .copy(&bb.pinned_meta, bucket * 4, &bb.positions, 0, bucket * 4, &self.stream)?;
        self.device
            .copy(&bb.pinned_meta, 2 * bucket * 4, &bb.seq_lens, 0, bucket * 4, &self.stream)?;
        let pt_host = bb.pinned_pt.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(pt.as_ptr() as *const u8, pt_host, bucket * mpp * 4);
        }
        self.device
            .copy(&bb.pinned_pt, 0, &bb.page_table, 0, bucket * mpp * 4, &self.stream)?;

        // Replay the bucket's forward+logits graph (capture on first use).
        if !self.batch_graphs.contains_key(&bucket) {
            let g = self.capture_batch_forward(bucket)?;
            self.batch_graphs.insert(bucket, g);
        }
        let graph = self.batch_graphs.get(&bucket).expect("captured").clone();
        self.device.launch_graph(&graph, &self.stream)?;

        // Sample the B live rows on the GPU (outside the graph so the per-seq
        // param mix is free). Greedy-only batches take the argmax fast path.
        self.batch_sample(b, params)?;

        let bb = self.batch_bufs.as_ref().expect("provisioned");
        self.device.copy(&bb.out_ids, 0, &bb.pinned_out, 0, b * 4, &self.stream)?;
        self.device.synchronize()?;
        let op = bb.pinned_out.host_ptr().expect("pinned mapping") as *const i32;
        let ids = unsafe { std::slice::from_raw_parts(op, b) };
        let mut out = Vec::with_capacity(b);
        for (i, &id) in ids.iter().enumerate() {
            if id < 0 || id as usize >= p.vocab_size {
                return Err(ForgeError::Kernel(format!(
                    "batched sampler returned out-of-range token {id} for seq {i}"
                )));
            }
            out.push(id as u32);
        }
        Ok(out)
    }

    /// GPU sampling over the `b` live logit rows currently in `batch_bufs`.
    fn batch_sample(&mut self, b: usize, params: &[SeqSampleParams]) -> Result<()> {
        let vocab = self.weights.descriptor.params.vocab_size;
        let bb = self.batch_bufs.as_ref().expect("provisioned");
        let stream = &self.stream;

        // Repetition penalty (skipped when no sequence has one active).
        let any_penalty = params.iter().any(|p| p.penalty != 1.0 && !p.penalty_ids.is_empty());
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
                vals.push(if p.penalty_ids.is_empty() { 1.0 } else { p.penalty });
            }
            if ids_flat.len() * 4 > bb.pinned_pen_ids.len() {
                return Err(ForgeError::Scheduler("penalty id staging overflow".into()));
            }
            Self::stage(&self.device, &bb.pinned_pen_ids, &bb.pen_ids, bytemuck::cast_slice(&ids_flat), stream)?;
            Self::stage(&self.device, &bb.pinned_pen_offsets, &bb.pen_offsets, bytemuck::cast_slice(&offsets), stream)?;
            Self::stage(&self.device, &bb.pinned_pen_vals, &bb.pen_vals, bytemuck::cast_slice(&vals), stream)?;
            self.kernels
                .sample_batched_penalize_f32(&bb.logits, vocab, &bb.pen_ids, &bb.pen_offsets, &bb.pen_vals, b, stream)?;
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
        self.device.copy(&bb.pinned_samp, o, &bb.samp_k, 0, b * 4, stream)?;
        o += b * 4;
        self.device.copy(&bb.pinned_samp, o, &bb.samp_inv_t, 0, b * 4, stream)?;
        o += b * 4;
        self.device.copy(&bb.pinned_samp, o, &bb.samp_top_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device.copy(&bb.pinned_samp, o, &bb.samp_min_p, 0, b * 4, stream)?;
        o += b * 4;
        self.device.copy(&bb.pinned_samp, o, &bb.samp_seed, 0, b * 8, stream)?;
        o += b * 8;
        self.device.copy(&bb.pinned_samp, o, &bb.samp_step, 0, b * 8, stream)?;
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
    fn stage(device: &Arc<dyn Device>, pinned: &DevBuffer, dev: &DevBuffer, bytes: &[u8], stream: &Stream) -> Result<()> {
        let host = pinned.host_ptr().expect("pinned mapping");
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), host, bytes.len());
        }
        device.copy(pinned, 0, dev, 0, bytes.len(), stream)
    }
}
