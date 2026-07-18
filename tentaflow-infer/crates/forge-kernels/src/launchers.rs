// ===== File: launchers.rs — typed launch wrappers over kernel artifacts =====
// Argument order and meaning must mirror the Mojo kernel signatures exactly
// (kernels/mojo/src/*.mojo). Mojo `Int` marshals as a 64-bit scalar slot,
// `Float32` as f32.

use std::sync::Arc;

use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Stream};
use forge_types::{DType, ForgeError, Result};

use crate::registry::KernelArtifacts;

const BLOCK: u32 = 256;
/// Warps per block in attn_decode (must not exceed MAX_WARPS in attention.mojo).
const ATTN_BLOCK: u32 = 128;

/// Per-block logits slice of the sampling kernels (SAMPLE_CHUNK in
/// sampling.mojo — staged in shared memory by topk_partial_f32).
const SAMPLE_CHUNK: usize = 4096;
/// Largest top_k the GPU draw supports (MAX_TOPK in sampling.mojo).
pub const SAMPLE_MAX_TOPK: usize = 64;
/// Largest vocab the GPU top-k draw supports (MAX_SAMPLE_BLOCKS * CHUNK).
pub const SAMPLE_MAX_VOCAB: usize = 64 * SAMPLE_CHUNK;
/// Scratch capacity in (f32, i32) pairs both sampling paths share
/// (top-k: MAX_SAMPLE_BLOCKS * MAX_TOPK partials; argmax: one per block).
pub const SAMPLE_SCRATCH_PAIRS: usize = 64 * SAMPLE_MAX_TOPK;

pub struct Kernels {
    device: Arc<dyn Device>,
    artifacts: KernelArtifacts,
    /// Codebook grid tables for the IQ formats, uploaded once at load
    /// (ggml iq2xs/iq2s/iq3s grids + ksigns; kernels take them as device
    /// pointers — the constant-table trick llama.cpp's CUDA kernels use).
    iq_tables: IqTables,
}

/// Device-resident ggml codebook tables (LE bytes of the u64/u32 grids).
struct IqTables {
    iq2xs_grid: DevBuffer,
    iq2s_grid: DevBuffer,
    iq3s_grid: DevBuffer,
    iq2xxs_grid: DevBuffer,
    iq3xxs_grid: DevBuffer,
    iq1s_grid: DevBuffer,
    ksigns: DevBuffer,
}

impl IqTables {
    fn upload(device: &dyn Device) -> Result<Self> {
        use forge_formats::iq_tables::{
            IQ1S_GRID, IQ2S_GRID, IQ2XS_GRID, IQ2XXS_GRID, IQ3S_GRID, IQ3XXS_GRID, KSIGNS_IQ2XS,
        };
        let up = |bytes: &[u8]| -> Result<DevBuffer> {
            let buf = device.alloc(bytes.len(), forge_types::MemKind::Device, forge_hal::Pool::Weights)?;
            device.write(bytes, &buf, 0)?;
            Ok(buf)
        };
        let u64s = |t: &[u64]| -> Vec<u8> { t.iter().flat_map(|v| v.to_le_bytes()).collect() };
        let u32s = |t: &[u32]| -> Vec<u8> { t.iter().flat_map(|v| v.to_le_bytes()).collect() };
        Ok(Self {
            iq2xs_grid: up(&u64s(&IQ2XS_GRID))?,
            iq2s_grid: up(&u64s(&IQ2S_GRID))?,
            iq3s_grid: up(&u32s(&IQ3S_GRID))?,
            iq2xxs_grid: up(&u64s(&IQ2XXS_GRID))?,
            iq3xxs_grid: up(&u32s(&IQ3XXS_GRID))?,
            iq1s_grid: up(&u64s(&IQ1S_GRID))?,
            ksigns: up(&KSIGNS_IQ2XS)?,
        })
    }
}

impl Kernels {
    pub fn load(device: Arc<dyn Device>) -> Result<Self> {
        let artifacts = KernelArtifacts::load(device.as_ref())?;
        let iq_tables = IqTables::upload(device.as_ref())?;
        Ok(Self { device, artifacts, iq_tables })
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

    /// `rmsnorm_f16` over a section of a fused buffer, addressed by byte offset
    /// (in/out share the slice). Used by the rot decode path to normalize the
    /// q/k slices of a fused qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16_at(
        &self,
        io: &DevBuffer,
        byte_off: usize,
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
            .buf_at(io, byte_off)?
            .buf_at(io, byte_off)?
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

    /// out = a * sigmoid(gate) over n f16 elements (attention output gate).
    pub fn sigmoid_mul_f16(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        gate: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sigmoid_mul_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(a)
            .buf(gate)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// De-interleave a gated Q projection [n_heads, 2*head_dim] into query and
    /// gate halves (each [n_heads, head_dim]). `n = n_heads * head_dim`.
    pub fn deinterleave_gate_f16(
        &self,
        qc: &DevBuffer,
        gatec: &DevBuffer,
        q_full: &DevBuffer,
        head_dim: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deinterleave_gate_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(qc)
            .buf(gatec)
            .buf(q_full)
            .scalar(head_dim as i64)
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

    /// `rope_neox_f16` over a section of a fused buffer, addressed by byte
    /// offset. Used by the rot decode path to rope the q/k slices of a fused
    /// qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16_at(
        &self,
        x_io: &DevBuffer,
        byte_off: usize,
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
            .buf_at(x_io, byte_off)?
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Partial NEOX rotary: rotate only the first `n_rot` dims of each head
    /// (qwen35moe M-RoPE reduces to this for text positions). Layout matches
    /// `rope_neox_f16` ([n_tokens, n_heads, head_dim], in place).
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_partial_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        n_rot: usize,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rope_neox_partial_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((n_rot as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(x_io)
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(n_rot as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Depthwise causal conv (width `d_conv`) + SiLU, one DeltaNet decode step.
    /// `win_io` [conv_dim, d_conv-1] (oldest first) is advanced in place;
    /// `weight` is ggml ssm_conv1d {d_conv, conv_dim} flattened. Grid-stride
    /// over channels.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16(
        &self,
        out: &DevBuffer,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        weight: &DevBuffer,
        conv_dim: usize,
        d_conv: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_conv_silu_f16")?;
        let cfg = LaunchConfig {
            grid: ((conv_dim as u32).div_ceil(256).min(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(win_io)
            .buf(x_new)
            .buf(weight)
            .scalar(conv_dim as i64)
            .scalar(d_conv as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head L2 normalization (out = x / sqrt(Σx² + eps)); one block per
    /// head, block covers `d_state`. Used on the DeltaNet conv q/k heads.
    pub fn l2norm_heads_f16(
        &self,
        out: &DevBuffer,
        x_in: &DevBuffer,
        n_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("l2norm_heads_f16")?;
        let cfg = LaunchConfig {
            grid: (n_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x_in)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// One Gated-DeltaNet recurrence step per value-head (grid = n_v_heads,
    /// block = d_state). `state_io` [n_v_heads, d_state, d_state] f32 is
    /// updated in place; q/k must already be L2-normed and repeated to
    /// n_v_heads. `g`/`beta` are the per-head log-decay / write gate (f32).
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_step_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_gated_step: d_state {d_state} exceeds shared staging (1024)"
            )));
        }
        let k_art = self.artifacts.get("deltanet_gated_step_f16")?;
        let cfg = LaunchConfig {
            grid: (n_v_heads as u32, 1, 1),
            block: (d_state as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Output gated RMSNorm per value-head: out = rmsnorm(o, weight)·silu(z).
    /// One block per head, block covers `d_state`.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_rmsnorm_f16(
        &self,
        out: &DevBuffer,
        o_in: &DevBuffer,
        z_in: &DevBuffer,
        weight: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_gated_rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (n_v_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(o_in)
            .buf(z_in)
            .buf(weight)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head DeltaNet log-decay g = softplus(alpha + dt_bias)·a (f32 out).
    pub fn deltanet_log_decay_f32(
        &self,
        g_out: &DevBuffer,
        alpha_in: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_log_decay_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(g_out)
            .buf(alpha_in)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head DeltaNet write gate beta = sigmoid(beta_proj) (f32 out).
    pub fn deltanet_beta_sigmoid_f32(
        &self,
        beta_out: &DevBuffer,
        beta_in: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_beta_sigmoid_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(beta_out)
            .buf(beta_in)
            .scalar(n_v_heads as i64);
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

    /// f16 GEMM emitting f32 outputs over a row window of `w` (batched logit
    /// head). Same grid/tiling as `gemm_f16_at`; the f32 store preserves the
    /// mma accumulator precision so batched logits match the single-row
    /// gemv_*_out_f32 path.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_f16_out_f32_at(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(8) {
            return Err(ForgeError::Kernel(format!(
                "gemm_f16_out_f32 requires cols % 8 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_f16_out_f32{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q8_0 GEMM emitting f32 outputs (batched logit head).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_out_f32_at(
        &self,
        y_f32: &DevBuffer,
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
                "gemm_q8_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q8_0_out_f32{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched greedy argmax over `logits` ([n_seqs, vocab] f32): one block per
    /// sequence, ties to the lowest id. `out_ids` receives n_seqs i32 token ids.
    pub fn sample_batched_argmax_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("argmax_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(logits)
            .scalar(vocab as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched categorical draw over `logits` ([n_seqs, vocab] f32) with
    /// per-seq params (k / inv_temp / top_p / min_p / seed / step arrays, each
    /// n_seqs long). `out_ids` receives n_seqs i32 token ids. `logits` is
    /// mutated (top-k masking) — valid because it is regenerated every step.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_topk_f32(
        &self,
        out_ids: &DevBuffer,
        logits: &DevBuffer,
        n_seqs: usize,
        vocab: usize,
        k_arr: &DevBuffer,
        inv_t_arr: &DevBuffer,
        top_p_arr: &DevBuffer,
        min_p_arr: &DevBuffer,
        seed_arr: &DevBuffer,
        step_arr: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_batched_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        let k = self.artifacts.get("topk_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_ids)
            .buf(logits)
            .scalar(vocab as i64)
            .buf(k_arr)
            .buf(inv_t_arr)
            .buf(top_p_arr)
            .buf(min_p_arr)
            .buf(seed_arr)
            .buf(step_arr);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Batched in-place repetition penalty. `offsets` is n_seqs+1 i32 prefix
    /// sums into the flat `ids` list; `penalties` is n_seqs f32.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_batched_penalize_f32(
        &self,
        logits: &DevBuffer,
        vocab: usize,
        ids: &DevBuffer,
        offsets: &DevBuffer,
        penalties: &DevBuffer,
        n_seqs: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_batched_f32")?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .scalar(vocab as i64)
            .buf(ids)
            .buf(offsets)
            .buf(penalties);
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

    /// Kernel-name suffix for a KV cache element type (F16 canonical, FP8
    /// E4M3 per-value scale-free quantization).
    fn kv_suffix(kv_dtype: DType, what: &str) -> Result<&'static str> {
        match kv_dtype {
            DType::F16 => Ok("f16"),
            DType::F8E4M3 => Ok("fp8"),
            other => Err(ForgeError::Unsupported(format!(
                "{what}: no kernels for KV cache dtype {other}"
            ))),
        }
    }

    /// Scatter a prefill chunk's K/V rows ([n_tokens, n_kv_heads, head_dim])
    /// into the paged cache at positions base_pos..base_pos+n_tokens.
    /// `kv_dtype` selects the cache element type (f16 verbatim | fp8-e4m3
    /// per-value cast).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch(
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
        kv_dtype: DType,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "kv_append_batch")?;
        let k = self.artifacts.get(&format!("kv_append_batch_{suffix}"))?;
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
    /// `kv_dtype` selects the cache element type; the fp8 variant widens
    /// e4m3 rows to f16 in shared memory (exact), so its math matches the
    /// f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill(
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
        kv_dtype: DType,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "attn_prefill")?;
        let name = match (head_dim, kv_dtype) {
            (64, _) => format!("attn_prefill_{suffix}_hd64"),
            (128, _) => format!("attn_prefill_{suffix}_hd128"),
            // Only the f16 cache has an hd256 specialization (qwen35moe
            // attention layers); fp8/rot hd256 is not compiled.
            (256, DType::F16) => format!("attn_prefill_{suffix}_hd256"),
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
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

    /// `gemv_q6_k_f16` over a row window of `w_q6k` (`w_byte_off` addresses the
    /// window's first row). One block per 8 output rows — used for the routed
    /// MoE down-projection so a single-token expert GEMV saturates the SMs
    /// instead of a 64-token GEMM tile with one live column.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_f16_at(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        w_byte_off: usize,
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
            .buf_at(w_q6k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q6_K expert GEMV whose expert row window is read ON DEVICE
    /// from `ids[sel]` (no host readback). Writes the per-expert `[rows]` output
    /// at `y[0..]`; global weight row = `ids[sel] * rows_per_expert + local_row`,
    /// bit-identical to `gemv_q6_k_f16_at` at that expert's byte offset.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q6_k_f16_gidx(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
        rows_per_expert: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q6_k_f16_gidx requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q6_k_f16_gidx")?;
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
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64)
            .scalar(rows_per_expert as i64);
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
            256 => "attn_decode_f16_hd256",
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
        // The NVFP4 gate|up dot is register-heavier than the Q4_K path, so the
        // rows/warp sweet spot is lower: measured best at 3 on the Bielik
        // inter=11264 shape (vs fused_rows_per_warp -> 5). This is scoped to
        // the NVFP4 silu launcher, so Q4_K's inter=14336 rpw=7 is untouched.
        let rpw = 3usize;
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
    /// `kv_dtype` selects the cache element type: the fp8 variant appends
    /// e4m3(f16(rope(k)))/e4m3(v) and widens cache reads exactly, so its
    /// math matches the f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split(
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
        kv_dtype: DType,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "attn_decode_split")?;
        let name = match head_dim {
            64 => format!("attn_decode_split_{suffix}_hd64"),
            128 => format!("attn_decode_split_{suffix}_hd128"),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_split: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
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

    /// Commit T tokens already resident in the paged f16 K/V cache
    /// (positions base_pos..base_pos+T) into the rotational low-bit store
    /// (rotquant.mojo: WHT rotate + 3/4-bit pack + per-(token,head) f16 scale).
    /// Grid (T, n_kv_heads); one thread per (token, head) vector.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot_from_cache(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot_from_cache", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rotate+quant+pack a batch of T linear (rope'd) K/V rows
    /// ([n_tokens, n_kv_heads, head_dim] f16) into the paged rotational store at
    /// the absolute positions in `positions` ([T] i32, one per token), writing
    /// the rotated f16 vectors into the residual ring at `pos % ring_slots` (the
    /// recent-window fidelity copy the decode attention reads directly). Reading
    /// the position from a device buffer keeps decode launches graph-capturable.
    /// Grid (T, n_kv_heads).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        k_in: &DevBuffer,
        k_in_byte_off: usize,
        v_in: &DevBuffer,
        v_in_byte_off: usize,
        page_table: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        ring_slots: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf_at(k_in, k_in_byte_off)?
            .buf_at(v_in, v_in_byte_off)?
            .buf(page_table)
            .buf(positions)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(ring_slots as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split-K rotational low-bit decode attention over the dual-region store:
    /// reads the residual f16 ring for the recent `ring_slots` positions (rotated
    /// f16, no unpack) and the packed 3/4-bit store for everything older. Rotates
    /// q once (block-cooperative WHT), scores in rotated space
    /// ((R·q)·k_rot = q·k), and writes each (seq, head, split) an UNNORMALIZED
    /// rotated partial to `parts` ([n_seqs, n_q_heads, n_splits, head_dim + 2]
    /// f32). `attn_decode_combine_rot` merges the splits and inverse-rotates.
    /// `ring_slots == 0` degrades to packed-only. Grid (n_seqs, n_q_heads,
    /// n_splits); block ATTN_BLOCK.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_rot(
        &self,
        parts: &DevBuffer,
        q: &DevBuffer,
        q_byte_off: usize,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        ring_slots: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_decode_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q, q_byte_off)?
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(ring_slots as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_rot's per-split rotated partials into the final
    /// [n_seqs, n_q_heads, head_dim] f16 output and inverse-rotate once per head
    /// (one warp per head, split order). Head_dim {64,128}.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_rot(
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
            64 => "attn_decode_combine_rot_hd64",
            128 => "attn_decode_combine_rot_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine_rot: head_dim {other} has no compiled specialization"
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

    /// Rotational low-bit causal prefill attention over the packed store: query
    /// token t attends positions 0..base_pos+t. Packed-only (the residual ring's
    /// recent window would be overwritten within a chunk). Grid (T, n_q_heads),
    /// one warp per (token, head).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_rot(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_prefill_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kernel name for a rotational specialization: `<base>_hd{64,128}_b{3,4}`.
    fn rot_kernel_name(base: &str, head_dim: usize, bits: u8) -> Result<String> {
        if bits != 3 && bits != 4 {
            return Err(ForgeError::Unsupported(format!(
                "rotational KV supports 3 or 4 bits, got {bits}"
            )));
        }
        match head_dim {
            64 | 128 => Ok(format!("{base}_hd{head_dim}_b{bits}")),
            other => Err(ForgeError::Unsupported(format!(
                "rotational KV: head_dim {other} has no compiled specialization"
            ))),
        }
    }

    /// Column bound of the dp4a kernels that quantize x from global memory
    /// into shared int8 (plain + residual variants; X_MAX in decode_dp4a.mojo).
    pub const DP4A_MAX_COLS: usize = 16384;


    fn check_dp4a_cols(cols: usize, quant_mult: usize, name: &str) -> Result<()> {
        if cols > Self::DP4A_MAX_COLS || !cols.is_multiple_of(quant_mult) {
            return Err(ForgeError::Kernel(format!(
                "{name} requires cols % {quant_mult} == 0 and cols <= {}, got {cols}",
                Self::DP4A_MAX_COLS
            )));
        }
        Ok(())
    }

    /// Q8_0 GEMV with int8-quantized activations (q8_1) and dp4a dots.
    /// Not bit-exact vs gemv_q8_0_f16 (activation quantization rounding).
    pub fn gemv_q8_0_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_q8_0_dp4a_f16")?;
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

    /// Q4_K GEMV with int8-quantized activations (q8_1) and dp4a dots.
    pub fn gemv_q4_k_dp4a_f16(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16")?;
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

    /// `gemv_q4_k_dp4a_f16` over a row window of `w_q4k` (`w_byte_off` addresses
    /// the window's first row). Used for the routed MoE gate/up projections so a
    /// single-token expert GEMV launches per-row blocks instead of a starved
    /// 64-token GEMM tile.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_f16_at(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w_q4k, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Routed-MoE Q4_K expert GEMV whose expert row window is read ON DEVICE
    /// from `ids[sel]` (no host readback of the router selection). Writes the
    /// per-expert `[rows]` output at `y[0..]`; the global weight row is
    /// `ids[sel] * rows_per_expert + local_row`, so the result is bit-identical
    /// to `gemv_q4_k_dp4a_f16_at` at byte offset `ids[sel]*rows_per_expert`.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_q4_k_dp4a_f16_gidx(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        ids: &DevBuffer,
        sel: usize,
        rows_per_expert: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a_f16_gidx")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_f16_gidx")?;
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
            .scalar(rows as i64)
            .buf(ids)
            .scalar(sel as i64)
            .scalar(rows_per_expert as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Q4_K logit GEMV (f32 out) with dp4a dots.
    pub fn gemv_q4_k_dp4a_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q4_k_dp4a_out_f32")?;
        let k = self.artifacts.get("gemv_q4_k_dp4a_out_f32")?;
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

    /// Fused rmsnorm-recompute + Q8_0 dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q8_0_dp4a_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q8_0_dp4a_f16")?;
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

    /// Fused rmsnorm-recompute + Q4_K dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_k_dp4a_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q4_k_dp4a_f16")?;
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

    /// Fused rmsnorm-recompute + Q6_K dp4a GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q6_k_dp4a_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_q6_k_dp4a_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q8_0 dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q8_0_dp4a_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q8_0_dp4a_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q4_K dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_k_dp4a_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_k_dp4a_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q6_K dp4a GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q6_k_dp4a_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_norm_silu_q6_k_dp4a_f16")?;
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

    /// Q8_0 dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q8_0_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q8: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 32, "gemv_residual_q8_0_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q8_0_dp4a_f16")?;
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

    /// Q6_K dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q6_k_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_residual_q6_k_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q6_k_dp4a_f16")?;
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

    /// Q6_K logit GEMV (f32 out) with dp4a dots.
    pub fn gemv_q6_k_dp4a_out_f32(
        &self,
        y_f32: &DevBuffer,
        w_q6k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_q6_k_dp4a_out_f32")?;
        let k = self.artifacts.get("gemv_q6_k_dp4a_out_f32")?;
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

    /// Q4_K dp4a GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_k_dp4a_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w_q4k: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        Self::check_dp4a_cols(cols, 256, "gemv_residual_q4_k_dp4a")?;
        let k = self.artifacts.get("gemv_residual_q4_k_dp4a_f16")?;
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

    /// In-place repetition penalty over `n_ids` distinct token ids staged in
    /// `ids` (i32). Callers must deduplicate: the kernel applies the penalty
    /// once per listed id.
    pub fn sample_penalize_f32(
        &self,
        logits: &DevBuffer,
        ids: &DevBuffer,
        n_ids: usize,
        penalty: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("penalize_f32")?;
        let cfg = LaunchConfig::linear(n_ids as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(ids)
            .scalar(n_ids as i64)
            .scalar(penalty);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Greedy argmax over f32 logits; the winning index lands in the first
    /// 4 bytes of `out` (i32) and its logprob slot (f32, 0 for greedy) in the
    /// next 4. Ties resolve to the lowest index like a sequential CPU scan.
    /// `scratch_vals`/`scratch_idx` hold the per-block partials
    /// (>= SAMPLE_SCRATCH_PAIRS entries each).
    pub fn sample_argmax_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        stream: &Stream,
    ) -> Result<()> {
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        if n_blocks > SAMPLE_SCRATCH_PAIRS {
            return Err(ForgeError::Unsupported(format!(
                "sample_argmax: vocab {vocab} exceeds scratch capacity"
            )));
        }
        let kp = self.artifacts.get("argmax_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("argmax_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(n_blocks as i64);
        self.device.launch(kf, &cfg, &args, stream)
    }

    /// Categorical draw over f32 logits: top-k (k <= SAMPLE_MAX_TOPK)
    /// selection, temperature softmax, min-p floor, top-p cut, then a
    /// deterministic counter-hash draw on (seed, step). The sampled id (i32)
    /// lands in the first 4 bytes of `out`, its top-k-softmax logprob (f32)
    /// in the next 4.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_topk_f32(
        &self,
        out: &DevBuffer,
        scratch_vals: &DevBuffer,
        scratch_idx: &DevBuffer,
        logits: &DevBuffer,
        vocab: usize,
        k: usize,
        inv_t: f32,
        top_p: f32,
        min_p: f32,
        seed: u64,
        step: u64,
        stream: &Stream,
    ) -> Result<()> {
        if k == 0 || k > SAMPLE_MAX_TOPK {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: k {k} outside 1..={SAMPLE_MAX_TOPK}"
            )));
        }
        if vocab > SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "sample_topk: vocab {vocab} exceeds {SAMPLE_MAX_VOCAB}"
            )));
        }
        let n_blocks = vocab.div_ceil(SAMPLE_CHUNK);
        let kp = self.artifacts.get("topk_partial_f32")?;
        let cfg = LaunchConfig {
            grid: (n_blocks as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(logits)
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar(vocab as i64)
            .scalar(SAMPLE_CHUNK as i64)
            .scalar(k as i64);
        self.device.launch(kp, &cfg, &args, stream)?;

        let kf = self.artifacts.get("topk_final_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(out, 4)?
            .buf(scratch_vals)
            .buf(scratch_idx)
            .scalar((n_blocks * k) as i64)
            .scalar(k as i64)
            .scalar(inv_t)
            .scalar(top_p)
            .scalar(min_p)
            .scalar(seed)
            .scalar(step);
        self.device.launch(kf, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML Q5_K blocks, x/y f16. Warp per row.
    pub fn gemv_q5_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_K weights → f32 logits.
    pub fn gemv_q5_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_k_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q5_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_k_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_k_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q5_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_q5_k")?;
        let k = self.artifacts.get("gemv_norm_q5_k_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q5_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q5_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_k_f16")?;
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

    /// Q5_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_k_f16")?;
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

    /// y = W·x with W in GGML Q3_K blocks, x/y f16. Warp per row.
    pub fn gemv_q3_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q3_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q3_K weights → f32 logits.
    pub fn gemv_q3_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q3_k_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q3_k_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q3_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q3_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q3_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q3_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q3_k_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q3_k_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q3_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q3_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_q3_k")?;
        let k = self.artifacts.get("gemv_norm_q3_k_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q3_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q3_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q3_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q3_k_f16")?;
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

    /// Q3_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q3_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q3_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q3_k_f16")?;
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

    /// y = W·x with W in GGML Q2_K blocks, x/y f16. Warp per row.
    pub fn gemv_q2_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q2_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q2_k_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q2_K weights → f32 logits.
    pub fn gemv_q2_k_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_q2_k_out_f32 requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q2_k_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q2_K weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q2_k_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q2_k_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q2_k_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q2_k_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q2_k requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q2_k_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q2_K GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q2_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_q2_k")?;
        let k = self.artifacts.get("gemv_norm_q2_k_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q2_K GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q2_k_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_q2_k")?;
        let k = self.artifacts.get("gemv_norm_silu_q2_k_f16")?;
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

    /// Q2_K GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q2_k_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) || cols > 32768 {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q2_k requires cols % 256 == 0 and cols <= 32768, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q2_k_f16")?;
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

    /// y = W·x with W in GGML Q4_0 blocks, x/y f16. Warp per row.
    pub fn gemv_q4_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_0 weights → f32 logits.
    pub fn gemv_q4_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_0_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q4_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_0_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q4_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_0_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_0_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q4_0 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_0_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_q4_0")?;
        let k = self.artifacts.get("gemv_norm_q4_0_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q4_0 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_0_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q4_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_0_f16")?;
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

    /// Q4_0 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_0_f16")?;
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

    /// y = W·x with W in GGML Q4_1 blocks, x/y f16. Warp per row.
    pub fn gemv_q4_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_1_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q4_1 weights → f32 logits.
    pub fn gemv_q4_1_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q4_1_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q4_1_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q4_1 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q4_1_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q4_1_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_1_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q4_1_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q4_1 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q4_1_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_q4_1")?;
        let k = self.artifacts.get("gemv_norm_q4_1_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q4_1 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q4_1_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q4_1")?;
        let k = self.artifacts.get("gemv_norm_silu_q4_1_f16")?;
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

    /// Q4_1 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q4_1_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q4_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q4_1_f16")?;
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

    /// y = W·x with W in GGML Q5_0 blocks, x/y f16. Warp per row.
    pub fn gemv_q5_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_0_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_0 weights → f32 logits.
    pub fn gemv_q5_0_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_0_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_0_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q5_0 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_0_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_0_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_0_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_0_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_0_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q5_0 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_0_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_q5_0")?;
        let k = self.artifacts.get("gemv_norm_q5_0_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q5_0 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_0_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q5_0")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_0_f16")?;
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

    /// Q5_0 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_0_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_0 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_0_f16")?;
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

    /// y = W·x with W in GGML Q5_1 blocks, x/y f16. Warp per row.
    pub fn gemv_q5_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_1_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over Q5_1 weights → f32 logits.
    pub fn gemv_q5_1_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_q5_1_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_q5_1_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over Q5_1 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_1_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_q5_1_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_q5_1_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q5_1_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q5_1_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + Q5_1 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_q5_1_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_q5_1")?;
        let k = self.artifacts.get("gemv_norm_q5_1_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up Q5_1 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_q5_1_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_q5_1")?;
        let k = self.artifacts.get("gemv_norm_silu_q5_1_f16")?;
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

    /// Q5_1 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_q5_1_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_q5_1 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_q5_1_f16")?;
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

    /// y = W·x with W in GGML IQ4_NL blocks, x/y f16. Warp per row.
    pub fn gemv_iq4_nl_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_nl_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ4_NL weights → f32 logits.
    pub fn gemv_iq4_nl_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_nl_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_nl_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over IQ4_NL weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_nl_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq4_nl_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq4_nl_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_nl_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq4_nl_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + IQ4_NL GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq4_nl_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_iq4_nl")?;
        let k = self.artifacts.get("gemv_norm_iq4_nl_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up IQ4_NL GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq4_nl_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_iq4_nl")?;
        let k = self.artifacts.get("gemv_norm_silu_iq4_nl_f16")?;
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

    /// IQ4_NL GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq4_nl_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq4_nl requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq4_nl_f16")?;
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

    /// y = W·x with W in GGML IQ4_XS blocks, x/y f16. Warp per row.
    pub fn gemv_iq4_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_xs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ4_XS weights → f32 logits.
    pub fn gemv_iq4_xs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq4_xs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq4_xs_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over IQ4_XS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq4_xs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq4_xs_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq4_xs_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq4_xs_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + IQ4_XS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq4_xs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq4_xs")?;
        let k = self.artifacts.get("gemv_norm_iq4_xs_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up IQ4_XS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq4_xs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq4_xs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq4_xs_f16")?;
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

    /// IQ4_XS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq4_xs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq4_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq4_xs_f16")?;
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

    /// y = W·x with W in GGML MXFP4 blocks, x/y f16. Warp per row.
    pub fn gemv_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over MXFP4 weights → f32 logits.
    pub fn gemv_mxfp4_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_mxfp4_out_f32 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_mxfp4_out_f32_v2")?;
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

    /// Y[t, row] = W·x[t] over MXFP4 weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_mxfp4_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_mxfp4_f16` over a row window of a fused weight matrix:
    /// `w_byte_off` addresses the first block of the window's first row.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_mxfp4_f16_at(
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
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_mxfp4_gguf_f16{suffix}"))?;
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

    /// Fused rmsnorm-recompute + MXFP4 GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_mxfp4_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_mxfp4_f16")?;
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

    /// Fused rmsnorm-recompute + gate|up MXFP4 GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_mxfp4_f16(
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
        Self::check_fused_hidden(cols, 32, "gemv_norm_silu_mxfp4")?;
        let k = self.artifacts.get("gemv_norm_silu_mxfp4_f16")?;
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

    /// MXFP4 GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_mxfp4_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_mxfp4 requires cols % 32 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_mxfp4_f16")?;
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

    /// y = W·x with W in GGML IQ2_XS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_XS weights → f32 logits.
    pub fn gemv_iq2_xs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_XS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_xs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_xs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xs_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_xs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_XS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_xs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_xs")?;
        let k = self.artifacts.get("gemv_norm_iq2_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
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

    /// Fused rmsnorm-recompute + gate|up IQ2_XS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_xs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_xs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_xs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_XS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_xs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_xs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_xs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2xs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ2_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_S weights → f32 logits.
    pub fn gemv_iq2_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_s_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_s")?;
        let k = self.artifacts.get("gemv_norm_iq2_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
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

    /// Fused rmsnorm-recompute + gate|up IQ2_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ3_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq3_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ3_S weights → f32 logits.
    pub fn gemv_iq3_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ3_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq3_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq3_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_s_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq3_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ3_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq3_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq3_s")?;
        let k = self.artifacts.get("gemv_norm_iq3_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
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

    /// Fused rmsnorm-recompute + gate|up IQ3_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq3_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq3_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq3_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ3_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq3_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq3_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq3_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq3s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// y = W·x with W in GGML IQ2_XXS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq2_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xxs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ2_XXS weights → f32 logits.
    pub fn gemv_iq2_xxs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq2_xxs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq2_xxs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ2_XXS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq2_xxs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq2_xxs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq2_xxs_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq2_xxs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ2_XXS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq2_xxs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq2_xxs")?;
        let k = self.artifacts.get("gemv_norm_iq2_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
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

    /// Fused rmsnorm-recompute + gate|up IQ2_XXS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq2_xxs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq2_xxs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq2_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ2_XXS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq2_xxs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq2_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq2_xxs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq2xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ3_XXS superblocks, x/y f16. Warp per row.
    pub fn gemv_iq3_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_xxs_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ3_XXS weights → f32 logits.
    pub fn gemv_iq3_xxs_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq3_xxs_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq3_xxs_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ3_XXS weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_xxs_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq3_xxs_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq3_xxs_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq3_xxs_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq3_xxs_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ3_XXS GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq3_xxs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq3_xxs")?;
        let k = self.artifacts.get("gemv_norm_iq3_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
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

    /// Fused rmsnorm-recompute + gate|up IQ3_XXS GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq3_xxs_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq3_xxs")?;
        let k = self.artifacts.get("gemv_norm_silu_iq3_xxs_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ3_XXS GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq3_xxs_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq3_xxs requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq3_xxs_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq3xxs_grid)
            .buf(&self.iq_tables.ksigns)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ1_S superblocks, x/y f16. Warp per row.
    pub fn gemv_iq1_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_s_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ1_S weights → f32 logits.
    pub fn gemv_iq1_s_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_s_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_s_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ1_S weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_s_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq1_s_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq1_s_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_s_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq1_s_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ1_S GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq1_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq1_s")?;
        let k = self.artifacts.get("gemv_norm_iq1_s_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
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

    /// Fused rmsnorm-recompute + gate|up IQ1_S GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq1_s_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq1_s")?;
        let k = self.artifacts.get("gemv_norm_silu_iq1_s_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ1_S GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq1_s_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq1_s requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq1_s_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
    /// y = W·x with W in GGML IQ1_M superblocks, x/y f16. Warp per row.
    pub fn gemv_iq1_m_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_m_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logit GEMV over IQ1_M weights → f32 logits.
    pub fn gemv_iq1_m_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_iq1_m_out_f32 requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_iq1_m_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Y[t, row] = W·x[t] over IQ1_M weights and row-major f16 activations.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_m_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.gemm_iq1_m_f16_at(y, w, 0, x, rows, cols, n_tokens, stream)
    }

    /// `gemm_iq1_m_f16` over a row window of a fused weight matrix.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_iq1_m_f16_at(
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
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemm_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_iq1_m_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(64), (n_tokens as u32).div_ceil(bm), 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused rmsnorm-recompute + IQ1_M GEMV (decode).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_iq1_m_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_iq1_m")?;
        let k = self.artifacts.get("gemv_norm_iq1_m_f16")?;
        let rpw = Self::fused_rows_per_warp(rows);
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
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

    /// Fused rmsnorm-recompute + gate|up IQ1_M GEMV + SiLU (decode FFN).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_norm_silu_iq1_m_f16(
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
        Self::check_fused_hidden(cols, 256, "gemv_norm_silu_iq1_m")?;
        let k = self.artifacts.get("gemv_norm_silu_iq1_m_f16")?;
        let rpw = Self::fused_rows_per_warp(inter);
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(8 * rpw as u32), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(act)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(h)
            .buf(h32)
            .buf(norm_w)
            .scalar(cols as i64)
            .scalar(inter as i64)
            .scalar(eps)
            .scalar(rpw as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// IQ1_M GEMV + residual add (see gemv_residual_q8_0_f16).
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_residual_iq1_m_f16(
        &self,
        h_io: &DevBuffer,
        h32: &DevBuffer,
        w: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_residual_iq1_m requires cols % 256 == 0, got {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_residual_iq1_m_f16")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(h_io)
            .buf(h32)
            .buf(w)
            .buf(&self.iq_tables.iq1s_grid)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// MoE router: for each of `n_tokens` rows of `x` (f16, [n_tokens, hidden])
    /// compute logits `x · gate_inp` over `n_expert` experts (f16 router,
    /// [n_expert, hidden]), softmax over all experts, then select the top-k.
    /// Writes `ids` ([n_tokens, top_k] i32) and `weights` ([n_tokens, top_k]
    /// f32). `norm_topk` renormalizes the selected weights to sum 1.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_router_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_inp: &DevBuffer,
        n_tokens: usize,
        hidden: usize,
        n_expert: usize,
        top_k: usize,
        norm_topk: bool,
        stream: &Stream,
    ) -> Result<()> {
        // Shared-memory staging caps (mirror MOE_MAX_* in moe.mojo).
        if hidden > 8192 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: hidden {hidden} exceeds kernel cap 8192"
            )));
        }
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: n_expert {n_expert} exceeds kernel cap 256"
            )));
        }
        let k = self.artifacts.get("moe_router_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(x)
            .buf(gate_inp)
            .scalar(hidden as i64)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(norm_topk as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fold one routed expert's f16 output into a token's FFN accumulator:
    /// `acc += scale * src` over `n` elements (or `acc = scale * src` when
    /// `init`). Both buffers are addressed by byte offset so a per-token row of
    /// a batched accumulator can be targeted.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        scale: f32,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .scalar(scale)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Like `moe_scale_add_f16` but the router weight is read ON DEVICE from
    /// `weights[sel]`, so no host readback of the routing weights is needed.
    /// For the shared expert, pass its device-resident sigmoid gate scale as
    /// `weights` with `sel = 0`.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_gidx_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        weights: &DevBuffer,
        sel: usize,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_gidx_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .buf(weights)
            .scalar(sel as i64)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out[0] = sigmoid(in[0])`: turns the shared-expert gate logit (f16) into
    /// a device-resident f32 scale so `moe_scale_add_gidx_f16` can fold the
    /// shared expert without a per-layer host round-trip.
    pub fn moe_sigmoid_f16_to_f32(
        &self,
        out: &DevBuffer,
        input: &DevBuffer,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_sigmoid_f16_to_f32")?;
        let cfg = LaunchConfig::linear(1, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(input);
        self.device.launch(k, &cfg, &args, stream)
    }

    // --- ONNX subset f32 ops (forge-onnx interpreter) -----------------------

    /// General 1-D convolution (group=1, dilation=1), all f32. `x` [in_ch, in_t],
    /// `w` [out_ch, in_ch, ksize], optional `bias` [out_ch], `out` [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        bias: Option<&DevBuffer>,
        in_ch: usize,
        in_t: usize,
        out_ch: usize,
        out_t: usize,
        ksize: usize,
        stride: usize,
        pad: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_f32")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(BLOCK), out_ch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent bias still needs a valid device pointer (never read); `out`
        // stands in, mirroring the qkv_post launcher convention.
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(w)
            .buf(bias.unwrap_or(out))
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_ch as i64)
            .scalar(out_t as i64)
            .scalar(ksize as i64)
            .scalar(stride as i64)
            .scalar(pad as i64)
            .scalar(if bias.is_some() { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = max(x, 0) over n f32 elements.
    pub fn relu_f32(&self, out: &DevBuffer, x: &DevBuffer, n: usize, stream: &Stream) -> Result<()> {
        let k = self.artifacts.get("relu_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sigmoid(x) over n f32 elements.
    pub fn sigmoid_f32(&self, out: &DevBuffer, x: &DevBuffer, n: usize, stream: &Stream) -> Result<()> {
        let k = self.artifacts.get("sigmoid_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = a + b, same shape, n f32 elements (broadcasting done host-side).
    pub fn add_f32(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        b: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("add_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(b).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = x^e (elementwise, scalar exponent) over n f32 elements.
    pub fn pow_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        e: f32,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("pow_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(e).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sqrt(x) over n f32 elements.
    pub fn sqrt_f32(&self, out: &DevBuffer, x: &DevBuffer, n: usize, stream: &Stream) -> Result<()> {
        let k = self.artifacts.get("sqrt_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[o, i] = mean over the reduced axis of x viewed as [outer, axis, inner].
    pub fn reduce_mean_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        outer: usize,
        axis: usize,
        inner: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("reduce_mean_f32")?;
        let cfg = LaunchConfig::linear((outer * inner) as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .scalar(outer as i64)
            .scalar(axis as i64)
            .scalar(inner as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Single-direction, batch-1 ONNX LSTM (gate order i,o,f,c). Shapes are
    /// direction/batch-squeezed by the caller: `x` [seq, input], `w` [4h, input],
    /// `r` [4h, hidden], `b` [8h], `h0`/`c0` [hidden]; `y` [seq, hidden],
    /// `yh`/`yc` [hidden].
    #[allow(clippy::too_many_arguments)]
    pub fn lstm_f32(
        &self,
        y: &DevBuffer,
        yh: &DevBuffer,
        yc: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        r: &DevBuffer,
        b: &DevBuffer,
        h0: &DevBuffer,
        c0: &DevBuffer,
        seq: usize,
        input_size: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Shared recurrent state is sized for LSTM_MAX_HIDDEN = 512 in the kernel.
        if hidden > 512 {
            return Err(ForgeError::Kernel(format!(
                "lstm_f32: hidden {hidden} exceeds shared-state capacity (512)"
            )));
        }
        let k = self.artifacts.get("lstm_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (hidden as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(yh)
            .buf(yc)
            .buf(x)
            .buf(w)
            .buf(r)
            .buf(b)
            .buf(h0)
            .buf(c0)
            .scalar(seq as i64)
            .scalar(input_size as i64)
            .scalar(hidden as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}
