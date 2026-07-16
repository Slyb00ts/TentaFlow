// ===== File: model.rs — single-sequence forward pass (prefill + decode share it) =====
// v0 processes one token per step: correctness and e2e plumbing first; batched
// prefill/decode arrive with the scheduler chunk. The residual stream is
// carried through fused rmsnorm_residual chaining, so no standalone add
// kernel exists: each fusion adds the previous sublayer's output and produces
// the next sublayer's normed input.

use std::path::Path;
use std::sync::Arc;

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_kernels::Kernels;
use forge_types::{ForgeError, MemKind, Result};

use crate::kv::{KvCache, KvConfig, SeqKv};
use crate::weights::{DevWeight, ModelWeights};

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
        Ok(Model {
            device,
            kernels,
            weights,
            kv,
            stream,
            page_table_dev,
            seq_len_dev,
            max_pages_per_seq,
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

    /// Run one token through the model, appending to `seq`, and return the
    /// f32 logits for the next-token distribution.
    pub fn step(&mut self, seq: &mut SeqKv, token_id: u32) -> Result<Vec<f32>> {
        let p = self.weights.descriptor.params.clone();
        let hidden = p.hidden_size;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let inter = p.intermediate_size;
        let eps = p.rms_norm_eps;
        let pos = seq.len;

        if pos >= p.max_position_embeddings {
            return Err(ForgeError::Scheduler(format!(
                "position {pos} exceeds model context {}",
                p.max_position_embeddings
            )));
        }

        self.kv.grow(seq)?;
        let page = seq.pages[pos / self.kv.cfg.page_size];
        let slot = pos % self.kv.cfg.page_size;

        // Refresh the device-side paging state for the attention kernel.
        {
            let mut pt = vec![-1i32; self.max_pages_per_seq];
            pt[..seq.pages.len()].copy_from_slice(&seq.pages);
            self.device
                .write(bytemuck::cast_slice(&pt), &self.page_table_dev, 0)?;
            self.device.write(
                bytemuck::cast_slice(&[(pos + 1) as i32]),
                &self.seq_len_dev,
                0,
            )?;
        }

        let dev = self.device.as_ref();
        let stream = self.stream.clone();
        let alloc = |elems: usize| dev.alloc(elems * 2, MemKind::Device, Pool::Activations);

        // Activation set for this step; dropped before reset_activations.
        let h = alloc(hidden)?;
        let x = alloc(hidden)?;
        let q = alloc(q_dim)?;
        let k = alloc(kv_dim)?;
        let v = alloc(kv_dim)?;
        let attn_out = alloc(q_dim)?;
        let o_out = alloc(hidden)?;
        let gate = alloc(inter)?;
        let up = alloc(inter)?;
        let act = alloc(inter)?;
        let down = alloc(hidden)?;
        let logits_dev = dev.alloc(p.vocab_size * 4, MemKind::Device, Pool::Activations)?;
        let ids = dev.alloc(4, MemKind::Device, Pool::Activations)?;
        let pos_dev = dev.alloc(4, MemKind::Device, Pool::Activations)?;

        dev.write(bytemuck::cast_slice(&[token_id as i32]), &ids, 0)?;
        dev.write(bytemuck::cast_slice(&[pos as i32]), &pos_dev, 0)?;

        let kernels = &self.kernels;
        kernels.gather_rows_f16(&h, &self.weights.token_embd_f16, &ids, 1, hidden, &stream)?;
        kernels.rmsnorm_f16(
            &x,
            &h,
            &self.weights.layers[0].attn_norm,
            1,
            hidden,
            eps,
            &stream,
        )?;

        let scale = 1.0 / (p.head_dim as f32).sqrt();
        let n_layers = self.weights.layers.len();
        for l in 0..n_layers {
            let layer = &self.weights.layers[l];

            self.gemv(&q, &layer.attn_q, &x, &stream)?;
            self.gemv(&k, &layer.attn_k, &x, &stream)?;
            self.gemv(&v, &layer.attn_v, &x, &stream)?;

            // Per-head QK norms (qwen3): rows = heads, cols = head_dim,
            // safe in place because each thread's read/write partitions match.
            if let Some(qn) = &layer.q_norm {
                kernels.rmsnorm_f16(&q, &q, qn, p.n_heads, p.head_dim, eps, &stream)?;
            }
            if let Some(kn) = &layer.k_norm {
                kernels.rmsnorm_f16(&k, &k, kn, p.n_kv_heads, p.head_dim, eps, &stream)?;
            }

            kernels.rope_neox_f16(
                &q,
                &pos_dev,
                1,
                p.n_heads,
                p.head_dim,
                p.rope_theta,
                &stream,
            )?;
            kernels.rope_neox_f16(
                &k,
                &pos_dev,
                1,
                p.n_kv_heads,
                p.head_dim,
                p.rope_theta,
                &stream,
            )?;

            // Scatter this token's K/V rows into the paged cache.
            let row_bytes = p.head_dim * 2;
            for kvh in 0..p.n_kv_heads {
                let dst_off = self.kv.token_offset(page, slot, kvh);
                dev.copy(
                    &k,
                    kvh * row_bytes,
                    &self.kv.k[l],
                    dst_off,
                    row_bytes,
                    &stream,
                )?;
                dev.copy(
                    &v,
                    kvh * row_bytes,
                    &self.kv.v[l],
                    dst_off,
                    row_bytes,
                    &stream,
                )?;
            }

            kernels.attn_decode_f16(
                &attn_out,
                &q,
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
                &stream,
            )?;

            self.gemv(&o_out, &layer.attn_o, &attn_out, &stream)?;
            kernels.rmsnorm_residual_f16(
                &x,
                &h,
                &o_out,
                &layer.ffn_norm,
                1,
                hidden,
                eps,
                &stream,
            )?;

            self.gemv(&gate, &layer.ffn_gate, &x, &stream)?;
            self.gemv(&up, &layer.ffn_up, &x, &stream)?;
            kernels.silu_mul_f16(&act, &gate, &up, inter, &stream)?;
            self.gemv(&down, &layer.ffn_down, &act, &stream)?;

            let next_norm = if l + 1 < n_layers {
                &self.weights.layers[l + 1].attn_norm
            } else {
                &self.weights.output_norm
            };
            kernels.rmsnorm_residual_f16(&x, &h, &down, next_norm, 1, hidden, eps, &stream)?;
        }

        self.logits_gemv(&logits_dev, &x, &stream)?;
        dev.synchronize()?;

        let mut bytes = vec![0u8; p.vocab_size * 4];
        dev.read(&logits_dev, 0, &mut bytes)?;
        let logits: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        drop((
            h, x, q, k, v, attn_out, o_out, gate, up, act, down, logits_dev, ids, pos_dev,
        ));
        self.device.reset_activations()?;

        Ok(logits)
    }
}
