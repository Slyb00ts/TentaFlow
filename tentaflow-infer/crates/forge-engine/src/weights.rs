// ===== File: weights.rs — model weight upload: GGUF / safetensors → device buffers =====
// Weight matrices stay in their storage quantization on the GPU (fused
// dequant-GEMV kernels read them directly); norms and the embedding table are
// materialized as f16 because they feed non-quantized kernels.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use forge_formats::nvfp4::{self, NvFp4Scheme, NvFp4TensorNames};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_formats::w4a8::{col_absmax, smoothing_scale, w4a8_pack_smoothed, W4A8_GROUP};
use forge_formats::{dequantize_to_f32, Gguf, HfConfig, LayerKind, ModelDescriptor, WeightRole};
use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{DType, ForgeError, MemKind, QuantKind, Result};
use half::f16;

/// A weight matrix on-device, tagged with how kernels must read it.
pub enum DevWeight {
    /// f16 row-major [rows, cols].
    F16 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q8_0 block stream for [rows, cols].
    Q8_0 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q4_K superblock stream (144 bytes / 256 elements) for [rows, cols].
    Q4K {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q6_K superblock stream (210 bytes / 256 elements) for [rows, cols].
    Q6K {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q5_K superblock stream (176 bytes / 256 elements) for [rows, cols].
    Q5K {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q3_K superblock stream (110 bytes / 256 elements) for [rows, cols].
    Q3K {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q2_K superblock stream (84 bytes / 256 elements) for [rows, cols].
    Q2K {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q4_0 block stream (18 bytes / 32 elements) for [rows, cols].
    Q4_0 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q4_1 block stream (20 bytes / 32 elements) for [rows, cols].
    Q4_1 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q5_0 block stream (22 bytes / 32 elements) for [rows, cols].
    Q5_0 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML Q5_1 block stream (24 bytes / 32 elements) for [rows, cols].
    Q5_1 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ4_NL block stream (18 bytes / 32 elements) for [rows, cols].
    Iq4Nl {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ4_XS superblock stream (136 bytes / 256 elements) for [rows, cols].
    Iq4Xs {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML MXFP4 block stream (17 bytes / 32 elements) for [rows, cols].
    Mxfp4 {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ2_XS superblock stream (74 bytes / 256 elements) for [rows, cols].
    Iq2Xs {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ2_S superblock stream (82 bytes / 256 elements) for [rows, cols].
    Iq2S {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ3_S superblock stream (110 bytes / 256 elements) for [rows, cols].
    Iq3S {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ2_XXS superblock stream (66 bytes / 256 elements) for [rows, cols].
    Iq2Xxs {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ3_XXS superblock stream (98 bytes / 256 elements) for [rows, cols].
    Iq3Xxs {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ1_S superblock stream (50 bytes / 256 elements) for [rows, cols].
    Iq1S {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// GGML IQ1_M superblock stream (56 bytes / 256 elements) for [rows, cols].
    Iq1M {
        buf: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// NVFP4 packed + FP8 scales (+ inverse global scale) for [rows, cols].
    NvFp4 {
        packed: DevBuffer,
        scales: DevBuffer,
        inv_global_scale: f32,
        rows: usize,
        cols: usize,
    },
}

impl DevWeight {
    pub fn rows(&self) -> usize {
        match self {
            DevWeight::F16 { rows, .. }
            | DevWeight::Q8_0 { rows, .. }
            | DevWeight::Q4K { rows, .. }
            | DevWeight::Q6K { rows, .. }
            | DevWeight::Q5K { rows, .. }
            | DevWeight::Q3K { rows, .. }
            | DevWeight::Q2K { rows, .. }
            | DevWeight::Q4_0 { rows, .. }
            | DevWeight::Q4_1 { rows, .. }
            | DevWeight::Q5_0 { rows, .. }
            | DevWeight::Q5_1 { rows, .. }
            | DevWeight::Iq4Nl { rows, .. }
            | DevWeight::Iq4Xs { rows, .. }
            | DevWeight::Mxfp4 { rows, .. }
            | DevWeight::Iq2Xs { rows, .. }
            | DevWeight::Iq2S { rows, .. }
            | DevWeight::Iq3S { rows, .. }
            | DevWeight::Iq2Xxs { rows, .. }
            | DevWeight::Iq3Xxs { rows, .. }
            | DevWeight::Iq1S { rows, .. }
            | DevWeight::Iq1M { rows, .. }
            | DevWeight::NvFp4 { rows, .. } => *rows,
        }
    }

    pub fn cols(&self) -> usize {
        match self {
            DevWeight::F16 { cols, .. }
            | DevWeight::Q8_0 { cols, .. }
            | DevWeight::Q4K { cols, .. }
            | DevWeight::Q6K { cols, .. }
            | DevWeight::Q5K { cols, .. }
            | DevWeight::Q3K { cols, .. }
            | DevWeight::Q2K { cols, .. }
            | DevWeight::Q4_0 { cols, .. }
            | DevWeight::Q4_1 { cols, .. }
            | DevWeight::Q5_0 { cols, .. }
            | DevWeight::Q5_1 { cols, .. }
            | DevWeight::Iq4Nl { cols, .. }
            | DevWeight::Iq4Xs { cols, .. }
            | DevWeight::Mxfp4 { cols, .. }
            | DevWeight::Iq2Xs { cols, .. }
            | DevWeight::Iq2S { cols, .. }
            | DevWeight::Iq3S { cols, .. }
            | DevWeight::Iq2Xxs { cols, .. }
            | DevWeight::Iq3Xxs { cols, .. }
            | DevWeight::Iq1S { cols, .. }
            | DevWeight::Iq1M { cols, .. }
            | DevWeight::NvFp4 { cols, .. } => *cols,
        }
    }
}

/// One QServe-packed W4A8 projection (int4 weights + per-group int8 secondary
/// scale/zero + per-channel f16 primary scale). Non-default prefill GEMM
/// (`FORGE_GEMM=w4a8`); the original Q4_K weight is kept alongside for decode +
/// the logit head, so this is an ADDITIONAL store, not a replacement.
pub struct W4A8Weight {
    pub qweight: DevBuffer,
    pub s2_scales: DevBuffer,
    pub s2_zeros: DevBuffer,
    pub s1_scales: DevBuffer,
    /// Per-input-channel SmoothQuant reciprocal `1/s` (f16 [cols]); the GEMM's
    /// activation quantizer multiplies each input channel by this before the
    /// int8 quant. All-ones when calibration hasn't run (identity).
    pub inv_smooth: DevBuffer,
    pub rows: usize,
    pub cols: usize,
}

/// W4A8 packs of every prefill projection in one dense layer. Each is its OWN
/// logical matrix (q/k/v and gate/up are NOT fused): QServe interleaves weights
/// and transposes per-group scales `[K/G][N]`, so a fused-row window would be a
/// non-contiguous column slice — splitting at load avoids any windowing.
pub struct W4A8Layer {
    pub q: W4A8Weight,
    pub k: W4A8Weight,
    pub v: W4A8Weight,
    pub attn_o: W4A8Weight,
    pub gate: W4A8Weight,
    pub up: W4A8Weight,
    pub down: W4A8Weight,
}

/// Whether the W4A8 prefill GEMM is selected (`FORGE_GEMM=w4a8`). Default and
/// any other value keep the native Mojo int8 Q4_K prefill path.
pub fn w4a8_enabled() -> bool {
    std::env::var("FORGE_GEMM").ok().as_deref() == Some("w4a8")
}

/// One fp8 (e4m3) projection: e4m3 weight bytes [N,K] + per-output-row f32
/// scale. Non-default prefill GEMM (`FORGE_GEMM=fp8`); the original Q4_K weight
/// stays alongside for decode + the logit head, so this is an ADDITIONAL store.
/// Because e4m3 is floating point, one per-row scale (not per-32-block like
/// int8) captures the row's magnitude spread — the block-to-block variation is
/// absorbed by e4m3's 4-bit exponent.
pub struct Fp8Weight {
    /// e4m3 bytes [rows, cols], row-major (one byte per weight).
    pub qweight: DevBuffer,
    /// Per-output-row dequant scale, f32 [rows]: `w ≈ scale[r] · e4m3[r,c]`.
    pub scales: DevBuffer,
    pub rows: usize,
    pub cols: usize,
}

/// fp8 packs of every prefill projection in one dense layer. Each is its own
/// logical matrix (q/k/v and gate/up are NOT fused): a fused-row window would
/// need a per-row-scale slice that the GEMM reads by absolute row, so splitting
/// at load keeps every pack self-contained.
pub struct Fp8Layer {
    pub q: Fp8Weight,
    pub k: Fp8Weight,
    pub v: Fp8Weight,
    pub attn_o: Fp8Weight,
    pub gate: Fp8Weight,
    pub up: Fp8Weight,
    pub down: Fp8Weight,
}

/// Whether an fp8 (e4m3) prefill GEMM is selected. `fp8` = the hand-written
/// single-PTX kernel; `fp8mod` = Modular's multistage cp.async kernel (faster,
/// docs/CODEGEN_PROOF.md Finding G). Both build the SAME e4m3 weight packs; only
/// the launched GEMM differs. Any other value keeps the native Mojo int8 Q4_K path.
pub fn fp8_enabled() -> bool {
    matches!(
        std::env::var("FORGE_GEMM").ok().as_deref(),
        Some("fp8") | Some("fp8mod")
    )
}

/// Whether the Modular multistage fp8 GEMM (`FORGE_GEMM=fp8mod`) is selected.
pub fn fp8_modular_enabled() -> bool {
    std::env::var("FORGE_GEMM").ok().as_deref() == Some("fp8mod")
}

/// Per-input-channel activation abs-max collected during the W4A8 calibration
/// pass (one vector per transformer layer, one set per linear-input point).
/// This is the SmoothQuant migration signal — statistics only, no original
/// fp16 weights required.
pub struct CalibStats {
    /// q/k/v input = attn-norm output [hidden].
    pub attn_in: Vec<Vec<f32>>,
    /// o_proj input = attention output [n_heads*head_dim].
    pub attn_out: Vec<Vec<f32>>,
    /// gate/up input = ffn-norm output [hidden].
    pub ffn_in: Vec<Vec<f32>>,
    /// down_proj input = SwiGLU output [intermediate].
    pub down_in: Vec<Vec<f32>>,
    /// SmoothQuant balance exponent (0.5 default).
    pub alpha: f32,
}

/// Q/K/V projections: one row-concatenated matrix when the three share a
/// storage format (single GEMV/GEMM launch, single copy in VRAM). When only
/// v differs (Q4_K_M stores attn_v as Q6_K), q and k still fuse into one
/// matrix and v stays separate — two launches instead of three. Fused row
/// order is q, then k, then v.
pub enum QkvWeights {
    Fused(DevWeight),
    FusedQk { qk: DevWeight, v: DevWeight },
    Split {
        q: DevWeight,
        k: DevWeight,
        v: DevWeight,
    },
}

/// SwiGLU gate/up projections; fused row order is gate, then up.
pub enum GateUpWeights {
    Fused(DevWeight),
    Split { gate: DevWeight, up: DevWeight },
}

/// A plain (dense) SwiGLU feed-forward block.
pub struct DenseFfn {
    pub gate_up: GateUpWeights,
    pub down: DevWeight,
}

/// A Mixture-of-Experts feed-forward block: a router plus stacked expert
/// projections (indexed per selected expert on the decode/prefill paths) and an
/// optional always-on shared expert.
pub struct MoeFfn {
    /// Router `ffn_gate_inp`, f16 [n_experts, hidden]; produces per-expert
    /// logits fed to the top-k softmax.
    pub router: DevWeight,
    /// Stacked expert gate/up projections, [n_experts*moe_inter, hidden].
    pub gate_exps: DevWeight,
    pub up_exps: DevWeight,
    /// Stacked expert down projection, [n_experts*hidden, moe_inter].
    pub down_exps: DevWeight,
    /// Shared always-on expert (Qwen-MoE / DeepSeek), added to every token.
    pub shared: Option<DenseFfn>,
    /// Per-token sigmoid gate on the shared expert (qwen35moe
    /// `ffn_gate_inp_shexp`, f16 [1, hidden] → one logit per token). `None`
    /// = the shared expert is added ungated (weight 1.0).
    pub shared_gate: Option<DevWeight>,
    pub n_experts: usize,
    pub n_experts_used: usize,
    pub moe_inter: usize,
    /// Renormalize the top-k routing weights to sum 1.
    pub norm_topk: bool,
}

/// Gated-DeltaNet (linear-attention) layer weights (qwen35moe hybrid stack).
/// Tensor roles mirror `qwen35moe.cpp build_layer_attn_linear`.
pub struct DeltaNetWeights {
    /// `wqkv` in-projection producing the mixed q|k|v conv stream
    /// ([conv_dim, hidden]).
    pub in_proj: DevWeight,
    /// `wqkv_gate` producing the output gate `z` ([value_dim, hidden]).
    pub gate_proj: DevWeight,
    /// Depthwise causal conv weight, f16 flattened `ssm_conv1d {d_conv, conv_dim}`.
    pub conv1d: DevBuffer,
    /// Time-step bias added before softplus, f16 [n_v_heads].
    pub dt_bias: DevBuffer,
    /// Log-decay scale `-exp(A_log)`, f16 [n_v_heads].
    pub a: DevBuffer,
    /// Per-head write-gate projection ([n_v_heads, hidden]).
    pub beta_proj: DevWeight,
    /// Per-head decay projection ([n_v_heads, hidden]).
    pub alpha_proj: DevWeight,
    /// Output gated-RMSNorm weight over head_v_dim, f16 [d_state].
    pub ssm_norm: DevBuffer,
    /// Output projection ([hidden, value_dim]).
    pub out_proj: DevWeight,
}

/// Feed-forward block: dense SwiGLU or routed Mixture-of-Experts. The MoE
/// variant is boxed — it is far larger than the dense one and rare per model.
pub enum LayerFfn {
    Dense(DenseFfn),
    Moe(Box<MoeFfn>),
}

/// Softmax self-attention weights for one layer.
pub struct AttnWeights {
    /// Optional per-head QK norms (qwen3); f16 vectors of head_dim.
    pub q_norm: Option<DevBuffer>,
    pub k_norm: Option<DevBuffer>,
    pub attn_qkv: QkvWeights,
    pub attn_o: DevWeight,
}

/// The token-mixing sublayer of a transformer block: standard softmax
/// attention, or (qwen35moe hybrid) Gated-DeltaNet linear attention. Non-hybrid
/// architectures are all `Attention`.
pub enum LayerMixer {
    Attention(Box<AttnWeights>),
    DeltaNet(Box<DeltaNetWeights>),
}

/// Per-layer weight set (roles resolved by the arch registry).
pub struct LayerWeights {
    pub attn_norm: DevBuffer,
    pub ffn_norm: DevBuffer,
    pub mixer: LayerMixer,
    pub ffn: LayerFfn,
}

impl LayerWeights {
    /// Attention sub-weights. The generic decode/prefill paths only run on
    /// all-attention (non-hybrid) models, so this never hits a DeltaNet layer;
    /// the hybrid paths dispatch on `mixer` directly.
    pub fn attn(&self) -> &AttnWeights {
        match &self.mixer {
            LayerMixer::Attention(a) => a,
            LayerMixer::DeltaNet(_) => {
                unreachable!("attention path reached a DeltaNet layer")
            }
        }
    }

    /// Dense FFN parts, or an error on a MoE layer — the dense decode/prefill
    /// paths (fused, rot, batched, separate) never run on a MoE model, which
    /// takes the dedicated routed path instead.
    pub fn dense_ffn(&self) -> Result<&DenseFfn> {
        match &self.ffn {
            LayerFfn::Dense(d) => Ok(d),
            LayerFfn::Moe(_) => Err(ForgeError::Unsupported(
                "MoE layer reached a dense FFN code path".into(),
            )),
        }
    }
}

pub struct ModelWeights {
    pub descriptor: ModelDescriptor,
    /// Token embedding table, always f16 [vocab, hidden] (gather kernel input).
    pub token_embd_f16: DevBuffer,
    /// Host-resident f16 embedding table [vocab*hidden] for the hybrid arch:
    /// its per-token gather runs on the host (one 4 KiB row/token), so the
    /// full ~1 GiB table need not occupy VRAM (`token_embd_f16` is then a small
    /// placeholder). `None` for models that gather on-device.
    pub token_embd_host: Option<Vec<f16>>,
    pub output_norm: DevBuffer,
    /// LM head. For tied embeddings this is a separate f16 view built from the
    /// same host data (kept simple; dedup is a later optimization).
    pub lm_head: DevWeight,
    pub layers: Vec<LayerWeights>,
    /// Layers whose Q/K/V (resp. gate/up) landed as one fused matrix — the
    /// rest fell back to q|k fusion with separate v, or fully split storage
    /// (format or NVFP4 global-scale mismatch).
    pub fused_qkv_layers: usize,
    pub fused_qk_layers: usize,
    pub fused_gate_up_layers: usize,
    /// Per-layer W4A8 packs for the prefill GEMM when `FORGE_GEMM=w4a8`, else
    /// `None`. Dense models only; the decode/logit paths keep the Q4_K weights.
    pub w4a8: Option<Vec<W4A8Layer>>,
    /// Per-layer fp8 (e4m3) packs for the prefill GEMM when `FORGE_GEMM=fp8`,
    /// else `None`. Dense models only; decode/logit keep the resident weights.
    pub fp8: Option<Vec<Fp8Layer>>,
    /// When `fp8` packs are resident, route the prefill GEMM to Modular's
    /// multistage kernel (`FORGE_GEMM=fp8mod`) instead of the hand kernel.
    pub fp8_modular: bool,
}

/// Source-agnostic host-side tensor fetch: (bytes, dtype, quant, dims).
trait TensorSource {
    fn fetch(&self, name: &str) -> Result<(Vec<u8>, DType, QuantKind, Vec<usize>)>;
    /// NVFP4 triple fetch; None when the tensor is not NVFP4-packed.
    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>>;
    /// compressed-tensors FP8 ("float-quantized"): f8e4m3 weight + sibling
    /// `<base>.weight_scale` (per-channel or per-tensor). None when absent.
    fn fetch_fp8(&self, name: &str) -> Result<Option<Fp8Host>>;
}

struct Fp8Host {
    weight: Vec<u8>,
    /// One scale per output row, or a single tensor-wide scale.
    scales: Vec<f32>,
    rows: usize,
    cols: usize,
}

struct NvFp4Host {
    packed: Vec<u8>,
    scales: Vec<u8>,
    global_scale: f32,
    rows: usize,
    cols: usize,
}

struct GgufSource<'a>(&'a Gguf);

impl TensorSource for GgufSource<'_> {
    fn fetch(&self, name: &str) -> Result<(Vec<u8>, DType, QuantKind, Vec<usize>)> {
        let t = self
            .0
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("missing tensor {name}")))?;
        let data = self.0.tensor_data(name)?.to_vec();
        // GGUF dims are innermost-first; matrices arrive as [cols, rows].
        let mut dims: Vec<usize> = t.dims.iter().map(|&d| d as usize).collect();
        dims.reverse();
        Ok((data, t.dtype, t.quant, dims))
    }

    fn fetch_nvfp4(&self, _name: &str) -> Result<Option<NvFp4Host>> {
        Ok(None)
    }

    fn fetch_fp8(&self, _name: &str) -> Result<Option<Fp8Host>> {
        Ok(None)
    }
}

struct StSource<'a> {
    st: &'a ShardedSafeTensors,
    scheme: Option<NvFp4Scheme>,
    /// compressed-tensors "float-quantized" (FP8 weights + scale siblings).
    fp8: bool,
}

impl TensorSource for StSource<'_> {
    fn fetch(&self, name: &str) -> Result<(Vec<u8>, DType, QuantKind, Vec<usize>)> {
        let t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("missing tensor {name}")))?;
        let data = self.st.data(name)?.to_vec();
        Ok((data, t.dtype, QuantKind::None, t.shape.clone()))
    }

    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>> {
        let Some(scheme) = &self.scheme else {
            return Ok(None);
        };
        let names = NvFp4TensorNames::for_weight(name)?;
        let Some(packed_t) = self.st.tensor(&names.packed) else {
            return Ok(None);
        };
        if scheme.group_size != 16 {
            return Err(ForgeError::Unsupported(format!(
                "nvfp4 group_size {} (kernel supports 16)",
                scheme.group_size
            )));
        }
        let rows = packed_t.shape[0];
        let cols = packed_t.shape[1] * 2;
        let packed = self.st.data(&names.packed)?.to_vec();
        let scales = self.st.data(&names.scale)?.to_vec();
        let gs_bytes = self.st.data(&names.global_scale)?;
        if gs_bytes.len() != 4 {
            return Err(ForgeError::Format(format!(
                "{}: expected one f32",
                names.global_scale
            )));
        }
        let global_scale = f32::from_le_bytes([gs_bytes[0], gs_bytes[1], gs_bytes[2], gs_bytes[3]]);
        Ok(Some(NvFp4Host {
            packed,
            scales,
            global_scale,
            rows,
            cols,
        }))
    }

    fn fetch_fp8(&self, name: &str) -> Result<Option<Fp8Host>> {
        if !self.fp8 {
            return Ok(None);
        }
        let Some(t) = self.st.tensor(name) else {
            return Ok(None);
        };
        if t.dtype != DType::F8E4M3 || t.shape.len() != 2 {
            return Ok(None);
        }
        let base = name.strip_suffix(".weight").unwrap_or(name);
        let scale_name = format!("{base}.weight_scale");
        let Some(scale_t) = self.st.tensor(&scale_name) else {
            return Err(ForgeError::Format(format!(
                "{name}: fp8 weight without {scale_name}"
            )));
        };
        let (rows, cols) = (t.shape[0], t.shape[1]);
        let scale_n = scale_t.numel();
        if scale_n != rows && scale_n != 1 {
            return Err(ForgeError::Format(format!(
                "{scale_name}: {scale_n} scales for {rows} rows (expect per-channel or per-tensor)"
            )));
        }
        let scale_bytes = self.st.data(&scale_name)?;
        let scales: Vec<f32> = match scale_t.dtype {
            DType::F32 => scale_bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            DType::BF16 => scale_bytes
                .chunks_exact(2)
                .map(|c| f32::from_bits(u32::from(u16::from_le_bytes([c[0], c[1]])) << 16))
                .collect(),
            DType::F16 => scale_bytes
                .chunks_exact(2)
                .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
                .collect(),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{scale_name}: scale dtype {other}"
                )))
            }
        };
        Ok(Some(Fp8Host {
            weight: self.st.data(name)?.to_vec(),
            scales,
            rows,
            cols,
        }))
    }
}

fn f32s_to_f16_bytes(vals: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 2);
    for &v in vals {
        out.extend_from_slice(&f16::from_f32(v).to_le_bytes());
    }
    out
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len(), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}

/// Upload a norm-style vector as f16 (dequantizing if needed).
fn upload_norm(device: &dyn Device, src: &dyn TensorSource, name: &str) -> Result<DevBuffer> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    let numel = dims.iter().product();
    let f32s = dequantize_to_f32(dtype, quant, &data, numel)?;
    upload(device, &f32s_to_f16_bytes(&f32s))
}

/// Load a 1-D tensor as a single-row f16 GEMV weight (`rows = 1`, `cols = n`).
/// Used for the qwen35moe shared-expert sigmoid gate (`ffn_gate_inp_shexp`).
fn load_vector_weight(
    device: &dyn Device,
    src: &dyn TensorSource,
    name: &str,
) -> Result<DevWeight> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    let numel: usize = dims.iter().product();
    let f32s = dequantize_to_f32(dtype, quant, &data, numel)?;
    Ok(DevWeight::F16 {
        buf: upload(device, &f32s_to_f16_bytes(&f32s))?,
        rows: 1,
        cols: numel,
    })
}

/// A weight matrix still on the host, in the exact byte layout the fused
/// kernels consume. Kept host-side long enough to row-concatenate sibling
/// projections (QKV, gate/up) before the single upload.
enum HostWeight {
    F16 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q8_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q6K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q3K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q2K {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q4_1 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5_0 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Q5_1 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq4Nl {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq4Xs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Mxfp4 {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2Xs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq3S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq2Xxs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq3Xxs {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq1S {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    Iq1M {
        data: Vec<u8>,
        rows: usize,
        cols: usize,
    },
    NvFp4 {
        packed: Vec<u8>,
        scales: Vec<u8>,
        global_scale: f32,
        rows: usize,
        cols: usize,
    },
}

impl HostWeight {
    fn rows(&self) -> usize {
        match self {
            HostWeight::F16 { rows, .. }
            | HostWeight::Q8_0 { rows, .. }
            | HostWeight::Q4K { rows, .. }
            | HostWeight::Q6K { rows, .. }
            | HostWeight::Q5K { rows, .. }
            | HostWeight::Q3K { rows, .. }
            | HostWeight::Q2K { rows, .. }
            | HostWeight::Q4_0 { rows, .. }
            | HostWeight::Q4_1 { rows, .. }
            | HostWeight::Q5_0 { rows, .. }
            | HostWeight::Q5_1 { rows, .. }
            | HostWeight::Iq4Nl { rows, .. }
            | HostWeight::Iq4Xs { rows, .. }
            | HostWeight::Mxfp4 { rows, .. }
            | HostWeight::Iq2Xs { rows, .. }
            | HostWeight::Iq2S { rows, .. }
            | HostWeight::Iq3S { rows, .. }
            | HostWeight::Iq2Xxs { rows, .. }
            | HostWeight::Iq3Xxs { rows, .. }
            | HostWeight::Iq1S { rows, .. }
            | HostWeight::Iq1M { rows, .. }
            | HostWeight::NvFp4 { rows, .. } => *rows,
        }
    }

    fn cols(&self) -> usize {
        match self {
            HostWeight::F16 { cols, .. }
            | HostWeight::Q8_0 { cols, .. }
            | HostWeight::Q4K { cols, .. }
            | HostWeight::Q6K { cols, .. }
            | HostWeight::Q5K { cols, .. }
            | HostWeight::Q3K { cols, .. }
            | HostWeight::Q2K { cols, .. }
            | HostWeight::Q4_0 { cols, .. }
            | HostWeight::Q4_1 { cols, .. }
            | HostWeight::Q5_0 { cols, .. }
            | HostWeight::Q5_1 { cols, .. }
            | HostWeight::Iq4Nl { cols, .. }
            | HostWeight::Iq4Xs { cols, .. }
            | HostWeight::Mxfp4 { cols, .. }
            | HostWeight::Iq2Xs { cols, .. }
            | HostWeight::Iq2S { cols, .. }
            | HostWeight::Iq3S { cols, .. }
            | HostWeight::Iq2Xxs { cols, .. }
            | HostWeight::Iq3Xxs { cols, .. }
            | HostWeight::Iq1S { cols, .. }
            | HostWeight::Iq1M { cols, .. }
            | HostWeight::NvFp4 { cols, .. } => *cols,
        }
    }
}

/// Fetch a weight matrix in the most direct form a kernel can consume.
fn fetch_matrix(src: &dyn TensorSource, name: &str) -> Result<HostWeight> {
    if let Some(fp8) = src.fetch_fp8(name)? {
        // v0 materializes FP8 as f16 (2 bytes/elem) — a fused f8 GEMV kernel
        // halves that later without touching this loader contract.
        let mut out = Vec::with_capacity(fp8.weight.len() * 2);
        for (i, &b) in fp8.weight.iter().enumerate() {
            let s = if fp8.scales.len() == 1 {
                fp8.scales[0]
            } else {
                fp8.scales[i / fp8.cols]
            };
            let v = nvfp4::f8e4m3_to_f32(b) * s;
            out.extend_from_slice(&f16::from_f32(v).to_le_bytes());
        }
        return Ok(HostWeight::F16 {
            data: out,
            rows: fp8.rows,
            cols: fp8.cols,
        });
    }
    if let Some(nv) = src.fetch_nvfp4(name)? {
        // Validate on CPU once so a corrupt checkpoint fails at load, not as
        // garbage tokens at runtime.
        nvfp4::dequantize_nvfp4(
            &nv.packed,
            &nv.scales,
            nv.global_scale,
            nv.rows,
            nv.cols,
            16,
        )?;
        return Ok(HostWeight::NvFp4 {
            packed: nv.packed,
            scales: nv.scales,
            global_scale: nv.global_scale,
            rows: nv.rows,
            cols: nv.cols,
        });
    }
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    quant_host_weight(name, data, dtype, quant, rows, cols)
}

/// Map a [rows, cols] block stream in storage quant `quant` to the on-device
/// HostWeight variant the fused kernels consume, dequantizing to f16 for any
/// format without a native GPU kernel. Shared by the plain 2D matrix path and
/// the flattened MoE expert-stack path.
fn quant_host_weight(
    name: &str,
    data: Vec<u8>,
    dtype: DType,
    quant: QuantKind,
    rows: usize,
    cols: usize,
) -> Result<HostWeight> {
    match quant {
        QuantKind::Q8_0 => Ok(HostWeight::Q8_0 { data, rows, cols }),
        // Whole 256-element superblocks per row keep every 144-byte block
        // 16-byte aligned for the fused kernels' wide loads (Q4K_MAX_SEGS in
        // gemv2.mojo bounds the shared x-sum staging).
        QuantKind::Q4K if cols.is_multiple_of(256) && cols <= 32768 => {
            Ok(HostWeight::Q4K { data, rows, cols })
        }
        // Q6_K superblocks are only 2-byte aligned (210 bytes); the kernels
        // load them as u16 lanes, so only whole superblocks per row matter.
        QuantKind::Q6K if cols.is_multiple_of(256) => Ok(HostWeight::Q6K { data, rows, cols }),
        // Q5_K shares Q4_K's 16-byte header and per-32-column x-sum staging
        // (same shared-memory bound); Q2_K stages per-16-column sums with the
        // same 32768-column ceiling; Q3_K has no x-sum staging.
        QuantKind::Q5K if cols.is_multiple_of(256) && cols <= 32768 => {
            Ok(HostWeight::Q5K { data, rows, cols })
        }
        QuantKind::Q3K if cols.is_multiple_of(256) => Ok(HostWeight::Q3K { data, rows, cols }),
        QuantKind::Q2K if cols.is_multiple_of(256) && cols <= 32768 => {
            Ok(HostWeight::Q2K { data, rows, cols })
        }
        // Legacy 32-element formats keep their storage quant on-device
        // (whole blocks per row is guaranteed by the block structure).
        QuantKind::Q4_0 => Ok(HostWeight::Q4_0 { data, rows, cols }),
        QuantKind::Q4_1 => Ok(HostWeight::Q4_1 { data, rows, cols }),
        QuantKind::Q5_0 => Ok(HostWeight::Q5_0 { data, rows, cols }),
        QuantKind::Q5_1 => Ok(HostWeight::Q5_1 { data, rows, cols }),
        QuantKind::IQ4NL => Ok(HostWeight::Iq4Nl { data, rows, cols }),
        QuantKind::IQ4XS if cols.is_multiple_of(256) => {
            Ok(HostWeight::Iq4Xs { data, rows, cols })
        }
        QuantKind::MXFP4 => Ok(HostWeight::Mxfp4 { data, rows, cols }),
        QuantKind::IQ2XS if cols.is_multiple_of(256) => {
            Ok(HostWeight::Iq2Xs { data, rows, cols })
        }
        QuantKind::IQ2S if cols.is_multiple_of(256) => Ok(HostWeight::Iq2S { data, rows, cols }),
        QuantKind::IQ3S if cols.is_multiple_of(256) => Ok(HostWeight::Iq3S { data, rows, cols }),
        QuantKind::IQ2XXS if cols.is_multiple_of(256) => {
            Ok(HostWeight::Iq2Xxs { data, rows, cols })
        }
        QuantKind::IQ3XXS if cols.is_multiple_of(256) => {
            Ok(HostWeight::Iq3Xxs { data, rows, cols })
        }
        QuantKind::IQ1S if cols.is_multiple_of(256) => Ok(HostWeight::Iq1S { data, rows, cols }),
        QuantKind::IQ1M if cols.is_multiple_of(256) => Ok(HostWeight::Iq1M { data, rows, cols }),
        // Everything else goes through f32 → f16. This covers F16/F32/BF16
        // directly and any other GGML quant via the CPU reference dequant —
        // correctness first; fused kernels for more quants land per PLAN.
        _ => {
            if quant != QuantKind::None {
                tracing::warn!(
                    "{name}: no native GPU kernels for {quant:?}, materializing as f16 \
                     ({rows}x{cols}; doubles VRAM vs quantized storage)"
                );
            }
            let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
            Ok(HostWeight::F16 {
                data: f32s_to_f16_bytes(&f32s),
                rows,
                cols,
            })
        }
    }
}

/// Fetch a stacked MoE expert tensor `[n_expert, a, b]` (quantized) and flatten
/// it to a single `[n_expert*a, b]` matrix. GGUF stores experts contiguously in
/// expert-major order, so the flattened byte stream IS a row-major matrix whose
/// expert `e` occupies rows `e*a .. e*a+a`; the per-expert GEMV then reads it
/// with a plain row-offset (byte offset `e*a` rows into the block stream).
fn fetch_expert_stack(src: &dyn TensorSource, name: &str) -> Result<HostWeight> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 3 {
        return Err(ForgeError::Format(format!(
            "{name}: expected stacked expert tensor [n_expert, inter, hidden], got {dims:?}"
        )));
    }
    // GGUF dims are innermost-first and `fetch` already reversed them, so
    // dims = [n_expert, a, b].
    let (n_expert, a, b) = (dims[0], dims[1], dims[2]);
    quant_host_weight(name, data, dtype, quant, n_expert * a, b)
}

/// Upload a host matrix as-is.
fn upload_weight(device: &dyn Device, w: HostWeight) -> Result<DevWeight> {
    match w {
        HostWeight::F16 { data, rows, cols } => Ok(DevWeight::F16 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q8_0 { data, rows, cols } => Ok(DevWeight::Q8_0 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q4K { data, rows, cols } => Ok(DevWeight::Q4K {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q6K { data, rows, cols } => Ok(DevWeight::Q6K {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q5K { data, rows, cols } => Ok(DevWeight::Q5K {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q3K { data, rows, cols } => Ok(DevWeight::Q3K {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q2K { data, rows, cols } => Ok(DevWeight::Q2K {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q4_0 { data, rows, cols } => Ok(DevWeight::Q4_0 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q4_1 { data, rows, cols } => Ok(DevWeight::Q4_1 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q5_0 { data, rows, cols } => Ok(DevWeight::Q5_0 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Q5_1 { data, rows, cols } => Ok(DevWeight::Q5_1 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq4Nl { data, rows, cols } => Ok(DevWeight::Iq4Nl {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq4Xs { data, rows, cols } => Ok(DevWeight::Iq4Xs {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Mxfp4 { data, rows, cols } => Ok(DevWeight::Mxfp4 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq2Xs { data, rows, cols } => Ok(DevWeight::Iq2Xs {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq2S { data, rows, cols } => Ok(DevWeight::Iq2S {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq3S { data, rows, cols } => Ok(DevWeight::Iq3S {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq2Xxs { data, rows, cols } => Ok(DevWeight::Iq2Xxs {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq3Xxs { data, rows, cols } => Ok(DevWeight::Iq3Xxs {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq1S { data, rows, cols } => Ok(DevWeight::Iq1S {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::Iq1M { data, rows, cols } => Ok(DevWeight::Iq1M {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        HostWeight::NvFp4 {
            packed,
            scales,
            global_scale,
            rows,
            cols,
        } => Ok(DevWeight::NvFp4 {
            packed: upload(device, &packed)?,
            scales: upload(device, &scales)?,
            inv_global_scale: 1.0 / global_scale,
            rows,
            cols,
        }),
    }
}

/// Dequantize a 2-D projection tensor to fp32 row-major `[rows, cols]`,
/// validating the W4A8 shape constraints. Format-agnostic: reads whatever the
/// resident checkpoint stores (Q4_K/Q6_K/…/fp16) — no original fp16 needed.
fn dequant_matrix_f32(src: &dyn TensorSource, name: &str) -> Result<(Vec<f32>, usize, usize)> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix for W4A8, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    if !rows.is_multiple_of(64) || !cols.is_multiple_of(128) {
        return Err(ForgeError::Unsupported(format!(
            "W4A8 needs rows % 64 == 0 && cols % 128 == 0, {name} is [{rows}, {cols}]"
        )));
    }
    let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    Ok((f32s, rows, cols))
}

/// Pack an already-dequantized fp32 projection to a SmoothQuant-migrated QServe
/// W4A8 pack and upload it (weights ×`smooth` per column; the GEMM applies
/// `1/smooth` to activations). `smooth` has `cols` entries.
fn pack_w4a8_from_f32(
    device: &dyn Device,
    w: &[f32],
    rows: usize,
    cols: usize,
    smooth: &[f32],
) -> Result<W4A8Weight> {
    let packed = w4a8_pack_smoothed(w, rows, cols, W4A8_GROUP, smooth);
    let s1_bytes: Vec<u8> = packed
        .s1_scales
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let inv_bytes: Vec<u8> = smooth
        .iter()
        .flat_map(|&s| f16::from_f32(1.0 / s).to_le_bytes())
        .collect();
    Ok(W4A8Weight {
        qweight: upload(device, &packed.qweight)?,
        s2_scales: upload(device, &packed.s2_scales)?,
        s2_zeros: upload(device, &packed.s2_zeros)?,
        s1_scales: upload(device, &s1_bytes)?,
        inv_smooth: upload(device, &inv_bytes)?,
        rows,
        cols,
    })
}

/// Dequantize a 2-D projection tensor to fp32 row-major `[rows, cols]` for the
/// fp8 pack. Format-agnostic; only requires `cols % 32 == 0` (the fp8 GEMM's K
/// tile), looser than the W4A8 shape constraint.
fn dequant_matrix_f32_fp8(src: &dyn TensorSource, name: &str) -> Result<(Vec<f32>, usize, usize)> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix for fp8, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    if !cols.is_multiple_of(32) {
        return Err(ForgeError::Unsupported(format!(
            "fp8 needs cols % 32 == 0, {name} is [{rows}, {cols}]"
        )));
    }
    let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    Ok((f32s, rows, cols))
}

/// Pack an already-dequantized fp32 projection to e4m3 with ONE scale per
/// output row (`scale[r] = absmax(row r) / 448`), then upload. Rows whose
/// weights are all zero get scale 0 (their e4m3 codes are zero too).
fn pack_fp8_from_f32(
    device: &dyn Device,
    w: &[f32],
    rows: usize,
    cols: usize,
) -> Result<Fp8Weight> {
    let mut codes = vec![0u8; rows * cols];
    let mut scales = vec![0f32; rows];
    for r in 0..rows {
        let row = &w[r * cols..(r + 1) * cols];
        let absmax = row.iter().fold(0f32, |m, &x| m.max(x.abs()));
        if absmax == 0.0 {
            continue;
        }
        let scale = absmax / 448.0;
        let inv = 448.0 / absmax;
        scales[r] = scale;
        let dst = &mut codes[r * cols..(r + 1) * cols];
        for (c, &x) in row.iter().enumerate() {
            dst[c] = forge_formats::nvfp4::f32_to_f8e4m3(x * inv);
        }
    }
    let scale_bytes: Vec<u8> = scales.iter().flat_map(|s| s.to_le_bytes()).collect();
    Ok(Fp8Weight {
        qweight: upload(device, &codes)?,
        scales: upload(device, &scale_bytes)?,
        rows,
        cols,
    })
}

/// Row-concatenate projection matrices into one [Σrows, cols] matrix. Every
/// supported format stores rows as independent contiguous byte runs (f16
/// elements, Q8_0 34-byte blocks, NVFP4 packed nibbles + FP8 scale bytes),
/// so fusion is a plain byte concat of each stream. Returns None when the
/// parts differ in format, or — for NVFP4 — in the tensor-wide global scale:
/// rescaling FP8 block scales to a common global would round and break
/// bit-exactness vs the unfused path, so such layers stay split.
fn fuse_rows(mut parts: Vec<HostWeight>) -> std::result::Result<HostWeight, Vec<HostWeight>> {
    let cols = parts[0].cols();
    if parts.iter().any(|p| p.cols() != cols) {
        return Err(parts);
    }
    match &parts[0] {
        HostWeight::F16 { .. } if parts.iter().all(|p| matches!(p, HostWeight::F16 { .. })) => {}
        HostWeight::Q8_0 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q8_0 { .. })) => {}
        HostWeight::Q4K { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q4K { .. })) => {}
        HostWeight::Q6K { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q6K { .. })) => {}
        HostWeight::Q5K { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q5K { .. })) => {}
        HostWeight::Q3K { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q3K { .. })) => {}
        HostWeight::Q2K { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q2K { .. })) => {}
        HostWeight::Q4_0 { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Q4_0 { .. })) => {}
        HostWeight::Q4_1 { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Q4_1 { .. })) => {}
        HostWeight::Q5_0 { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Q5_0 { .. })) => {}
        HostWeight::Q5_1 { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Q5_1 { .. })) => {}
        HostWeight::Iq4Nl { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq4Nl { .. })) => {}
        HostWeight::Iq4Xs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq4Xs { .. })) => {}
        HostWeight::Mxfp4 { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Mxfp4 { .. })) => {}
        HostWeight::Iq2Xs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq2Xs { .. })) => {}
        HostWeight::Iq2S { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq2S { .. })) => {}
        HostWeight::Iq3S { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq3S { .. })) => {}
        HostWeight::Iq2Xxs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq2Xxs { .. })) => {}
        HostWeight::Iq3Xxs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq3Xxs { .. })) => {}
        HostWeight::Iq1S { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq1S { .. })) => {}
        HostWeight::Iq1M { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq1M { .. })) => {}
        HostWeight::NvFp4 { global_scale, .. } => {
            let gs = global_scale.to_bits();
            let ok = parts.iter().all(
                |p| matches!(p, HostWeight::NvFp4 { global_scale, .. } if global_scale.to_bits() == gs),
            );
            if !ok {
                return Err(parts);
            }
        }
        _ => return Err(parts),
    }
    let rows = parts.iter().map(|p| p.rows()).sum();
    let mut fused = parts.remove(0);
    for p in parts {
        match (&mut fused, p) {
            (HostWeight::F16 { data, .. }, HostWeight::F16 { data: d, .. })
            | (HostWeight::Q8_0 { data, .. }, HostWeight::Q8_0 { data: d, .. })
            | (HostWeight::Q4K { data, .. }, HostWeight::Q4K { data: d, .. })
            | (HostWeight::Q6K { data, .. }, HostWeight::Q6K { data: d, .. })
            | (HostWeight::Q5K { data, .. }, HostWeight::Q5K { data: d, .. })
            | (HostWeight::Q3K { data, .. }, HostWeight::Q3K { data: d, .. })
            | (HostWeight::Q2K { data, .. }, HostWeight::Q2K { data: d, .. })
            | (HostWeight::Q4_0 { data, .. }, HostWeight::Q4_0 { data: d, .. })
            | (HostWeight::Q4_1 { data, .. }, HostWeight::Q4_1 { data: d, .. })
            | (HostWeight::Q5_0 { data, .. }, HostWeight::Q5_0 { data: d, .. })
            | (HostWeight::Q5_1 { data, .. }, HostWeight::Q5_1 { data: d, .. })
            | (HostWeight::Iq4Nl { data, .. }, HostWeight::Iq4Nl { data: d, .. })
            | (HostWeight::Iq4Xs { data, .. }, HostWeight::Iq4Xs { data: d, .. })
            | (HostWeight::Mxfp4 { data, .. }, HostWeight::Mxfp4 { data: d, .. })
            | (HostWeight::Iq2Xs { data, .. }, HostWeight::Iq2Xs { data: d, .. })
            | (HostWeight::Iq2S { data, .. }, HostWeight::Iq2S { data: d, .. })
            | (HostWeight::Iq3S { data, .. }, HostWeight::Iq3S { data: d, .. })
            | (HostWeight::Iq2Xxs { data, .. }, HostWeight::Iq2Xxs { data: d, .. })
            | (HostWeight::Iq3Xxs { data, .. }, HostWeight::Iq3Xxs { data: d, .. })
            | (HostWeight::Iq1S { data, .. }, HostWeight::Iq1S { data: d, .. })
            | (HostWeight::Iq1M { data, .. }, HostWeight::Iq1M { data: d, .. }) => {
                data.extend_from_slice(&d)
            }
            (
                HostWeight::NvFp4 { packed, scales, .. },
                HostWeight::NvFp4 {
                    packed: p2,
                    scales: s2,
                    ..
                },
            ) => {
                packed.extend_from_slice(&p2);
                scales.extend_from_slice(&s2);
            }
            _ => unreachable!("format equality checked above"),
        }
    }
    match &mut fused {
        HostWeight::F16 { rows: r, .. }
        | HostWeight::Q8_0 { rows: r, .. }
        | HostWeight::Q4K { rows: r, .. }
        | HostWeight::Q6K { rows: r, .. }
        | HostWeight::Q5K { rows: r, .. }
        | HostWeight::Q3K { rows: r, .. }
        | HostWeight::Q2K { rows: r, .. }
        | HostWeight::Q4_0 { rows: r, .. }
        | HostWeight::Q4_1 { rows: r, .. }
        | HostWeight::Q5_0 { rows: r, .. }
        | HostWeight::Q5_1 { rows: r, .. }
        | HostWeight::Iq4Nl { rows: r, .. }
        | HostWeight::Iq4Xs { rows: r, .. }
        | HostWeight::Mxfp4 { rows: r, .. }
        | HostWeight::Iq2Xs { rows: r, .. }
        | HostWeight::Iq2S { rows: r, .. }
        | HostWeight::Iq3S { rows: r, .. }
        | HostWeight::Iq2Xxs { rows: r, .. }
        | HostWeight::Iq3Xxs { rows: r, .. }
        | HostWeight::Iq1S { rows: r, .. }
        | HostWeight::Iq1M { rows: r, .. }
        | HostWeight::NvFp4 { rows: r, .. } => *r = rows,
    }
    Ok(fused)
}

/// Fetch the embedding table as a host-resident f16 vector (row-major
/// [vocab*hidden]); returns `(table, vocab, hidden)`.
fn fetch_embedding_host(
    src: &dyn TensorSource,
    name: &str,
) -> Result<(Vec<f16>, usize, usize)> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    let f16s = f32s.iter().map(|&v| f16::from_f32(v)).collect();
    Ok((f16s, rows, cols))
}

/// Upload the embedding table as f16 regardless of storage quant.
fn upload_embedding(
    device: &dyn Device,
    src: &dyn TensorSource,
    name: &str,
) -> Result<(DevBuffer, usize, usize)> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    Ok((upload(device, &f32s_to_f16_bytes(&f32s))?, rows, cols))
}

impl ModelWeights {
    pub fn load_gguf(device: &Arc<dyn Device>, path: &Path) -> Result<Self> {
        let gguf = Gguf::open(path)?;
        let descriptor = ModelDescriptor::detect(&gguf)?;
        let src = GgufSource(&gguf);
        Self::load(device.as_ref(), descriptor, &src)
    }

    pub fn load_safetensors_dir(device: &Arc<dyn Device>, dir: &Path) -> Result<Self> {
        let config: HfConfig = {
            let text = std::fs::read_to_string(dir.join("config.json"))?;
            serde_json::from_str::<HfConfig>(&text)
                .map_err(|e| ForgeError::Format(format!("config.json: {e}")))?
        };
        let descriptor = ModelDescriptor::from_hf(&config)?;
        let st = ShardedSafeTensors::load_dir(dir)?;
        let scheme = NvFp4Scheme::detect(&config);
        let fp8 = config
            .quantization_config
            .as_ref()
            .and_then(|qc| qc.get("format"))
            .and_then(|f| f.as_str())
            == Some("float-quantized");
        let src = StSource { st: &st, scheme, fp8 };
        Self::load(device.as_ref(), descriptor, &src)
    }

    fn load(
        device: &dyn Device,
        descriptor: ModelDescriptor,
        src: &dyn TensorSource,
    ) -> Result<Self> {
        // Hybrid attention/DeltaNet arches (qwen35moe) have per-layer weight
        // sets that differ by kind and a gated attention Q projection the
        // generic shape checks would reject; they take a dedicated loader.
        if descriptor.params.ssm.is_some() {
            if w4a8_enabled() {
                return Err(ForgeError::Unsupported(
                    "FORGE_GEMM=w4a8 supports dense (non-hybrid) models only".into(),
                ));
            }
            return Self::load_hybrid(device, descriptor, src);
        }
        let global = |role: WeightRole| -> Result<&String> {
            descriptor
                .globals
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("missing global weight {role:?}")))
        };

        let embd_name = global(WeightRole::TokenEmbd)?;
        let (token_embd_f16, vocab, hidden) = upload_embedding(device, src, embd_name)?;
        let output_norm = upload_norm(device, src, global(WeightRole::OutputNorm)?)?;

        let lm_head_name = descriptor
            .globals
            .get(&WeightRole::LmHead)
            .unwrap_or(embd_name);
        let lm_head = match fetch_matrix(src, lm_head_name)? {
            // The logit head needs an f32-output kernel, which exists for f16
            // and Q8_0 only — materialize an NVFP4 head as f16 instead of
            // failing at first token.
            HostWeight::NvFp4 {
                packed,
                scales,
                global_scale,
                rows,
                cols,
            } => {
                let f32s = nvfp4::dequantize_nvfp4(&packed, &scales, global_scale, rows, cols, 16)?;
                DevWeight::F16 {
                    buf: upload(device, &f32s_to_f16_bytes(&f32s))?,
                    rows,
                    cols,
                }
            }
            w => upload_weight(device, w)?,
        };
        if lm_head.rows() != vocab || lm_head.cols() != hidden {
            return Err(ForgeError::Format(format!(
                "lm_head shape [{}, {}] does not match embedding [{vocab}, {hidden}]",
                lm_head.rows(),
                lm_head.cols()
            )));
        }
        if vocab != descriptor.params.vocab_size || hidden != descriptor.params.hidden_size {
            return Err(ForgeError::Format(format!(
                "embedding [{vocab}, {hidden}] does not match model config [{}, {}]",
                descriptor.params.vocab_size, descriptor.params.hidden_size
            )));
        }

        // Shape validation: activation buffers are sized from the descriptor,
        // so any weight disagreeing with it would launch out-of-bounds GEMVs.
        // Validated host-side, before fusion hides the per-projection shapes.
        let p = &descriptor.params;
        let q_dim = p.n_heads * p.head_dim;
        let kv_dim = p.n_kv_heads * p.head_dim;
        let expect = |what: &str, w: &HostWeight, rows: usize, cols: usize| -> Result<()> {
            if w.rows() != rows || w.cols() != cols {
                return Err(ForgeError::Format(format!(
                    "{what}: shape [{}, {}] does not match model config [{rows}, {cols}]",
                    w.rows(),
                    w.cols()
                )));
            }
            Ok(())
        };

        // W4A8 requant is dense-only (per-logical-projection packs; the routed
        // MoE / hybrid stacks have no dense prefill projection set). Fail loudly
        // rather than silently falling back to Q4_K when the flag can't apply.
        // W4A8 packs are built AFTER load by `Model::calibrate_w4a8` (it needs a
        // running model to collect SmoothQuant activation statistics), so load
        // only validates applicability here and leaves `w4a8 = None`.
        let want_w4a8 = w4a8_enabled();
        if want_w4a8 && p.moe.is_some() {
            return Err(ForgeError::Unsupported(
                "FORGE_GEMM=w4a8 supports dense (non-MoE) models only".into(),
            ));
        }

        let mut layers = Vec::with_capacity(descriptor.params.block_count);
        let mut fused_qkv_layers = 0usize;
        let mut fused_qk_layers = 0usize;
        let mut fused_gate_up_layers = 0usize;
        for (idx, layer_map) in descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };
            let at = |what: &str| format!("layer {idx} {what}");

            let q = fetch_matrix(src, name(WeightRole::AttnQ)?)?;
            let k = fetch_matrix(src, name(WeightRole::AttnK)?)?;
            let v = fetch_matrix(src, name(WeightRole::AttnV)?)?;
            expect(&at("attn_q"), &q, q_dim, p.hidden_size)?;
            expect(&at("attn_k"), &k, kv_dim, p.hidden_size)?;
            expect(&at("attn_v"), &v, kv_dim, p.hidden_size)?;
            let attn_qkv = match fuse_rows(vec![q, k, v]) {
                Ok(fused) => {
                    fused_qkv_layers += 1;
                    QkvWeights::Fused(upload_weight(device, fused)?)
                }
                Err(mut parts) => {
                    let v = parts.pop().expect("three parts");
                    let k = parts.pop().expect("three parts");
                    let q = parts.pop().expect("three parts");
                    match fuse_rows(vec![q, k]) {
                        Ok(qk) => {
                            fused_qk_layers += 1;
                            QkvWeights::FusedQk {
                                qk: upload_weight(device, qk)?,
                                v: upload_weight(device, v)?,
                            }
                        }
                        Err(mut qk_parts) => {
                            let k = qk_parts.pop().expect("two parts");
                            let q = qk_parts.pop().expect("two parts");
                            QkvWeights::Split {
                                q: upload_weight(device, q)?,
                                k: upload_weight(device, k)?,
                                v: upload_weight(device, v)?,
                            }
                        }
                    }
                }
            };

            let attn_o = fetch_matrix(src, name(WeightRole::AttnO)?)?;
            expect(&at("attn_o"), &attn_o, p.hidden_size, q_dim)?;

            let ffn = match &p.moe {
                None => {
                    let gate = fetch_matrix(src, name(WeightRole::FfnGate)?)?;
                    let up = fetch_matrix(src, name(WeightRole::FfnUp)?)?;
                    expect(&at("ffn_gate"), &gate, p.intermediate_size, p.hidden_size)?;
                    expect(&at("ffn_up"), &up, p.intermediate_size, p.hidden_size)?;
                    let gate_up = match fuse_rows(vec![gate, up]) {
                        Ok(fused) => {
                            fused_gate_up_layers += 1;
                            GateUpWeights::Fused(upload_weight(device, fused)?)
                        }
                        Err(mut parts) => {
                            let up = parts.pop().expect("two parts");
                            let gate = parts.pop().expect("two parts");
                            GateUpWeights::Split {
                                gate: upload_weight(device, gate)?,
                                up: upload_weight(device, up)?,
                            }
                        }
                    };
                    let down = fetch_matrix(src, name(WeightRole::FfnDown)?)?;
                    expect(&at("ffn_down"), &down, p.hidden_size, p.intermediate_size)?;
                    LayerFfn::Dense(DenseFfn {
                        gate_up,
                        down: upload_weight(device, down)?,
                    })
                }
                Some(moe) => {
                    let router = fetch_matrix(src, name(WeightRole::FfnGateInp)?)?;
                    expect(&at("ffn_gate_inp"), &router, moe.n_experts, p.hidden_size)?;
                    let router = match router {
                        // The router kernel reads f16 weights; the loader
                        // materializes the (typically f32) gate as f16 already,
                        // so a non-f16 result would be a format surprise.
                        HostWeight::F16 { .. } => upload_weight(device, router)?,
                        other => {
                            return Err(ForgeError::Unsupported(format!(
                                "layer {idx} ffn_gate_inp must be f16-materializable, got {:?}",
                                std::mem::discriminant(&other)
                            )))
                        }
                    };
                    let gate_exps = fetch_expert_stack(src, name(WeightRole::FfnGateExps)?)?;
                    let up_exps = fetch_expert_stack(src, name(WeightRole::FfnUpExps)?)?;
                    let down_exps = fetch_expert_stack(src, name(WeightRole::FfnDownExps)?)?;
                    expect(
                        &at("ffn_gate_exps"),
                        &gate_exps,
                        moe.n_experts * moe.moe_intermediate_size,
                        p.hidden_size,
                    )?;
                    expect(
                        &at("ffn_up_exps"),
                        &up_exps,
                        moe.n_experts * moe.moe_intermediate_size,
                        p.hidden_size,
                    )?;
                    expect(
                        &at("ffn_down_exps"),
                        &down_exps,
                        moe.n_experts * p.hidden_size,
                        moe.moe_intermediate_size,
                    )?;
                    // Optional shared expert (all roles present together).
                    let shared = match (
                        layer_map.get(&WeightRole::FfnGateShExp),
                        layer_map.get(&WeightRole::FfnUpShExp),
                        layer_map.get(&WeightRole::FfnDownShExp),
                    ) {
                        (Some(gn), Some(un), Some(dn)) => {
                            let si = moe.shared_intermediate_size;
                            let gate = fetch_matrix(src, gn)?;
                            let up = fetch_matrix(src, un)?;
                            let down = fetch_matrix(src, dn)?;
                            expect(&at("ffn_gate_shexp"), &gate, si, p.hidden_size)?;
                            expect(&at("ffn_up_shexp"), &up, si, p.hidden_size)?;
                            expect(&at("ffn_down_shexp"), &down, p.hidden_size, si)?;
                            let gate_up = match fuse_rows(vec![gate, up]) {
                                Ok(fused) => GateUpWeights::Fused(upload_weight(device, fused)?),
                                Err(mut parts) => {
                                    let up = parts.pop().expect("two parts");
                                    let gate = parts.pop().expect("two parts");
                                    GateUpWeights::Split {
                                        gate: upload_weight(device, gate)?,
                                        up: upload_weight(device, up)?,
                                    }
                                }
                            };
                            Some(DenseFfn {
                                gate_up,
                                down: upload_weight(device, down)?,
                            })
                        }
                        _ => None,
                    };
                    LayerFfn::Moe(Box::new(MoeFfn {
                        router,
                        gate_exps: upload_weight(device, gate_exps)?,
                        up_exps: upload_weight(device, up_exps)?,
                        down_exps: upload_weight(device, down_exps)?,
                        shared,
                        shared_gate: None,
                        n_experts: moe.n_experts,
                        n_experts_used: moe.n_experts_used,
                        moe_inter: moe.moe_intermediate_size,
                        norm_topk: moe.norm_topk_prob,
                    }))
                }
            };

            layers.push(LayerWeights {
                attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
                ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
                mixer: LayerMixer::Attention(Box::new(AttnWeights {
                    q_norm: match layer_map.get(&WeightRole::AttnQNorm) {
                        Some(n) => Some(upload_norm(device, src, n)?),
                        None => None,
                    },
                    k_norm: match layer_map.get(&WeightRole::AttnKNorm) {
                        Some(n) => Some(upload_norm(device, src, n)?),
                        None => None,
                    },
                    attn_qkv,
                    attn_o: upload_weight(device, attn_o)?,
                })),
                ffn,
            });
        }

        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            token_embd_host: None,
            output_norm,
            lm_head,
            layers,
            fused_qkv_layers,
            fused_qk_layers,
            fused_gate_up_layers,
            w4a8: None,
            fp8: None,
            fp8_modular: false,
        })
    }

    /// Load the qwen35moe hybrid stack: interleaved gated-attention and
    /// Gated-DeltaNet layers, each with a routed + gated-shared MoE FFN. The
    /// attention Q projection is gated (width `2*n_heads*head_dim`) so it is
    /// stored split (no q/k/v fusion); DeltaNet layers carry the SSM weight set.
    fn load_hybrid(
        device: &dyn Device,
        descriptor: ModelDescriptor,
        src: &dyn TensorSource,
    ) -> Result<Self> {
        let global = |role: WeightRole| -> Result<&String> {
            descriptor
                .globals
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("missing global weight {role:?}")))
        };
        let embd_name = global(WeightRole::TokenEmbd)?;
        // The embedding table (~1 GiB f16) stays on the host so the 22 GB of
        // quantized weights fit VRAM; the gather runs host-side per token.
        let (host_embed, vocab, hidden) = fetch_embedding_host(src, embd_name)?;
        let token_embd_f16 = upload(device, &vec![0u8; hidden * 2])?;
        let output_norm = upload_norm(device, src, global(WeightRole::OutputNorm)?)?;
        let lm_head_name = descriptor
            .globals
            .get(&WeightRole::LmHead)
            .unwrap_or(embd_name);
        let lm_head = match fetch_matrix(src, lm_head_name)? {
            HostWeight::NvFp4 {
                packed,
                scales,
                global_scale,
                rows,
                cols,
            } => {
                let f32s = nvfp4::dequantize_nvfp4(&packed, &scales, global_scale, rows, cols, 16)?;
                DevWeight::F16 {
                    buf: upload(device, &f32s_to_f16_bytes(&f32s))?,
                    rows,
                    cols,
                }
            }
            w => upload_weight(device, w)?,
        };
        if vocab != descriptor.params.vocab_size || hidden != descriptor.params.hidden_size {
            return Err(ForgeError::Format(format!(
                "embedding [{vocab}, {hidden}] does not match model config [{}, {}]",
                descriptor.params.vocab_size, descriptor.params.hidden_size
            )));
        }

        let p = &descriptor.params;
        let moe = p.moe.clone().expect("hybrid model is MoE");

        let mut layers = Vec::with_capacity(p.block_count);
        for (idx, layer_map) in descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };

            let mixer = match descriptor.layer_kinds[idx] {
                LayerKind::Attention => {
                    let q = upload_weight(device, fetch_matrix(src, name(WeightRole::AttnQ)?)?)?;
                    let k = upload_weight(device, fetch_matrix(src, name(WeightRole::AttnK)?)?)?;
                    let v = upload_weight(device, fetch_matrix(src, name(WeightRole::AttnV)?)?)?;
                    let attn_o =
                        upload_weight(device, fetch_matrix(src, name(WeightRole::AttnO)?)?)?;
                    LayerMixer::Attention(Box::new(AttnWeights {
                        q_norm: Some(upload_norm(device, src, name(WeightRole::AttnQNorm)?)?),
                        k_norm: Some(upload_norm(device, src, name(WeightRole::AttnKNorm)?)?),
                        attn_qkv: QkvWeights::Split { q, k, v },
                        attn_o,
                    }))
                }
                LayerKind::DeltaNet => LayerMixer::DeltaNet(Box::new(DeltaNetWeights {
                    in_proj: upload_weight(device, fetch_matrix(src, name(WeightRole::SsmInProj)?)?)?,
                    gate_proj: upload_weight(device, fetch_matrix(src, name(WeightRole::SsmGate)?)?)?,
                    conv1d: upload_norm(device, src, name(WeightRole::SsmConv1d)?)?,
                    dt_bias: upload_norm(device, src, name(WeightRole::SsmDt)?)?,
                    a: upload_norm(device, src, name(WeightRole::SsmA)?)?,
                    beta_proj: upload_weight(device, fetch_matrix(src, name(WeightRole::SsmBeta)?)?)?,
                    alpha_proj: upload_weight(
                        device,
                        fetch_matrix(src, name(WeightRole::SsmAlpha)?)?,
                    )?,
                    ssm_norm: upload_norm(device, src, name(WeightRole::SsmNorm)?)?,
                    out_proj: upload_weight(device, fetch_matrix(src, name(WeightRole::SsmOut)?)?)?,
                })),
            };

            // Routed experts + always-on gated shared expert (every layer).
            let router = match fetch_matrix(src, name(WeightRole::FfnGateInp)?)? {
                r @ HostWeight::F16 { .. } => upload_weight(device, r)?,
                other => {
                    return Err(ForgeError::Unsupported(format!(
                        "layer {idx} ffn_gate_inp must be f16-materializable, got {:?}",
                        std::mem::discriminant(&other)
                    )))
                }
            };
            let gate_exps = fetch_expert_stack(src, name(WeightRole::FfnGateExps)?)?;
            let up_exps = fetch_expert_stack(src, name(WeightRole::FfnUpExps)?)?;
            let down_exps = fetch_expert_stack(src, name(WeightRole::FfnDownExps)?)?;
            let sh_gate = upload_weight(device, fetch_matrix(src, name(WeightRole::FfnGateShExp)?)?)?;
            let sh_up = upload_weight(device, fetch_matrix(src, name(WeightRole::FfnUpShExp)?)?)?;
            let sh_down = upload_weight(device, fetch_matrix(src, name(WeightRole::FfnDownShExp)?)?)?;
            let shared_gate = load_vector_weight(device, src, name(WeightRole::FfnGateInpShExp)?)?;

            let ffn = LayerFfn::Moe(Box::new(MoeFfn {
                router,
                gate_exps: upload_weight(device, gate_exps)?,
                up_exps: upload_weight(device, up_exps)?,
                down_exps: upload_weight(device, down_exps)?,
                shared: Some(DenseFfn {
                    gate_up: GateUpWeights::Split {
                        gate: sh_gate,
                        up: sh_up,
                    },
                    down: sh_down,
                }),
                shared_gate: Some(shared_gate),
                n_experts: moe.n_experts,
                n_experts_used: moe.n_experts_used,
                moe_inter: moe.moe_intermediate_size,
                norm_topk: moe.norm_topk_prob,
            }));

            layers.push(LayerWeights {
                attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
                ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
                mixer,
                ffn,
            });
        }

        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            token_embd_host: Some(host_embed),
            output_norm,
            lm_head,
            layers,
            fused_qkv_layers: 0,
            fused_qk_layers: 0,
            fused_gate_up_layers: 0,
            w4a8: None,
            fp8: None,
            fp8_modular: false,
        })
    }

    /// Rebuild every layer's W4A8 pack from the resident GGUF weights with
    /// SmoothQuant migration derived from a calibration pass. Format-agnostic in
    /// spirit — it operates on dequantized fp32 weights, so it works for any
    /// resident quant — with the GGUF re-open plumbing wired here (the dense
    /// W4A8 gate model is a Q4_K GGUF); other sources keep the identity path.
    ///
    /// q/k/v share their input (attn-norm output) so they share ONE migration
    /// vector (weight abs-max taken over all three); likewise gate/up share the
    /// ffn-norm output. o_proj and down_proj each smooth their own input.
    pub fn rebuild_w4a8_smoothed(
        &self,
        device: &dyn Device,
        path: &Path,
        stats: &CalibStats,
    ) -> Result<Vec<W4A8Layer>> {
        let gguf = Gguf::open(path)?;
        let src = GgufSource(&gguf);
        let alpha = stats.alpha;
        let mut out = Vec::with_capacity(self.descriptor.layers.len());
        for (idx, layer_map) in self.descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };
            let combine = |a: &mut Vec<f32>, b: Vec<f32>| {
                for (x, y) in a.iter_mut().zip(b) {
                    *x = x.max(y);
                }
            };

            // Q/K/V — one shared migration over the attn-norm output.
            let (q, qr, qc) = dequant_matrix_f32(&src, name(WeightRole::AttnQ)?)?;
            let (k, kr, kc) = dequant_matrix_f32(&src, name(WeightRole::AttnK)?)?;
            let (v, vr, vc) = dequant_matrix_f32(&src, name(WeightRole::AttnV)?)?;
            let mut wmax_qkv = col_absmax(&q, qr, qc);
            combine(&mut wmax_qkv, col_absmax(&k, kr, kc));
            combine(&mut wmax_qkv, col_absmax(&v, vr, vc));
            let s_qkv = smoothing_scale(&stats.attn_in[idx], &wmax_qkv, alpha);
            let q = pack_w4a8_from_f32(device, &q, qr, qc, &s_qkv)?;
            let k = pack_w4a8_from_f32(device, &k, kr, kc, &s_qkv)?;
            let v = pack_w4a8_from_f32(device, &v, vr, vc, &s_qkv)?;

            // o_proj — smooths the attention output.
            let (o, or, oc) = dequant_matrix_f32(&src, name(WeightRole::AttnO)?)?;
            let s_o = smoothing_scale(&stats.attn_out[idx], &col_absmax(&o, or, oc), alpha);
            let attn_o = pack_w4a8_from_f32(device, &o, or, oc, &s_o)?;

            // gate/up — one shared migration over the ffn-norm output.
            let (g, gr, gc) = dequant_matrix_f32(&src, name(WeightRole::FfnGate)?)?;
            let (u, ur, uc) = dequant_matrix_f32(&src, name(WeightRole::FfnUp)?)?;
            let mut wmax_gu = col_absmax(&g, gr, gc);
            combine(&mut wmax_gu, col_absmax(&u, ur, uc));
            let s_gu = smoothing_scale(&stats.ffn_in[idx], &wmax_gu, alpha);
            let gate = pack_w4a8_from_f32(device, &g, gr, gc, &s_gu)?;
            let up = pack_w4a8_from_f32(device, &u, ur, uc, &s_gu)?;

            // down_proj — smooths the SwiGLU output.
            let (d, dr, dc) = dequant_matrix_f32(&src, name(WeightRole::FfnDown)?)?;
            let s_down = smoothing_scale(&stats.down_in[idx], &col_absmax(&d, dr, dc), alpha);
            let down = pack_w4a8_from_f32(device, &d, dr, dc, &s_down)?;

            out.push(W4A8Layer {
                q,
                k,
                v,
                attn_o,
                gate,
                up,
                down,
            });
        }
        Ok(out)
    }

    /// Build every layer's fp8 (e4m3) pack from the resident GGUF weights. No
    /// calibration or SmoothQuant migration is needed: e4m3's floating-point
    /// range lets one per-row scale capture the weight distribution, so each
    /// projection is packed independently (dequant → per-row absmax → e4m3).
    /// Format-agnostic — operates on dequantized fp32, so any resident quant
    /// works; GGUF re-open is wired here (the dense fp8 gate model is Q4_K).
    pub fn rebuild_fp8(&self, device: &dyn Device, path: &Path) -> Result<Vec<Fp8Layer>> {
        let gguf = Gguf::open(path)?;
        let src = GgufSource(&gguf);
        let mut out = Vec::with_capacity(self.descriptor.layers.len());
        for (idx, layer_map) in self.descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };
            let pack = |role: WeightRole| -> Result<Fp8Weight> {
                let (w, r, c) = dequant_matrix_f32_fp8(&src, name(role)?)?;
                pack_fp8_from_f32(device, &w, r, c)
            };
            out.push(Fp8Layer {
                q: pack(WeightRole::AttnQ)?,
                k: pack(WeightRole::AttnK)?,
                v: pack(WeightRole::AttnV)?,
                attn_o: pack(WeightRole::AttnO)?,
                gate: pack(WeightRole::FfnGate)?,
                up: pack(WeightRole::FfnUp)?,
                down: pack(WeightRole::FfnDown)?,
            });
        }
        Ok(out)
    }

    /// Whether this model uses routed Mixture-of-Experts FFN blocks.
    pub fn is_moe(&self) -> bool {
        self.descriptor.params.moe.is_some()
    }

    /// Weight-role → tensor-name map used for diagnostics.
    pub fn describe(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("arch".into(), self.descriptor.arch.clone());
        if let Some(moe) = &self.descriptor.params.moe {
            m.insert("moe_experts".into(), moe.n_experts.to_string());
            m.insert("moe_experts_used".into(), moe.n_experts_used.to_string());
            m.insert(
                "moe_intermediate_size".into(),
                moe.moe_intermediate_size.to_string(),
            );
        }
        m.insert(
            "layers".into(),
            self.descriptor.params.block_count.to_string(),
        );
        m.insert(
            "fused_qkv_layers".into(),
            self.fused_qkv_layers.to_string(),
        );
        m.insert(
            "fused_qk_layers".into(),
            self.fused_qk_layers.to_string(),
        );
        m.insert(
            "fused_gate_up_layers".into(),
            self.fused_gate_up_layers.to_string(),
        );
        m
    }
}
