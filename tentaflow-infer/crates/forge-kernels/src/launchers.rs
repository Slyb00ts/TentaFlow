// ===== File: launchers.rs — typed launch wrappers over kernel artifacts =====
// Argument order and meaning must mirror the Mojo kernel signatures exactly
// (kernels/mojo/src/*.mojo). Mojo `Int` marshals as a 64-bit scalar slot,
// `Float32` as f32.

use std::sync::Arc;

use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Stream};
use forge_types::{ForgeError, Result};

use crate::registry::KernelArtifacts;

const BLOCK: u32 = 256;
/// Warps per block in attn_decode (must not exceed MAX_WARPS in attention.mojo).
const ATTN_BLOCK: u32 = 128;

pub struct Kernels {
    device: Arc<dyn Device>,
    artifacts: KernelArtifacts,
}

impl Kernels {
    pub fn load(device: Arc<dyn Device>) -> Result<Self> {
        let artifacts = KernelArtifacts::load(device.as_ref())?;
        Ok(Self { device, artifacts })
    }

    pub fn artifacts(&self) -> &KernelArtifacts {
        &self.artifacts
    }

    /// out[row] = rmsnorm(x[row]) * weight, f16, one block per row.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = rmsnorm(residual) * weight (fused, f16).
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = silu(gate) * up over n f16 elements.
    pub fn silu_mul_f16(
        &self,
        out: &DevBuffer,
        gate: &DevBuffer,
        up: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("silu_mul_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(gate)
            .buf(up)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `silu_mul_f16` where gate and up are sections of one fused gate|up
    /// buffer, addressed by byte offsets.
    #[allow(clippy::too_many_arguments)]
    pub fn silu_mul_f16_at(
        &self,
        out: &DevBuffer,
        gate_up: &DevBuffer,
        gate_byte_off: usize,
        up_byte_off: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("silu_mul_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(gate_up, gate_byte_off)?
            .buf_at(gate_up, up_byte_off)?
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// In-place neox RoPE over [n_tokens, n_heads, head_dim] f16.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rope_neox_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((head_dim as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(x_io)
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q8_0 blocks, x/y f16. One block per output row.
    pub fn gemv_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q8_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x, all f16. One block per output row.
    pub fn gemv_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in NVFP4 (compressed-tensors) packed layout.
    /// `inv_global_scale` = 1 / weight_global_scale.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4 requires cols % 16 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_nvfp4_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(packed)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[t] = table[ids[t]] — token embedding gather (f16 rows).
    pub fn gather_rows_f16(
        &self,
        out: &DevBuffer,
        table: &DevBuffer,
        ids: &DevBuffer,
        n_tokens: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gather_rows_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK.min(cols as u32).max(32), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(table)
            .buf(ids)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV, f16 weights → f32 logits.
    pub fn gemv_f16_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q8_0 weights (tied embeddings) → f32 logits.
    pub fn gemv_q8_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q8_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q8_0_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[row] = layernorm(x[row]) * weight + bias.
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = layernorm(residual) * weight + bias (fused).
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Elementwise GELU (exact erf) over n f16 elements.
    pub fn gelu_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gelu_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// 1-D conv (kernel 3, pad 1) with fused optional GELU.
    /// x: [in_ch, in_t]; weight: [out_ch, in_ch, 3]; out: [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_k3_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        in_ch: usize,
        out_ch: usize,
        in_t: usize,
        out_t: usize,
        stride: usize,
        apply_gelu: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_k3_f16")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(128), out_ch as u32, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_t as i64)
            .scalar(stride as i64)
            .scalar(if apply_gelu { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Full (non-paged) attention over contiguous K/V; causal optional.
    /// q/out: [n_q, n_q_heads, hd]; k/v: [n_kv, n_kv_heads, hd].
    #[allow(clippy::too_many_arguments)]
    pub fn attn_full_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_buf: &DevBuffer,
        v_buf: &DevBuffer,
        n_q: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        n_kv: usize,
        causal: bool,
        q_offset: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_full_f16_hd64",
            128 => "attn_full_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_full: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_q as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_buf)
            .buf(v_buf)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(n_kv as i64)
            .scalar(if causal { 1i64 } else { 0i64 })
            .scalar(q_offset as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_f16` reading x at `x_byte_off` and writing y at `y_byte_off`.
    /// Sequence-shaped callers (Whisper encoder) launch one GEMV per position
    /// over the same stream instead of staging per-position copies.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_at(
        &self,
        y: &DevBuffer,
        y_byte_off: usize,
        w: &DevBuffer,
        x: &DevBuffer,
        x_byte_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_byte_off)?
            .buf(w)
            .buf_at(x, x_byte_off)?
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `gemv_f16_bias` reading x at `x_byte_off` and writing y at `y_byte_off`.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_bias_at(
        &self,
        y: &DevBuffer,
        y_byte_off: usize,
        w: &DevBuffer,
        x: &DevBuffer,
        x_byte_off: usize,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_bias")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_byte_off)?
            .buf(w)
            .buf_at(x, x_byte_off)?
            .buf(bias)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// f16 GEMV with per-row bias: y = W·x + b.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_f16_bias(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gemv_f16_bias")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .buf(bias)
            .scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Scatter the current token's K/V rows ([n_kv_heads, head_dim]) into the
    /// paged cache at position seq_len[0]-1 (device-resident addressing —
    /// CUDA-graph-replay safe).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_f16")?;
        let cfg = LaunchConfig {
            grid: (n_kv_heads as u32, 1, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Pick the prefill GEMM tile for a (rows, n_tokens) shape. The BM=64
    /// instantiation doubles the token-block count, which wins everywhere
    /// except very tall matrices at short chunks where the BM=128 grid is
    /// already saturated (measured on RTX 4090, kernels/mojo benches).
    fn gemm_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if rows >= 8192 && n_tokens <= 256 {
            ("", 256, 128)
        } else {
            ("_bm64", 128, 64)
        }
    }

    /// Y[t, row] = W·x[t] over Q8_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q8_0_f16_at(y, w_q8, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q8_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16_at(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q8_0_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over NVFP4 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_nvfp4_f16_at(
            y,
            packed,
            0,
            scales,
            0,
            x,
            rows,
            cols,
            n_tokens,
            inv_global_scale,
            stream,
        )
    }

    /// `gemm_nvfp4_f16` over a row window of a fused weight matrix; packed
    /// nibbles and FP8 block scales are separate streams, so the window needs
    /// a byte offset into each.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_f16_at(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        packed_byte_off: usize,
        scales: &DevBuffer,
        scales_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4 requires cols % 16 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_nvfp4_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(packed, packed_byte_off)?
            .buf_at(scales, scales_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t], all f16, row-major activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_f16` over a row window of a fused weight matrix. The kernel's
    /// 16-byte weight loads require `w_byte_off % 16 == 0`, which
    /// row-aligned offsets satisfy for any cols % 8 == 0.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16_at(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        // The kernel consumes the reduction dim in vectors of 8; a tail would
        // be silently dropped, so reject it loudly instead.
        if !cols.is_multiple_of(8) {
            return Err(ForgeError::Kernel(format!(
                "gemm_f16 requires cols % 8 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused decode QKV post-processing: optional per-head q/k RMSNorm, neox
    /// RoPE on q and k, and the paged-cache k/v append in ONE launch. q/k/v
    /// are [heads, head_dim] rows addressed by byte offsets (sections of a
    /// fused qkv buffer or separate buffers). Position and page id come from
    /// device buffers — CUDA-graph-replay safe. Bit-exact vs the separate
    /// rmsnorm/rope/kv_append chain (verified in test_kernels.mojo).
    #[allow(clippy::too_many_arguments)]
    pub fn qkv_post_f16(
        &self,
        q_io: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        positions: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        eps: f32,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        // One element per thread: block = head_dim (MAX_HEAD_DIM in
        // qkv_post.mojo bounds the shared staging array).
        if head_dim > 256 || !head_dim.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "qkv_post requires head_dim % 32 == 0 and head_dim <= 256, got {head_dim}"
            )));
        }
        let k = self.artifacts.get("qkv_post_f16")?;
        let cfg = LaunchConfig {
            grid: ((n_heads + n_kv_heads) as u32, 1, 1),
            block: (head_dim as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_io stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf_at(q_io, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_io))
            .buf(k_norm.unwrap_or(q_io))
            .buf(k_cache)
            .buf(v_cache)
            .buf(positions)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(head_dim as i64)
            .scalar(page_size as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Scatter a prefill chunk's K/V rows ([n_tokens, n_kv_heads, head_dim])
    /// into the paged cache at positions base_pos..base_pos+n_tokens.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_batch_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Causal prefill attention over the paged cache. Query token t attends
    /// positions 0..base_pos+t, whose K/V must already be appended.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_prefill_f16_hd64",
            128 => "attn_prefill_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        // Kernel tiling contract (prefill.mojo QT): 16 queries per block,
        // block of 8 warps.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(16), n_q_heads as u32, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q4_K superblocks, x/y f16. Warp per row.
    pub fn gemv_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_K weights → f32 logits.
    pub fn gemv_q4_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q4_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_k_f16_at(y, w_q4k, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q4_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first superblock of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q4k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q6_K superblocks, x/y f16. Warp per row.
    pub fn gemv_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q6_K weights → f32 logits.
    pub fn gemv_q6_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over Q6_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q6_k_f16_at(y, w_q6k, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q6_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first superblock of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q6_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q6_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q6k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Paged flash-decode attention. Layouts documented in attention.mojo.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_f16_hd64",
            128 => "attn_decode_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rows each warp computes in the norm-recomputing fused decode kernels.
    /// Fewer blocks means fewer redundant per-block norm recomputes (h32/h/
    /// norm-weight traffic), which pays off once the projection is tall
    /// enough to keep the GPU busy anyway; per-row math is unchanged.
    fn fused_rows_per_warp(rows: usize) -> usize {
        (rows / 2048).clamp(1, 8)
    }

    /// Guard shared by the norm-recomputing fused decode kernels: the normed
    /// x is staged in a MAX_HIDDEN-element shared array (decode_fused.mojo).
    fn check_fused_hidden(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > 8192 || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= 8192, got {cols}"
            )));
        }
        Ok(())
    }

    /// Fused rmsnorm-recompute + Q8_0 GEMV (decode). ss_from_h16 selects the
    /// sum-of-squares source: the f16 residual h (layer 0, straight from the
    /// embedding gather) or the unrounded f32 mirror h32 (later layers).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q8_0_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_q8_0")?;
        let k = self.artifacts.get("gemv_norm_q8_0_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + NVFP4 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_nvfp4_f16(
        &self,
        y: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 16, "gemv_norm_nvfp4")?;
        let k = self.artifacts.get("gemv_norm_nvfp4_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(packed)
            .buf(scales)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + f16 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 8, "gemv_norm_f16")?;
        let k = self.artifacts.get("gemv_norm_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q4_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_k_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q4_k")?;
        let k = self.artifacts.get("gemv_norm_q4_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + Q6_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q6_k_f16(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        rows: usize,
        cols: usize,
        ss_from_h16: bool,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_q6_k")?;
        let k = self.artifacts.get("gemv_norm_q6_k_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(if ss_from_h16 { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q8_0 GEMV + SiLU (decode FFN).
    /// `w_q8` is the fused gate|up matrix (rows 0..inter gate, inter..2*inter
    /// up); one launch writes act = silu(gate) * up.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q8_0_f16(
        &self,
        act: &DevBuffer,
        w_q8: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q8_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q8_0_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q8)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up NVFP4 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_nvfp4_f16(
        &self,
        act: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        inv_global_scale: f32,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 16, "gemv_norm_silu_nvfp4")?;
        let k = self.artifacts.get("gemv_norm_silu_nvfp4_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(packed)
            .buf(scales)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(inv_global_scale)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up f16 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_f16(
        &self,
        act: &DevBuffer,
        w: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 8, "gemv_norm_silu_f16")?;
        let k = self.artifacts.get("gemv_norm_silu_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q4_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_k_f16(
        &self,
        act: &DevBuffer,
        w_q4k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q4_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q4k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + gate|up Q6_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q6_k_f16(
        &self,
        act: &DevBuffer,
        w_q6k: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        norm_w: &DevBuffer,
        inter: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q6_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q6_k_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w_q6k)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q8_0 GEMV + residual add: h += f16(W·x) with rmsnorm_residual_f16's
    /// rounding; the unrounded f32 sum lands in h32 for the next norm.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q8_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q8_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q8_0_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q8)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// NVFP4 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_nvfp4_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_nvfp4 requires cols % 16 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_nvfp4_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(packed)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// f16 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(8) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_f16 requires cols % 8 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K GEMV + residual add (see gemv_residual_q8_0_f16). The kernel
    /// stages per-32-column x sums in shared memory (Q4K_MAX_SEGS bounds
    /// cols at 32768).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q4k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q6_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q6_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q6_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q6_k_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w_q6k)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Final decode norm from the (h f16, h32 f32) residual pair: out =
    /// rmsnorm(h) * weight with the sum-of-squares taken from h32.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_h32_f16(
        &self,
        out: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_h32_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(h)
            .buf(h32)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split-context flash-decode attention with the qkv_post stage fused in
    /// as a per-block prologue (q/k RMSNorm + RoPE + paged k/v append). q/k/v
    /// are sections of the raw QKV GEMV output addressed by byte offsets;
    /// rotated q lives only in shared memory (the q section is never written
    /// back). Unnormalized per-split partials land in `parts`
    /// ([n_seqs, n_q_heads, n_splits, head_dim + 2] f32) for
    /// attn_decode_combine_f16. n_splits == 1 is bit-exact vs attn_decode_f16.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split_f16(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_split_f16_hd64",
            128 => "attn_decode_split_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_split: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_in stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_in))
            .buf(k_norm.unwrap_or(q_in))
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_split_f16 partials into the final [n_seqs,
    /// n_q_heads, head_dim] f16 output (one warp per head, split order).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_f16(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        head_dim: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_combine_f16_hd64",
            128 => "attn_decode_combine_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads as i64)
            .scalar(n_splits as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}
