// ===== File: model.rs — single-sequence forward pass (batched prefill + graphed decode) =====
// Decode runs one token per step through a captured CUDA graph; prefill runs
// whole prompt chunks through batched GEMM/attention kernels (same math, T
// tokens at once). The residual stream is carried through fused
// rmsnorm_residual chaining, so no standalone add kernel exists: each fusion
// adds the previous sublayer's output and produces the next sublayer's
// normed input.

use std::path::Path;
use std::sync::Arc;

use forge_hal::{DevBuffer, Device, ExecGraph, Pool, Stream};
use forge_kernels::Kernels;
use forge_types::{ForgeError, MemKind, Result};

use crate::kv::{KvCache, KvConfig, SeqKv};
use crate::weights::{DevWeight, ModelWeights};

/// Largest token count `prefill_chunk` accepts per call; callers split longer
/// prompts. Bounds the persistent prefill scratch allocation.
pub const MAX_PREFILL_CHUNK: usize = 256;

pub struct ModelConfig {
    pub kv_page_size: usize,
    pub kv_pages: usize,
    pub max_seq_len: usize,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            kv_page_size: 32,
            kv_pages: 512,
            max_seq_len: 8192,
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
}

/// Persistent per-step activation buffers. Fixed addresses are what makes the
/// decode step CUDA-graph-replayable: only their contents change per token.
struct DecodeBufs {
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
    logits: DevBuffer,
    ids: DevBuffer,
    pos: DevBuffer,
    /// Pinned-host staging: [token_id, pos, seq_len] i32 triple.
    pinned_in: DevBuffer,
    /// Pinned-host mirror of the page table (async H2D on page boundary).
    pinned_pt: DevBuffer,
    /// Pinned-host landing buffer for logits (avoids pageable D2H).
    pinned_logits: DevBuffer,
}

/// Persistent prefill scratch sized for MAX_PREFILL_CHUNK tokens. Activation
/// matrices are [T, cols] row-major except `xt`, which holds the transposed
/// [cols, T_pad] view the batched GEMMs consume. Rows T..T_pad of `xt` (and of
/// the GEMM outputs) carry garbage by design: the prefill GEMM lanes read 4
/// tokens per vector, so the token stride is padded to a multiple of 4 and
/// outputs beyond the real token count are never read.
struct PrefillBufs {
    h: DevBuffer,
    x: DevBuffer,
    xt: DevBuffer,
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
            x: alloc(hidden)?,
            q: alloc(q_dim)?,
            k: alloc(kv_dim)?,
            v: alloc(kv_dim)?,
            attn_out: alloc(q_dim)?,
            o_out: alloc(hidden)?,
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
        })
    }

    pub fn new_seq(&self) -> SeqKv {
        self.kv.new_seq()
    }

    pub fn release_seq(&mut self, seq: &mut SeqKv) {
        self.kv.release(seq);
    }

    fn gemv(&self, y: &DevBuffer, w: &DevWeight, x: &DevBuffer, stream: &Stream) -> Result<()> {
        match w {
            DevWeight::F16 { buf, rows, cols } => {
                self.kernels.gemv_f16(y, buf, x, *rows, *cols, stream)
            }
            DevWeight::Q8_0 { buf, rows, cols } => {
                self.kernels.gemv_q8_0_f16(y, buf, x, *rows, *cols, stream)
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

    fn logits_gemv(&self, y_f32: &DevBuffer, x: &DevBuffer, stream: &Stream) -> Result<()> {
        match &self.weights.lm_head {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemv_f16_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemv_q8_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
            DevWeight::NvFp4 { .. } => Err(ForgeError::Unsupported(
                "NVFP4 lm_head has no f32-logit kernel yet".into(),
            )),
        }
    }

    /// Batched GEMM over transposed activations; `n_tokens` is the padded
    /// token stride (multiple of 4).
    fn gemm_xt(
        &self,
        y: &DevBuffer,
        w: &DevWeight,
        xt: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        match w {
            DevWeight::F16 { buf, rows, cols } => self
                .kernels
                .gemm_f16_xt_f16(y, buf, xt, *rows, *cols, n_tokens, stream),
            DevWeight::Q8_0 { buf, rows, cols } => self
                .kernels
                .gemm_q8_0_xt_f16(y, buf, xt, *rows, *cols, n_tokens, stream),
            DevWeight::NvFp4 {
                packed,
                scales,
                inv_global_scale,
                rows,
                cols,
            } => self.kernels.gemm_nvfp4_xt_f16(
                y,
                packed,
                scales,
                xt,
                *rows,
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
            xt: alloc(t_max * hidden.max(q_dim).max(inter))?,
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

    /// Run a prompt chunk (≤ MAX_PREFILL_CHUNK tokens) through the model in
    /// one batched pass, appending to `seq`, and return the last token's
    /// logits. Not graph-captured: T varies per call and prefill launches are
    /// large enough that launch overhead is immaterial.
    pub fn prefill_chunk(&mut self, seq: &mut SeqKv, tokens: &[u32]) -> Result<Vec<f32>> {
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

        // Padded token stride for the transposed activations (see PrefillBufs).
        let t_pad = t.next_multiple_of(4);
        let hidden = p.hidden_size;
        let inter = p.intermediate_size;
        let q_dim = p.n_heads * p.head_dim;
        let eps = p.rms_norm_eps;
        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let kernels = &self.kernels;
        let stream = &self.stream;

        kernels.gather_rows_f16(&pb.h, &self.weights.token_embd_f16, &pb.ids, t, hidden, stream)?;
        kernels.rmsnorm_f16(&pb.x, &pb.h, &self.weights.layers[0].attn_norm, t, hidden, eps, stream)?;

        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            kernels.transpose_f16(&pb.xt, &pb.x, t_pad, hidden, stream)?;
            self.gemm_xt(&pb.q, &layer.attn_q, &pb.xt, t_pad, stream)?;
            self.gemm_xt(&pb.k, &layer.attn_k, &pb.xt, t_pad, stream)?;
            self.gemm_xt(&pb.v, &layer.attn_v, &pb.xt, t_pad, stream)?;

            if let Some(qn) = &layer.q_norm {
                kernels.rmsnorm_f16(&pb.q, &pb.q, qn, t * p.n_heads, p.head_dim, eps, stream)?;
            }
            if let Some(kn) = &layer.k_norm {
                kernels.rmsnorm_f16(&pb.k, &pb.k, kn, t * p.n_kv_heads, p.head_dim, eps, stream)?;
            }

            kernels.rope_neox_f16(&pb.q, &pb.positions, t, p.n_heads, p.head_dim, p.rope_theta, stream)?;
            kernels.rope_neox_f16(&pb.k, &pb.positions, t, p.n_kv_heads, p.head_dim, p.rope_theta, stream)?;

            // Causal attention reads the chunk's own K/V from the cache, so
            // the batch append must land before the attention launch.
            kernels.kv_append_batch_f16(
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
                stream,
            )?;
            kernels.attn_prefill_f16(
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
                scale,
                stream,
            )?;

            kernels.transpose_f16(&pb.xt, &pb.attn_out, t_pad, q_dim, stream)?;
            self.gemm_xt(&pb.o_out, &layer.attn_o, &pb.xt, t_pad, stream)?;
            kernels.rmsnorm_residual_f16(&pb.x, &pb.h, &pb.o_out, &layer.ffn_norm, t, hidden, eps, stream)?;

            kernels.transpose_f16(&pb.xt, &pb.x, t_pad, hidden, stream)?;
            self.gemm_xt(&pb.gate, &layer.ffn_gate, &pb.xt, t_pad, stream)?;
            self.gemm_xt(&pb.up, &layer.ffn_up, &pb.xt, t_pad, stream)?;
            kernels.silu_mul_f16(&pb.act, &pb.gate, &pb.up, t * inter, stream)?;
            kernels.transpose_f16(&pb.xt, &pb.act, t_pad, inter, stream)?;
            self.gemm_xt(&pb.down, &layer.ffn_down, &pb.xt, t_pad, stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&pb.x, &pb.h, &pb.down, next_norm, t, hidden, eps, stream)?;
        }

        // Only the last token's logits matter; route its hidden state through
        // the decode logits path (same GEMV + pinned landing).
        self.device
            .copy(&pb.x, (t - 1) * hidden * 2, &self.bufs.x, 0, hidden * 2, stream)?;
        self.logits_gemv(&self.bufs.logits, &self.bufs.x, stream)?;
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            p.vocab_size * 4,
            stream,
        )?;
        self.device.synchronize()?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, p.vocab_size) }.to_vec();
        Ok(logits)
    }

    /// Run one token through the model, appending to `seq`, and return the
    /// f32 logits for the next-token distribution.
    pub fn step(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<Vec<f32>> {
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
        self.device.launch_graph(&graph, &self.stream)?;
        // Land logits in pinned memory on the same stream, then one sync.
        self.device.copy(
            &self.bufs.logits,
            0,
            &self.bufs.pinned_logits,
            0,
            p.vocab_size * 4,
            &self.stream,
        )?;
        self.device.synchronize()?;

        let lp = self
            .bufs
            .pinned_logits
            .host_ptr()
            .expect("pinned buffer has host mapping") as *const f32;
        let logits = unsafe { std::slice::from_raw_parts(lp, p.vocab_size) }.to_vec();

        Ok(logits)
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
            kernels.gather_rows_f16(&b.h, &self.weights.token_embd_f16, &b.ids, 1, hidden, stream)?;
            kernels.rmsnorm_f16(&b.x, &b.h, &self.weights.layers[0].attn_norm, 1, hidden, eps, stream)?;

            let scale = 1.0 / (p.head_dim as f32).sqrt();
            let n_layers = self.weights.layers.len();
            for l in 0..n_layers {
                let layer = &self.weights.layers[l];

                self.gemv(&b.q, &layer.attn_q, &b.x, stream)?;
                self.gemv(&b.k, &layer.attn_k, &b.x, stream)?;
                self.gemv(&b.v, &layer.attn_v, &b.x, stream)?;

                // Per-head QK norms (qwen3); in-place is safe: each thread's
                // read/write partitions match.
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

                self.gemv(&b.o_out, &layer.attn_o, &b.attn_out, stream)?;
                kernels.rmsnorm_residual_f16(&b.x, &b.h, &b.o_out, &layer.ffn_norm, 1, hidden, eps, stream)?;

                self.gemv(&b.gate, &layer.ffn_gate, &b.x, stream)?;
                self.gemv(&b.up, &layer.ffn_up, &b.x, stream)?;
                kernels.silu_mul_f16(&b.act, &b.gate, &b.up, inter, stream)?;
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
}
