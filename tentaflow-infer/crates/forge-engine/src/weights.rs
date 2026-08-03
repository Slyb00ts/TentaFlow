// ===== File: weights.rs — model weight upload: GGUF / safetensors → device buffers =====
// Weight matrices stay in their storage quantization on the GPU (fused
// dequant-GEMV kernels read them directly); norms and the embedding table are
// materialized as f16 because they feed non-quantized kernels.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::expert_spill::ExpertSpill;
use crate::moe_residency::{ExpertBudget, ExpertStack, ExpertUsage};
use crate::mtp::{MtpEmbedding, MtpTensorLoader, MtpWeights};
use forge_formats::nvfp4::{self, NvFp4Scheme, NvFp4TensorNames};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_formats::w4a8::{col_absmax, smoothing_scale, w4a8_pack_smoothed, W4A8_GROUP};
use forge_formats::MtpWeightRole;
use forge_formats::{
    dequantize_to_f32, BlockMatrix, Gguf, HfConfig, LayerKind, ModelDescriptor,
    RoleShard, TpShard, WeightRole,
};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::{Kernels, Nvfp4GgufLayout};
use forge_types::{DType, ForgeError, MemKind, QuantKind, Result};
use half::f16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvFp4CtLayoutPolicy {
    Auto,
    RowMajorE4M3,
    S0N64K128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NvFp4CtLoadPlan {
    RowMajorE4M3,
    S0N64K128,
}

pub enum NvFp4CtStorage {
    RowMajorE4M3 {
        packed: DevBuffer,
        scales: DevBuffer,
    },
    S0N64K128 {
        data: DevBuffer,
    },
}

impl NvFp4CtStorage {
    pub fn row_major(&self) -> Result<(&DevBuffer, &DevBuffer)> {
        match self {
            Self::RowMajorE4M3 { packed, scales } => Ok((packed, scales)),
            Self::S0N64K128 { .. } => Err(ForgeError::Unsupported(
                "routing S0 N64/K128 nie jest jeszcze aktywny".into(),
            )),
        }
    }

    pub fn s0_data(&self) -> Result<&DevBuffer> {
        match self {
            Self::S0N64K128 { data } => Ok(data),
            Self::RowMajorE4M3 { .. } => Err(ForgeError::Unsupported(
                "waga NVFP4 nie używa układu S0 N64/K128".into(),
            )),
        }
    }
}

pub struct NvFp4CtRowWindow<'a> {
    data: &'a DevBuffer,
    physical_rows: usize,
    cols: usize,
    row_offset: usize,
    rows: usize,
}

impl NvFp4CtRowWindow<'_> {
    fn new(
        data: &DevBuffer,
        physical_rows: usize,
        cols: usize,
        row_offset: usize,
        rows: usize,
    ) -> Result<NvFp4CtRowWindow<'_>> {
        let expected_bytes = nvfp4_ct_s0_resident_bytes(physical_rows, cols)?;
        if data.len() != expected_bytes {
            return Err(ForgeError::Format(format!(
                "NVFP4 CT resident ma {} bajtów, oczekiwano {expected_bytes}",
                data.len()
            )));
        }
        let end = row_offset
            .checked_add(rows)
            .ok_or_else(|| ForgeError::Format("NVFP4 CT: przepełnienie okna wierszy".into()))?;
        if rows == 0
            || !row_offset.is_multiple_of(64)
            || !rows.is_multiple_of(64)
            || end > physical_rows
            || !cols.is_multiple_of(128)
        {
            return Err(ForgeError::Format(
                "NVFP4 CT wymaga niepustego okna wyrównanego do N64".into(),
            ));
        }
        Ok(NvFp4CtRowWindow {
            data,
            physical_rows,
            cols,
            row_offset,
            rows,
        })
    }

    pub fn data(&self) -> &DevBuffer {
        self.data
    }

    pub fn physical_rows(&self) -> usize {
        self.physical_rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn row_offset(&self) -> usize {
        self.row_offset
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}

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
        storage: NvFp4CtStorage,
        inv_global_scale: f32,
        rows: usize,
        cols: usize,
    },
    /// Wagi FP8 E4M3 z jedną skalą f32 na wiersz.
    Fp8Row {
        buf: DevBuffer,
        scales: DevBuffer,
        rows: usize,
        cols: usize,
    },
    /// Surowy strumień bloków GGUF NVFP4 (36 bajtów / 64 elementy).
    NvFp4Gguf {
        buf: DevBuffer,
        output_scale: f32,
        rows: usize,
        cols: usize,
        layout: Nvfp4GgufLayout,
    },
}

impl DevWeight {
    pub fn nvfp4_ct_row_window(
        &self,
        row_offset: usize,
        rows: usize,
    ) -> Result<NvFp4CtRowWindow<'_>> {
        let DevWeight::NvFp4 {
            storage,
            rows: physical_rows,
            cols,
            ..
        } = self
        else {
            return Err(ForgeError::Unsupported(
                "okno S0 wymaga wagi compressed-tensors NVFP4".into(),
            ));
        };
        let data = storage.s0_data()?;
        NvFp4CtRowWindow::new(data, *physical_rows, *cols, row_offset, rows)
    }

    /// Kwantyzacja wagi dla tych formatów, które umie dzielić tensor parallel.
    /// `None` znaczy „ten format nie ma ścieżki podziału", a nie „nieznany".
    pub fn split_quant(&self) -> Option<QuantKind> {
        match self {
            DevWeight::Q8_0 { .. } => Some(QuantKind::Q8_0),
            DevWeight::Q4K { .. } => Some(QuantKind::Q4K),
            DevWeight::Q6K { .. } => Some(QuantKind::Q6K),
            DevWeight::NvFp4Gguf { .. } => Some(QuantKind::NVFP4Gguf),
            _ => None,
        }
    }

    /// Jedyny bufor bajtów wagi. `None` dla formatów trzymających kilka
    /// buforów (compressed-tensors NVFP4 ma osobne pakiety i skale), które nie
    /// dają się opisać jednym wskaźnikiem bazowym.
    pub fn buffer(&self) -> Option<&DevBuffer> {
        match self {
            DevWeight::F16 { buf, .. }
            | DevWeight::Q8_0 { buf, .. }
            | DevWeight::Q4K { buf, .. }
            | DevWeight::Q6K { buf, .. }
            | DevWeight::Q5K { buf, .. }
            | DevWeight::Q3K { buf, .. }
            | DevWeight::Q2K { buf, .. }
            | DevWeight::Q4_0 { buf, .. }
            | DevWeight::Q4_1 { buf, .. }
            | DevWeight::Q5_0 { buf, .. }
            | DevWeight::Q5_1 { buf, .. }
            | DevWeight::Iq4Nl { buf, .. }
            | DevWeight::Iq4Xs { buf, .. }
            | DevWeight::Mxfp4 { buf, .. }
            | DevWeight::Iq2Xs { buf, .. }
            | DevWeight::Iq2S { buf, .. }
            | DevWeight::Iq3S { buf, .. }
            | DevWeight::Iq2Xxs { buf, .. }
            | DevWeight::Iq3Xxs { buf, .. }
            | DevWeight::Iq1S { buf, .. }
            | DevWeight::Iq1M { buf, .. }
            | DevWeight::NvFp4Gguf { buf, .. } => Some(buf),
            // Skale wierszowe są osobnym buforem, więc waga nie ma jednego
            // wskaźnika bazowego i nie wchodzi w rezydencję ekspertów.
            DevWeight::Fp8Row { .. } | DevWeight::NvFp4 { .. } => None,
        }
    }

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
            | DevWeight::NvFp4 { rows, .. }
            | DevWeight::NvFp4Gguf { rows, .. }
            | DevWeight::Fp8Row { rows, .. } => *rows,
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
            | DevWeight::NvFp4 { cols, .. }
            | DevWeight::NvFp4Gguf { cols, .. }
            | DevWeight::Fp8Row { cols, .. } => *cols,
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
#[derive(Clone)]
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

/// Paczki FP8 dla projekcji używanych przez hybrydowy prefill.
pub struct Fp8FfnLayer {
    pub q: Fp8Weight,
    /// K i V. Hybryda dlugo zostawiala je na NVFP4: profil prefillu Bielika
    /// pokazal 320 wywolan `nvfp4_ct_prefill_gemm` po 0.26 ms, czyli 33 TFLOPS
    /// tam, gdzie sciezka FP8 robi 142. Paczki dla nich to ~336 MB przy 7 GB
    /// calosci, wiec koszt pamieci jest marginalny.
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

/// Czy wybrano hybrydowy prefill FP8 tylko dla FFN.
pub fn fp8_ffn_modular_enabled() -> bool {
    matches!(
        std::env::var("FORGE_GEMM").ok().as_deref(),
        None | Some("fp8mod-ffn")
    )
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
    FusedQk {
        qk: DevWeight,
        v: DevWeight,
    },
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
    pub gate_exps: ExpertStack,
    pub up_exps: ExpertStack,
    /// Stacked expert down projection, [n_experts*hidden, moe_inter].
    pub down_exps: ExpertStack,
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
    /// Bias routera dodawany PRZED wyborem top-k, ale nie wchodzący do wag
    /// (DeepSeek V4, `noaux_tc`).
    pub gate_bias: Option<DevBuffer>,
    /// Tablica `token -> eksperci` dla warstw z routingiem haszowanym; gdy jest,
    /// zastępuje wybór po wyniku bramki, ale nie wagi.
    pub tid2eid: Option<DevBuffer>,
    /// Liczniki wyboru ekspertów tej warstwy — źródło danych dla rezydencji.
    pub usage: ExpertUsage,
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
    /// Gemma normalizuje V czystą normą RMS, bez wyuczonej wagi. Trzymamy tu
    /// wektor jedynek zamiast osobnego kernela bez wagi — wynik jest ten sam.
    pub v_norm: Option<DevBuffer>,
    pub attn_qkv: QkvWeights,
    pub attn_o: DevWeight,
}

/// The token-mixing sublayer of a transformer block: standard softmax
/// attention, or (qwen35moe hybrid) Gated-DeltaNet linear attention. Non-hybrid
/// architectures are all `Attention`.
/// Kompresor strumienia KV: uczony pooling bramkowany po oknie `compress_ratio`
/// tokenów. Przy stopniu 4 projekcje są DWA RAZY szersze — pierwsza połowa
/// wymiarów opisuje okno przesunięte o blok wstecz.
pub struct CompressorWeights {
    pub wkv: DevWeight,
    pub wgate: DevWeight,
    /// Kodowanie pozycji wewnątrz okna, `[ratio, szerokość projekcji]`.
    pub ape: DevBuffer,
    pub norm: DevBuffer,
}

/// Indekser rzadkiej uwagi: własny kompresor (z rotacją Hadamarda i
/// kwantyzacją FP4) plus projekcje zapytań i wag na głowicę.
pub struct IndexerWeights {
    pub wq_b: DevWeight,
    pub weights_proj: DevWeight,
    pub compressor: CompressorWeights,
}

/// Uwaga latentna DeepSeeka V4: Q i wyjście przez projekcje LoRA, pojedyncza
/// głowica KV, kotwica na głowicę, oraz — zależnie od warstwy — kompresor
/// strumienia KV i indekser rzadkiej uwagi.
pub struct DeepseekAttnWeights {
    pub wq_a: DevWeight,
    pub q_norm: DevBuffer,
    pub wq_b: DevWeight,
    pub wkv: DevWeight,
    pub kv_norm: DevBuffer,
    /// Zejście LoRA wyjścia, grupowane po `o_groups` blokach.
    pub wo_a: DevWeight,
    pub wo_b: DevWeight,
    /// Logit kotwicy, jeden na głowicę; wchodzi tylko do mianownika softmaxu.
    pub attn_sink: DevBuffer,
    pub compressor: Option<CompressorWeights>,
    pub indexer: Option<IndexerWeights>,
}

/// Wagi hyper-connections jednego miejsca wpięcia (uwaga albo FFN).
pub struct HyperConnectionWeights {
    /// `[mix_hc, hc * hidden]` — z niej powstają wagi redukcji, rozprowadzenia
    /// i macierz mieszająca.
    pub mix_fn: DevBuffer,
    pub base: DevBuffer,
    /// Trzy skale: dla redukcji, rozprowadzenia i macierzy.
    pub scale: DevBuffer,
}

pub enum LayerMixer {
    Attention(Box<AttnWeights>),
    DeltaNet(Box<DeltaNetWeights>),
    DeepseekAttention(Box<DeepseekAttnWeights>),
}

/// Per-layer weight set (roles resolved by the arch registry).
pub struct LayerWeights {
    pub attn_norm: DevBuffer,
    pub ffn_norm: DevBuffer,
    /// Normy "sandwich" nakładane na wyjście bloku PRZED dodaniem rezyduum
    /// (rodzina Gemma). Modele bez nich zostawiają te pola puste.
    pub post_attn_norm: Option<DevBuffer>,
    pub post_ffw_norm: Option<DevBuffer>,
    /// Skalar mnożący wyjście warstwy. Wczytywany raz, więc trzymamy go na
    /// hoście — nie ma powodu na dodatkowy odczyt z urządzenia w każdym kroku.
    pub layer_output_scale: Option<f32>,
    pub mixer: LayerMixer,
    pub ffn: LayerFfn,
    /// Hyper-connections: strumień rezydualny ma `hc_mult` kopii, a nie jedną.
    /// `Some` tylko dla architektur, które ich używają.
    pub hc_attn: Option<HyperConnectionWeights>,
    pub hc_ffn: Option<HyperConnectionWeights>,
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
            LayerMixer::DeepseekAttention(_) => {
                unreachable!("ścieżka uwagi trafiła na warstwę DeepSeeka V4")
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
    /// Dzielniki częstotliwości rope dla warstw globalnych (`rope_freqs`, f32).
    pub rope_freqs: Option<DevBuffer>,
    /// Jednoelementowy bufor f32 `-inf` — źródło maski tokenów zabronionych.
    pub neg_inf: Option<DevBuffer>,
    /// LM head. For tied embeddings this is a separate f16 view built from the
    /// same host data (kept simple; dedup is a later optimization).
    pub lm_head: DevWeight,
    /// Lm_head FP8 używany tylko przez single-stream decode w trybie opt-in.
    pub fp8_lm_head: Option<Fp8Weight>,
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
    /// Dodatkowe paczki FP8 dla Q/O i FFN w opt-in prefill.
    pub fp8_ffn: Option<Vec<Fp8FfnLayer>>,
    /// When `fp8` packs are resident, route the prefill GEMM to Modular's
    /// multistage kernel (`FORGE_GEMM=fp8mod`) instead of the hand kernel.
    pub fp8_modular: bool,
    /// Redukcja kopii strumienia w głowie wyjściowej (DeepSeek V4). Prostsza niż
    /// w bloku: sama sigmoida, bez Sinkhorna.
    pub hc_head: Option<HyperConnectionWeights>,
    /// Opcjonalna natywna warstwa NextN współdzieląca embedding i LM head targetu.
    pub mtp: Option<MtpWeights>,
    pub nvfp4_repacked_weights: usize,
}

/// Source-agnostic host-side tensor fetch: (bytes, dtype, quant, dims).
pub(crate) type TensorFetch = (Vec<u8>, DType, QuantKind, Vec<usize>);

pub(crate) trait TensorSource {
    /// Czy zrodlo trzyma wiersze Q/K w ORYGINALNEJ, przeplatanej kolejnosci
    /// rodziny Llama. GGUF tak; HF permutuje je juz przy konwersji, zeby moc
    /// liczyc rotacja NeoX. Kernele RoPE silnika sa NeoX, wiec przestawiac
    /// wiersze wolno TYLKO dla zrodel, ktore tego nie zrobily — inaczej
    /// permutacja zostaje nalozona dwa razy i model generuje smieci.
    fn stores_original_rope_order(&self) -> bool {
        false
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch>;
    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>>;
    /// NVFP4 triple fetch; None when the tensor is not NVFP4-packed.
    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>>;
    /// compressed-tensors FP8 ("float-quantized"): f8e4m3 weight + sibling
    /// `<base>.weight_scale` (per-channel or per-tensor). None when absent.
    fn fetch_fp8(&self, name: &str) -> Result<Option<Fp8Host>>;
    /// Wagi w układach swoistych dla DeepSeeka V4. `None` dla pozostałych
    /// źródeł i tensorów.
    fn fetch_deepseek(&self, _name: &str) -> Result<Option<HostWeight>> {
        Ok(None)
    }
    /// Rozmiar tensora na dysku, bez jego wczytywania. `None`, gdy źródło nie
    /// potrafi go podać — wtedy budżet rezydencji ekspertów jest nieznany.
    fn byte_len(&self, name: &str) -> Option<usize>;
}

struct SourceMtpLoader<'a> {
    device: &'a dyn Device,
    source: &'a dyn TensorSource,
    nvfp4_ct: Option<&'a NvFp4CtUploadContext<'a>>,
}

impl MtpTensorLoader for SourceMtpLoader<'_> {
    fn matrix(&mut self, name: &str, rows: usize, cols: usize) -> Result<DevWeight> {
        let weight = fetch_matrix(self.source, name)?;
        if weight.rows() != rows || weight.cols() != cols {
            return Err(ForgeError::Format(format!(
                "MTP {name}: kształt [{}, {}], wymagano [{rows}, {cols}]",
                weight.rows(),
                weight.cols()
            )));
        }
        upload_weight_with_nvfp4_ct(self.device, weight, self.nvfp4_ct)
    }

    fn matrix_q8(&mut self, name: &str, rows: usize, cols: usize) -> Result<DevWeight> {
        let weight = fetch_matrix(self.source, name)?;
        if weight.rows() != rows || weight.cols() != cols {
            return Err(ForgeError::Format(format!(
                "MTP {name}: kształt [{}, {}], wymagano [{rows}, {cols}]",
                weight.rows(),
                weight.cols()
            )));
        }
        if matches!(weight, HostWeight::Q8_0 { .. }) {
            return upload_weight_with_nvfp4_ct(self.device, weight, self.nvfp4_ct);
        }
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Format(format!(
                "MTP {name}: przekwantowanie do Q8_0 wymaga cols % 32 == 0, jest {cols}"
            )));
        }
        let (data, dtype, quant, _) = self.source.fetch(name)?;
        let values = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
        let buf = upload(self.device, &pack_q8_0(&values))?;
        Ok(DevWeight::Q8_0 { buf, rows, cols })
    }

    fn vector(&mut self, name: &str, len: usize) -> Result<DevBuffer> {
        let (data, dtype, quant, dims) = self.source.fetch(name)?;
        let elements = dims.iter().product::<usize>();
        if elements != len {
            return Err(ForgeError::Format(format!(
                "MTP {name}: długość {elements}, wymagano {len}"
            )));
        }
        let values = dequantize_to_f32(dtype, quant, &data, elements)?;
        upload(self.device, &f32s_to_f16_bytes(&values))
    }
}

/// Pakuje wartości f32 do bloków GGML Q8_0 (32 wartości, 34 B: skala f16 plus
/// 32 kody int8, `d = amax / 127`).
fn pack_q8_0(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() / 32 * 34);
    for block in values.chunks_exact(32) {
        let amax = block.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let d = amax / 127.0;
        out.extend_from_slice(&f16::from_f32(d).to_le_bytes());
        let inv = if d > 0.0 { 1.0 / d } else { 0.0 };
        for v in block {
            out.push(((v * inv).round().clamp(-127.0, 127.0) as i8) as u8);
        }
    }
    out
}

pub(crate) struct Fp8Host {
    weight: Vec<u8>,
    /// One scale per output row, or a single tensor-wide scale.
    scales: Vec<f32>,
    rows: usize,
    cols: usize,
}

pub(crate) struct NvFp4Host {
    packed: Vec<u8>,
    scales: Vec<u8>,
    global_scale: f32,
    rows: usize,
    cols: usize,
}

const NVFP4_CT_STAGING_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NvFp4CtUploadIdentity {
    names: Vec<String>,
    rows: usize,
    cols: usize,
    global_scale_bits: u32,
}

struct NvFp4CtPreflight {
    plan: NvFp4CtLoadPlan,
    max_cols: usize,
    expected_identities: HashSet<NvFp4CtUploadIdentity>,
}

struct NvFp4CtUploadContext<'a> {
    kernels: &'a Kernels,
    stream: &'a forge_hal::Stream,
    plan: NvFp4CtLoadPlan,
    packed_scratch: Option<DevBuffer>,
    scale_scratch: Option<DevBuffer>,
    chunk_rows: usize,
    expected_identities: HashSet<NvFp4CtUploadIdentity>,
    completed_identities: RefCell<HashSet<NvFp4CtUploadIdentity>>,
}

fn allocate_nvfp4_ct_scratch<T>(
    first_bytes: usize,
    second_bytes: usize,
    mut allocate: impl FnMut(usize) -> Result<T>,
    reset: impl FnOnce() -> Result<()>,
) -> Result<(T, T)> {
    let first = allocate(first_bytes)?;
    match allocate(second_bytes) {
        Ok(second) => Ok((first, second)),
        Err(error) => {
            drop(first);
            let _ = reset();
            Err(error)
        }
    }
}

fn resolve_nvfp4_ct_plan(
    policy: NvFp4CtLayoutPolicy,
    capable: bool,
    aligned: bool,
    tensors: usize,
) -> Result<NvFp4CtLoadPlan> {
    match policy {
        NvFp4CtLayoutPolicy::RowMajorE4M3 => Ok(NvFp4CtLoadPlan::RowMajorE4M3),
        NvFp4CtLayoutPolicy::Auto if capable && aligned && tensors > 0 => {
            Ok(NvFp4CtLoadPlan::S0N64K128)
        }
        NvFp4CtLayoutPolicy::Auto => Ok(NvFp4CtLoadPlan::RowMajorE4M3),
        NvFp4CtLayoutPolicy::S0N64K128 if !capable => Err(ForgeError::Unsupported(
            "S0 N64/K128 wymaga NVIDIA sm80 warp32 i pełnych 16 artefaktów".into(),
        )),
        NvFp4CtLayoutPolicy::S0N64K128 if !aligned || tensors == 0 => Err(ForgeError::Unsupported(
            "S0 N64/K128 wymaga wszystkich kształtów wyrównanych do N64/K128".into(),
        )),
        NvFp4CtLayoutPolicy::S0N64K128 => Ok(NvFp4CtLoadPlan::S0N64K128),
    }
}

fn validate_nvfp4_ct_scale_bytes(name: &str, scales: &[u8]) -> Result<()> {
    if let Some((index, byte)) = scales
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| nvfp4::nvfp4_ct_s0_from_e4m3(*byte).is_none())
    {
        return Err(ForgeError::Format(format!(
            "{name}: niedozwolona skala E4M3 0x{byte:02x} pod indeksem {index}"
        )));
    }
    Ok(())
}

fn nvfp4_ct_s0_resident_bytes(rows: usize, cols: usize) -> Result<usize> {
    if rows == 0 || cols == 0 || !rows.is_multiple_of(64) || !cols.is_multiple_of(128) {
        return Err(ForgeError::Format(format!(
            "NVFP4 CT S0 wymaga kształtu N64/K128, otrzymano [{rows}, {cols}]"
        )));
    }
    rows.checked_mul(cols)
        .and_then(|elements| elements.checked_mul(9))
        .and_then(|bytes| bytes.checked_div(16))
        .ok_or_else(|| ForgeError::Format("NVFP4 CT: przepełnienie resident".into()))
}

fn validate_nvfp4_ct_packed_metadata(
    name: &str,
    dtype: DType,
    shape: &[usize],
    group_size: usize,
) -> Result<(usize, usize)> {
    if dtype != DType::U8 || shape.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: oczekiwano macierzy U8 2D"
        )));
    }
    let rows = shape[0];
    let cols = shape[1]
        .checked_mul(2)
        .ok_or_else(|| ForgeError::Format(format!("{name}: przepełnienie liczby kolumn")))?;
    if rows == 0 || cols == 0 || !cols.is_multiple_of(group_size) {
        return Err(ForgeError::Format(format!(
            "{name}: niedozwolony kształt [{rows}, {cols}]"
        )));
    }
    Ok((rows, cols))
}

fn validate_nvfp4_ct_scale_metadata(
    name: &str,
    dtype: DType,
    shape: &[usize],
    rows: usize,
    cols: usize,
) -> Result<usize> {
    let expected_shape = [rows, cols / 16];
    if dtype != DType::F8E4M3 || shape != expected_shape {
        return Err(ForgeError::Format(format!(
            "{name}: oczekiwano F8E4M3 o kształcie {expected_shape:?}"
        )));
    }
    rows.checked_mul(cols / 16)
        .ok_or_else(|| ForgeError::Format(format!("{name}: przepełnienie rozmiaru")))
}

fn validate_nvfp4_ct_global_scale_metadata(
    name: &str,
    dtype: DType,
    shape: &[usize],
    bytes: &[u8],
) -> Result<f32> {
    if dtype != DType::F32 || shape != [1] || bytes.len() != 4 {
        return Err(ForgeError::Format(format!(
            "{name}: oczekiwano F32 o kształcie [1]"
        )));
    }
    let global_scale = f32::from_le_bytes(
        bytes
            .try_into()
            .expect("sprawdzona długość skali globalnej"),
    );
    if !global_scale.is_finite() || global_scale <= 0.0 {
        return Err(ForgeError::Format(format!(
            "{name}: skala musi być dodatnia i skończona"
        )));
    }
    Ok(global_scale)
}

fn validate_nvfp4_ct_companions(name: &str, present: [bool; 3]) -> Result<bool> {
    if present.iter().any(|value| *value) && !present.iter().all(|value| *value) {
        return Err(ForgeError::Format(format!(
            "{name}: niepełna trójka packed/scale/global_scale"
        )));
    }
    Ok(present.iter().all(|value| *value))
}

fn validate_nvfp4_ct_upload_manifest(
    plan: NvFp4CtLoadPlan,
    expected: &HashSet<NvFp4CtUploadIdentity>,
    completed: &HashSet<NvFp4CtUploadIdentity>,
) -> Result<()> {
    if plan == NvFp4CtLoadPlan::S0N64K128 && completed != expected {
        return Err(ForgeError::Format(
            "NVFP4 CT: wykonany zbiór uploadów różni się od preflight".into(),
        ));
    }
    Ok(())
}

fn nvfp4_ct_upload_identity(
    names: Vec<String>,
    rows: usize,
    cols: usize,
    global_scale_bits: u32,
) -> NvFp4CtUploadIdentity {
    NvFp4CtUploadIdentity {
        names,
        rows,
        cols,
        global_scale_bits,
    }
}

fn nvfp4_ct_physical_manifest(
    descriptor: &ModelDescriptor,
    native_mtp: bool,
) -> Result<Vec<Vec<String>>> {
    let mut manifest = Vec::new();
    for (index, layer) in descriptor.layers.iter().enumerate() {
        let name = |role: WeightRole| {
            layer
                .get(&role)
                .cloned()
                .ok_or_else(|| ForgeError::Format(format!("warstwa {index}: brak roli {role:?}")))
        };
        manifest.push(vec![
            name(WeightRole::AttnQ)?,
            name(WeightRole::AttnK)?,
            name(WeightRole::AttnV)?,
        ]);
        manifest.push(vec![name(WeightRole::FfnGate)?, name(WeightRole::FfnUp)?]);
        manifest.push(vec![name(WeightRole::FfnDown)?]);
        manifest.push(vec![name(WeightRole::AttnO)?]);
    }
    if native_mtp {
        let mtp = descriptor
            .mtp
            .as_ref()
            .ok_or_else(|| ForgeError::Unsupported("model nie zawiera głowy MTP/NextN".into()))?;
        let matrix_roles = [
            MtpWeightRole::AttnQ,
            MtpWeightRole::AttnK,
            MtpWeightRole::AttnV,
            MtpWeightRole::AttnO,
            MtpWeightRole::FfnGate,
            MtpWeightRole::FfnUp,
            MtpWeightRole::FfnDown,
            MtpWeightRole::EhProj,
        ];
        for (index, layer) in mtp.layers.iter().enumerate() {
            if index == 0 {
                for role in [MtpWeightRole::Embedding, MtpWeightRole::SharedHead] {
                    if let Some(name) = layer.get(&role) {
                        manifest.push(vec![name.clone()]);
                    }
                }
            }
            for role in matrix_roles {
                let name = layer.get(&role).cloned().ok_or_else(|| {
                    ForgeError::Format(format!("MTP warstwa {index}: brak roli {role:?}"))
                })?;
                manifest.push(vec![name]);
            }
        }
    }
    Ok(manifest)
}

fn plan_nvfp4_ct_safetensors(
    st: &ShardedSafeTensors,
    scheme: Option<&NvFp4Scheme>,
    descriptor: &ModelDescriptor,
    native_mtp: bool,
    policy: NvFp4CtLayoutPolicy,
    capable: bool,
) -> Result<NvFp4CtPreflight> {
    if descriptor.params.moe.is_some() || descriptor.params.ssm.is_some() {
        let plan = match policy {
            NvFp4CtLayoutPolicy::S0N64K128 => {
                return Err(ForgeError::Unsupported(
                    "S0 N64/K128 nie obsługuje jeszcze loadera MoE ani hybrid".into(),
                ))
            }
            _ => NvFp4CtLoadPlan::RowMajorE4M3,
        };
        return Ok(NvFp4CtPreflight {
            plan,
            max_cols: 0,
            expected_identities: HashSet::new(),
        });
    }
    let Some(scheme) = scheme else {
        if policy == NvFp4CtLayoutPolicy::S0N64K128 {
            return Err(ForgeError::Unsupported(
                "wymuszony S0 wymaga checkpointu compressed-tensors NVFP4".into(),
            ));
        }
        return Ok(NvFp4CtPreflight {
            plan: NvFp4CtLoadPlan::RowMajorE4M3,
            max_cols: 0,
            expected_identities: HashSet::new(),
        });
    };
    if scheme.group_size != 16 {
        return Err(ForgeError::Unsupported(format!(
            "NVFP4 CT wymaga group_size=16, otrzymano {}",
            scheme.group_size
        )));
    }
    let manifest = nvfp4_ct_physical_manifest(descriptor, native_mtp)?;
    let mut eligible = true;
    let mut max_cols = 0usize;
    let mut aligned = true;
    let mut expected_identities = HashSet::new();
    for group in &manifest {
        let mut group_rows = 0usize;
        let mut group_cols = None;
        let mut group_scale = None;
        let mut complete = true;
        for weight_name in group {
            let names = NvFp4TensorNames::for_weight(weight_name)?;
            let present = [
                st.tensor(&names.packed).is_some(),
                st.tensor(&names.scale).is_some(),
                st.tensor(&names.global_scale).is_some(),
            ];
            if !validate_nvfp4_ct_companions(weight_name, present)? {
                complete = false;
                continue;
            }
            let packed = st
                .tensor(&names.packed)
                .expect("sprawdzona obecność tensora packed");
            let (rows, cols) = validate_nvfp4_ct_packed_metadata(
                &names.packed,
                packed.dtype,
                &packed.shape,
                scheme.group_size,
            )?;
            let scale_tensor = st
                .tensor(&names.scale)
                .expect("sprawdzona obecność tensora scale");
            let expected_scales = validate_nvfp4_ct_scale_metadata(
                &names.scale,
                scale_tensor.dtype,
                &scale_tensor.shape,
                rows,
                cols,
            )?;
            let scales = st.data(&names.scale)?;
            if scales.len() != expected_scales {
                return Err(ForgeError::Format(format!(
                    "{}: {} bajtów, oczekiwano {expected_scales}",
                    names.scale,
                    scales.len()
                )));
            }
            validate_nvfp4_ct_scale_bytes(&names.scale, scales)?;
            let global_tensor = st
                .tensor(&names.global_scale)
                .expect("sprawdzona obecność tensora global_scale");
            let global_bytes = st.data(&names.global_scale)?;
            let global_scale = validate_nvfp4_ct_global_scale_metadata(
                &names.global_scale,
                global_tensor.dtype,
                &global_tensor.shape,
                global_bytes,
            )?;
            group_rows = group_rows.checked_add(rows).ok_or_else(|| {
                ForgeError::Format(format!("{weight_name}: przepełnienie fused rows"))
            })?;
            if group_cols
                .replace(cols)
                .is_some_and(|previous| previous != cols)
                || group_scale
                    .replace(global_scale.to_bits())
                    .is_some_and(|previous| previous != global_scale.to_bits())
            {
                complete = false;
            }
        }
        if complete {
            let cols = group_cols.expect("pełna grupa ma kolumny");
            aligned &= group_rows.is_multiple_of(64) && cols.is_multiple_of(128);
            max_cols = max_cols.max(cols);
            let identity = nvfp4_ct_upload_identity(
                group.clone(),
                group_rows,
                cols,
                group_scale.expect("pełna grupa ma skalę globalną"),
            );
            if !expected_identities.insert(identity) {
                return Err(ForgeError::Format(
                    "NVFP4 CT: zduplikowana tożsamość fizycznej wagi".into(),
                ));
            }
        } else {
            eligible = false;
        }
    }
    let plan = resolve_nvfp4_ct_plan(
        policy,
        capable,
        aligned && eligible,
        expected_identities.len(),
    )?;
    Ok(NvFp4CtPreflight {
        plan,
        max_cols,
        expected_identities,
    })
}

fn create_nvfp4_ct_upload_context<'a>(
    device: &dyn Device,
    kernels: &'a Kernels,
    stream: &'a forge_hal::Stream,
    preflight: NvFp4CtPreflight,
) -> Result<NvFp4CtUploadContext<'a>> {
    if preflight.plan == NvFp4CtLoadPlan::RowMajorE4M3 {
        return Ok(NvFp4CtUploadContext {
            kernels,
            stream,
            plan: preflight.plan,
            packed_scratch: None,
            scale_scratch: None,
            chunk_rows: 0,
            expected_identities: HashSet::new(),
            completed_identities: RefCell::new(HashSet::new()),
        });
    }
    let row_bytes = preflight
        .max_cols
        .checked_mul(9)
        .and_then(|bytes| bytes.checked_div(16))
        .ok_or_else(|| ForgeError::Format("NVFP4 CT: przepełnienie stagingu".into()))?;
    if row_bytes == 0 {
        return Err(ForgeError::Format(
            "NVFP4 CT: zerowy rozmiar wiersza stagingu".into(),
        ));
    }
    let chunk_rows = (NVFP4_CT_STAGING_BYTES / row_bytes / 64) * 64;
    if chunk_rows == 0 {
        return Err(ForgeError::Unsupported(
            "NVFP4 CT: pojedynczy kafel N64 przekracza limit stagingu".into(),
        ));
    }
    let packed_bytes = chunk_rows
        .checked_mul(preflight.max_cols / 2)
        .ok_or_else(|| ForgeError::Format("NVFP4 CT: przepełnienie stagingu packed".into()))?;
    let scale_bytes = chunk_rows
        .checked_mul(preflight.max_cols / 16)
        .ok_or_else(|| ForgeError::Format("NVFP4 CT: przepełnienie stagingu scales".into()))?;
    if packed_bytes + scale_bytes > NVFP4_CT_STAGING_BYTES {
        return Err(ForgeError::Format(
            "NVFP4 CT: staging przekracza ustalony limit".into(),
        ));
    }
    let resident_bytes =
        preflight
            .expected_identities
            .iter()
            .try_fold(0usize, |total, identity| {
                total
                    .checked_add(nvfp4_ct_s0_resident_bytes(identity.rows, identity.cols)?)
                    .ok_or_else(|| {
                        ForgeError::Format("NVFP4 CT: przepełnienie sumy resident".into())
                    })
            })?;
    tracing::info!(
        expected_uploads = preflight.expected_identities.len(),
        resident_bytes,
        staging_peak_bytes = packed_bytes + scale_bytes,
        chunk_rows,
        "plan uploadu NVFP4 CT S0"
    );
    let (packed_scratch, scale_scratch) = allocate_nvfp4_ct_scratch(
        packed_bytes,
        scale_bytes,
        |bytes| device.alloc(bytes, MemKind::Device, Pool::Activations),
        || device.reset_activations().map(|_| ()),
    )?;
    Ok(NvFp4CtUploadContext {
        kernels,
        stream,
        plan: preflight.plan,
        packed_scratch: Some(packed_scratch),
        scale_scratch: Some(scale_scratch),
        chunk_rows,
        expected_identities: preflight.expected_identities,
        completed_identities: RefCell::new(HashSet::new()),
    })
}

fn finish_nvfp4_ct_load<T>(load: Result<T>, reset: Result<()>) -> Result<T> {
    match (load, reset) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

struct GgufSource<'a>(&'a Gguf);

impl TensorSource for GgufSource<'_> {
    fn stores_original_rope_order(&self) -> bool {
        true
    }

    fn byte_len(&self, name: &str) -> Option<usize> {
        self.0.tensor(name).map(|t| t.size_bytes)
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch> {
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

    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>> {
        if self.0.tensor(name).is_none() {
            return Ok(None);
        }
        self.fetch(name).map(Some)
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
    /// Checkpoint DeepSeeka V4: eksperci NVFP4 pod nazwą `weight` ze skalą
    /// globalną `weight_scale_2` (MNOŻĄCĄ), a pozostałe wagi FP8 ze skalą
    /// kafelkową w siostrzanym `.scale`. Oba układy różnią się od
    /// compressed-tensors na tyle, że wspólna ścieżka dałaby ciche śmieci.
    deepseek_v4: bool,
}

/// Wczytuje wagę DeepSeeka V4 w postaci, którą kernel przyjmie wprost.
/// `None`, gdy tensor nie pasuje do żadnego z dwóch układów tego checkpointu.
fn fetch_deepseek_weight(st: &ShardedSafeTensors, name: &str) -> Result<Option<HostWeight>> {
    let Some(info) = st.tensor(name) else {
        return Ok(None);
    };
    if info.shape.len() != 2 {
        return Ok(None);
    }
    let base = name.strip_suffix(".weight").unwrap_or(name);

    // Ekspert NVFP4: pakiet U8 plus skale co 16 elementów i skala globalna.
    if info.dtype == DType::U8 {
        let names = nvfp4::DeepseekNvFp4Names::for_weight(name)?;
        let scales = st.data(&names.scale)?;
        let global_bytes = st.data(&names.global_scale)?;
        if global_bytes.len() != 4 {
            return Err(ForgeError::Format(format!(
                "{}: oczekiwano skalarnej skali f32",
                names.global_scale
            )));
        }
        let global = f32::from_le_bytes([
            global_bytes[0],
            global_bytes[1],
            global_bytes[2],
            global_bytes[3],
        ]);
        let packed = st.data(&names.packed)?;
        let repacked = nvfp4::deepseek_expert_to_gguf(packed, &info.shape, scales, global)?;
        return Ok(Some(HostWeight::NvFp4Gguf {
            data: repacked.blocks,
            output_scale: repacked.output_scale,
            rows: repacked.rows,
            cols: repacked.cols,
        }));
    }

    // Waga FP8 z kafelkową skalą E8M0: skala idzie na wiersze, a różnica
    // wykładników wtapia się w bajty E4M3. Zmierzony błąd wyjścia projekcji to
    // 4,7e-7 przy jednym bajcie na wagę — wobec 5,4e-3 dla przekwantyzowania na
    // Q8_0 i 13,7 GiB dla materializacji do f16.
    if info.dtype == DType::F8E4M3 {
        let scale_name = format!("{base}.scale");
        let Some(scale_info) = st.tensor(&scale_name) else {
            return Ok(None);
        };
        let (rows, cols) = (info.shape[0], info.shape[1]);
        if scale_info.shape.len() != 2 || scale_info.shape[1] == 0 {
            return Err(ForgeError::Format(format!(
                "{scale_name}: oczekiwano dwuwymiarowej skali kafelkowej"
            )));
        }
        let tile = cols / scale_info.shape[1];
        if tile == 0 || rows.div_ceil(tile) != scale_info.shape[0] {
            return Err(ForgeError::Format(format!(
                "{scale_name}: kafel {tile} nie zgadza się z kształtem {:?}",
                scale_info.shape
            )));
        }
        let (data, scales) = nvfp4::deepseek_fp8_to_row_scaled(
            st.data(name)?,
            st.data(&scale_name)?,
            rows,
            cols,
            tile,
        )?;
        return Ok(Some(HostWeight::Fp8Row {
            data,
            scales,
            rows,
            cols,
        }));
    }

    Ok(None)
}

impl TensorSource for StSource<'_> {
    fn byte_len(&self, name: &str) -> Option<usize> {
        let info = self.st.tensor(name)?;
        Some(info.numel() * info.dtype.size())
    }

    fn fetch_deepseek(&self, name: &str) -> Result<Option<HostWeight>> {
        if !self.deepseek_v4 {
            return Ok(None);
        }
        fetch_deepseek_weight(self.st, name)
    }

    fn fetch(&self, name: &str) -> Result<TensorFetch> {
        let t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("missing tensor {name}")))?;
        let data = self.st.data(name)?.to_vec();
        Ok((data, t.dtype, QuantKind::None, t.shape.clone()))
    }

    fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>> {
        if self.st.tensor(name).is_none() {
            return Ok(None);
        }
        self.fetch(name).map(Some)
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

/// Wgrywa bajty do wskazanej warstwy pamięci. Rezydencja ekspertów wybiera
/// warstwę sama, więc nie może polegać na automatycznym zsuwaniu do hosta —
/// to ono rozdzielało pamięć w kolejności ładowania.
fn upload_as(device: &dyn Device, bytes: &[u8], kind: MemKind) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len(), kind, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len(), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}

fn nvfp4_gguf_output_scale(src: &dyn TensorSource, name: &str, quant: QuantKind) -> Result<f32> {
    if quant != QuantKind::NVFP4Gguf {
        return Ok(1.0);
    }
    let Some(base) = name.strip_suffix(".weight") else {
        return Ok(1.0);
    };
    let scale_name = format!("{base}.scale");
    let Some((scale_data, scale_dtype, scale_quant, scale_dims)) =
        src.fetch_optional(&scale_name)?
    else {
        return Ok(1.0);
    };
    let numel: usize = scale_dims.iter().product();
    if numel != 1 {
        return Err(ForgeError::Format(format!(
            "{scale_name}: oczekiwano skalarnej skali NVFP4, otrzymano {scale_dims:?}"
        )));
    }
    let scale = dequantize_to_f32(scale_dtype, scale_quant, &scale_data, numel)?[0];
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ForgeError::Format(format!(
            "{scale_name}: skala NVFP4 musi być skończona i dodatnia, otrzymano {scale}"
        )));
    }
    Ok(scale)
}

/// Upload a norm-style vector as f16 (dequantizing if needed).
/// Odczytuje jednoelementowy tensor jako skalar hosta (np. `layer_output_scale`).
fn load_scalar_f32(src: &dyn TensorSource, name: &str) -> Result<f32> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    let numel: usize = dims.iter().product();
    if numel != 1 {
        return Err(ForgeError::Unsupported(format!(
            "tensor {name} miał mieć 1 element, ma {numel}"
        )));
    }
    Ok(dequantize_to_f32(dtype, quant, &data, numel)?[0])
}

/// Wczytuje tensor jako f32 na urządzenie (dzielniki rope są f32, nie f16).
fn upload_f32(device: &dyn Device, src: &dyn TensorSource, name: &str) -> Result<DevBuffer> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    let numel = dims.iter().product();
    let f32s = dequantize_to_f32(dtype, quant, &data, numel)?;
    upload(device, bytemuck::cast_slice(&f32s))
}

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

/// Wycinek macierzy hosta dla jednej rangi tensor-parallel.
///
/// Cięcie idzie na BAJTACH, bez dekwantyzacji: wiersze leżą ciągle w każdym
/// formacie blokowym, a wycinek kolumn pada na granicę bloku (wymusza to
/// `BlockMatrix`). Format wraca ten sam, więc pocięty tensor wchodzi w zwykłą
/// ścieżkę uploadu i nie potrzebuje osobnych kerneli.
fn shard_host_weight(weight: HostWeight, plan: &RoleShard) -> Result<HostWeight> {
    if matches!(plan, RoleShard::Replicated) {
        return Ok(weight);
    }
    let (data, rows, cols, quant, output_scale) = match weight {
        HostWeight::F16 { data, rows, cols } => (data, rows, cols, QuantKind::None, 1.0),
        HostWeight::Q8_0 { data, rows, cols } => (data, rows, cols, QuantKind::Q8_0, 1.0),
        HostWeight::Q4K { data, rows, cols } => (data, rows, cols, QuantKind::Q4K, 1.0),
        HostWeight::Q6K { data, rows, cols } => (data, rows, cols, QuantKind::Q6K, 1.0),
        HostWeight::NvFp4Gguf {
            data,
            output_scale,
            rows,
            cols,
        } => (data, rows, cols, QuantKind::NVFP4Gguf, output_scale),
        _ => {
            return Err(ForgeError::Unsupported(
                "podział tensor-parallel obejmuje F16, Q8_0, Q4_K, Q6_K i GGUF NVFP4".into(),
            ));
        }
    };
    let matrix = BlockMatrix {
        data: &data,
        rows,
        cols,
        quant,
    };
    let (sliced, out_rows, out_cols) = match plan {
        RoleShard::Replicated => unreachable!("obsłużone wyżej"),
        RoleShard::Rows(ranges) => {
            let taken = ranges.iter().map(|(_, count)| count).sum::<usize>();
            (matrix.take_rows(ranges)?, taken, cols)
        }
        RoleShard::Cols(ranges) => {
            let taken = ranges.iter().map(|(_, count)| count).sum::<usize>();
            (matrix.take_cols(ranges)?, rows, taken)
        }
    };
    Ok(match quant {
        QuantKind::None => HostWeight::F16 {
            data: sliced,
            rows: out_rows,
            cols: out_cols,
        },
        QuantKind::Q8_0 => HostWeight::Q8_0 {
            data: sliced,
            rows: out_rows,
            cols: out_cols,
        },
        QuantKind::Q4K => HostWeight::Q4K {
            data: sliced,
            rows: out_rows,
            cols: out_cols,
        },
        QuantKind::Q6K => HostWeight::Q6K {
            data: sliced,
            rows: out_rows,
            cols: out_cols,
        },
        QuantKind::NVFP4Gguf => HostWeight::NvFp4Gguf {
            data: sliced,
            output_scale,
            rows: out_rows,
            cols: out_cols,
        },
        other => {
            return Err(ForgeError::Unsupported(format!(
                "podział tensor-parallel nie zna formatu {other:?}"
            )));
        }
    })
}

/// Wektor f32/f16 (normy, `conv1d`, `ssm_a`, `ssm_dt`) pocięty tym samym planem.
///
/// `row_width` to liczba wartości przypadająca na jednostkę planu: `d_conv` dla
/// splotu, jeden dla wektorów per głowica. Plan liczy jednostki, nie wartości —
/// dzięki temu ten sam zakres opisuje wiersze `in_proj` i kanały `conv1d`.
fn upload_norm_shard(
    device: &dyn Device,
    src: &dyn TensorSource,
    name: &str,
    plan: &RoleShard,
    row_width: usize,
) -> Result<DevBuffer> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    let numel = dims.iter().product();
    let f32s = dequantize_to_f32(dtype, quant, &data, numel)?;
    let RoleShard::Rows(ranges) = plan else {
        if matches!(plan, RoleShard::Replicated) {
            return upload(device, &f32s_to_f16_bytes(&f32s));
        }
        return Err(ForgeError::Unsupported(
            "wektor normy dzieli się wyłącznie po jednostkach planu".into(),
        ));
    };
    if row_width == 0 || !numel.is_multiple_of(row_width) {
        return Err(ForgeError::Format(format!(
            "{name}: {numel} wartości nie jest wielokrotnością {row_width}"
        )));
    }
    let mut out = Vec::with_capacity(
        ranges.iter().map(|(_, count)| count * row_width).sum::<usize>(),
    );
    for &(first, count) in ranges {
        let from = first * row_width;
        let to = (first + count) * row_width;
        if to > f32s.len() {
            return Err(ForgeError::Format(format!(
                "{name}: zakres [{from}, {to}) wykracza poza {} wartości",
                f32s.len()
            )));
        }
        out.extend_from_slice(&f32s[from..to]);
    }
    upload(device, &f32s_to_f16_bytes(&out))
}

/// A weight matrix still on the host, in the exact byte layout the fused
/// kernels consume. Kept host-side long enough to row-concatenate sibling
/// projections (QKV, gate/up) before the single upload.
pub(crate) enum HostWeight {
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
        names: Vec<String>,
        packed: Vec<u8>,
        scales: Vec<u8>,
        global_scale: f32,
        rows: usize,
        cols: usize,
    },
    NvFp4Gguf {
        data: Vec<u8>,
        output_scale: f32,
        rows: usize,
        cols: usize,
    },
    /// Wagi FP8 E4M3 z jedną skalą na wiersz. DeepSeek V4 trzyma na dysku skalę
    /// kafelkową; loader przenosi ją na wiersze, wtapiając różnicę wykładników
    /// w same bajty (patrz `nvfp4::deepseek_fp8_to_row_scaled`).
    Fp8Row {
        data: Vec<u8>,
        scales: Vec<f32>,
        rows: usize,
        cols: usize,
    },
}

impl HostWeight {
    fn mtp_device_bytes(&self) -> Option<usize> {
        match self {
            HostWeight::Q8_0 { data, .. }
            | HostWeight::Q4K { data, .. }
            | HostWeight::NvFp4Gguf { data, .. } => Some(data.len()),
            _ => None,
        }
    }

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
            | HostWeight::NvFp4 { rows, .. }
            | HostWeight::NvFp4Gguf { rows, .. }
            | HostWeight::Fp8Row { rows, .. } => *rows,
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
            | HostWeight::NvFp4 { cols, .. }
            | HostWeight::NvFp4Gguf { cols, .. }
            | HostWeight::Fp8Row { cols, .. } => *cols,
        }
    }
}


impl HostWeight {
    /// Bufory tego formatu ułożone WIERSZOWO, wraz z krokiem wiersza w bajtach.
    ///
    /// Każdy format odpowiada tu za siebie, zamiast być wyliczany w cudzym
    /// `match` razem z dwudziestoma innymi. Tamten kształt kosztował: NVFP4
    /// compressed-tensors wpadał w gałąź „nieobsługiwane", bo trzyma wartości i
    /// skale w DWÓCH buforach i nie pasował do wzorca „jeden bufor".
    ///
    /// Zwraca liczbę wierszy i listę widoków; operacja wierszowa (dziś
    /// permutacja RoPE) przechodzi po wszystkich, każdy ze swoim krokiem.
    fn row_views_mut(&mut self) -> Result<(usize, Vec<(&mut Vec<u8>, usize)>)> {
        // Formaty jednobuforowe: krok wiersza wynika z rozmiaru bufora.
        macro_rules! single {
            ($data:expr, $rows:expr) => {{
                let rows = $rows;
                let data = $data;
                if rows == 0 || !data.len().is_multiple_of(rows) {
                    return Err(ForgeError::Format(
                        "rozmiar macierzy nie dzieli się na równe wiersze".into(),
                    ));
                }
                let row_bytes = data.len() / rows;
                Ok((rows, vec![(data, row_bytes)]))
            }};
        }
        match self {
            HostWeight::F16 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q8_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q6K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q3K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q2K { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q4_1 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5_0 { data, rows, .. } => single!(data, *rows),
            HostWeight::Q5_1 { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq4Nl { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq4Xs { data, rows, .. } => single!(data, *rows),
            HostWeight::Mxfp4 { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2Xs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq3S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq2Xxs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq3Xxs { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq1S { data, rows, .. } => single!(data, *rows),
            HostWeight::Iq1M { data, rows, .. } => single!(data, *rows),
            HostWeight::NvFp4Gguf { data, rows, .. } => single!(data, *rows),
            // NVFP4 compressed-tensors: wartości i skale osobno. Wiersze są
            // niezależne w obu (bloki biegną wzdłuż kolumn), więc ta sama
            // permutacja nałożona na oba daje ten sam wynik co dla formatów
            // jednobuforowych — bez dekwantyzacji.
            HostWeight::NvFp4 {
                packed,
                scales,
                rows,
                cols,
                ..
            } => {
                let (rows, cols) = (*rows, *cols);
                Ok((rows, vec![(packed, cols / 2), (scales, cols / 16)]))
            }
            _ => Err(ForgeError::Unsupported(
                "ten format wag nie deklaruje układu wierszowego".into(),
            )),
        }
    }
}

/// Przestawia wiersze Q/K tak, żeby kernel RoPE w stylu NeoX policzył rotację
/// PRZEPLATANĄ, której wymaga rodzina Llama.
///
/// NeoX obraca pary `(j, j + d/2)`, styl przeplatany pary `(2i, 2i+1)`. Po
/// przestawieniu wierszy w kolejność `[0, 2, 4, …, 1, 3, 5, …]` obie definicje
/// dają ten sam wynik przy tej samej częstotliwości `i`. Iloczyn skalarny Q·K
/// jest niewrażliwy na permutację wymiarów zastosowaną do OBU macierzy, a V i
/// projekcja wyjściowa nie są ruszane — więc uwaga wychodzi bitowo ta sama, co
/// przy natywnym kernelu przeplatanym.
///
/// Wiersze są niezależne w każdym formacie blokowym (bloki idą wzdłuż kolumn),
/// więc permutacja działa bez dekwantyzacji.
fn permute_rope_pairs(weight: &mut HostWeight, head_dim: usize) -> Result<()> {
    if head_dim == 0 || !head_dim.is_multiple_of(2) {
        return Err(ForgeError::Format(format!(
            "RoPE przeplatany wymaga parzystego head_dim, jest {head_dim}"
        )));
    }
    let (rows, views) = weight.row_views_mut()?;
    if rows == 0 || !rows.is_multiple_of(head_dim) {
        return Err(ForgeError::Format(format!(
            "liczba wierszy {rows} nie jest wielokrotnością head_dim {head_dim}"
        )));
    }
    for (data, row_bytes) in views {
        permute_rows(data, rows, row_bytes, head_dim)?;
    }
    Ok(())
}

/// Przestawia wiersze bufora w kolejność `[0, 2, 4, …, 1, 3, 5, …]` w obrębie
/// każdej głowy. Wydzielone z `permute_rope_pairs`, bo formaty trzymające
/// wartości i skale osobno muszą przepuścić przez to OBA bufory, każdy ze swoim
/// krokiem wiersza.
fn permute_rows(data: &mut Vec<u8>, rows: usize, row_bytes: usize, head_dim: usize) -> Result<()> {
    if data.len() != rows * row_bytes {
        return Err(ForgeError::Format(format!(
            "bufor {} B nie odpowiada {rows} wierszom po {row_bytes} B",
            data.len()
        )));
    }
    let half = head_dim / 2;
    let mut out = vec![0u8; data.len()];
    for head in 0..rows / head_dim {
        let base = head * head_dim;
        for i in 0..half {
            let src_even = (base + 2 * i) * row_bytes;
            let src_odd = (base + 2 * i + 1) * row_bytes;
            let dst_lo = (base + i) * row_bytes;
            let dst_hi = (base + half + i) * row_bytes;
            out[dst_lo..dst_lo + row_bytes].copy_from_slice(&data[src_even..src_even + row_bytes]);
            out[dst_hi..dst_hi + row_bytes].copy_from_slice(&data[src_odd..src_odd + row_bytes]);
        }
    }
    *data = out;
    Ok(())
}

/// Fetch a weight matrix in the most direct form a kernel can consume.
fn fetch_matrix(src: &dyn TensorSource, name: &str) -> Result<HostWeight> {
    if let Some(weight) = src.fetch_deepseek(name)? {
        return Ok(weight);
    }
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
            names: vec![name.to_string()],
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
    let output_scale = nvfp4_gguf_output_scale(src, name, quant)?;
    quant_host_weight(name, data, dtype, quant, rows, cols, output_scale)
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
    output_scale: f32,
) -> Result<HostWeight> {
    match quant {
        QuantKind::NVFP4Gguf if cols.is_multiple_of(64) => {
            if !output_scale.is_finite() || output_scale <= 0.0 {
                return Err(ForgeError::Format(format!(
                    "{name}: skala NVFP4 musi być skończona i dodatnia, otrzymano {output_scale}"
                )));
            }
            Ok(HostWeight::NvFp4Gguf {
                data,
                output_scale,
                rows,
                cols,
            })
        }
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
        QuantKind::IQ4XS if cols.is_multiple_of(256) => Ok(HostWeight::Iq4Xs { data, rows, cols }),
        QuantKind::MXFP4 => Ok(HostWeight::Mxfp4 { data, rows, cols }),
        QuantKind::IQ2XS if cols.is_multiple_of(256) => Ok(HostWeight::Iq2Xs { data, rows, cols }),
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
    quant_host_weight(name, data, dtype, quant, n_expert * a, b, 1.0)
}

/// Wczytuje wagi kompresora strumienia KV, gdy warstwa go ma.
fn load_compressor(
    device: &dyn Device,
    src: &dyn TensorSource,
    prefix: &str,
) -> Result<Option<CompressorWeights>> {
    if src
        .fetch_optional(&format!("{prefix}.wkv.weight"))?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(CompressorWeights {
        wkv: upload_weight(device, fetch_matrix(src, &format!("{prefix}.wkv.weight"))?)?,
        wgate: upload_weight(
            device,
            fetch_matrix(src, &format!("{prefix}.wgate.weight"))?,
        )?,
        ape: upload_f32(device, src, &format!("{prefix}.ape"))?,
        norm: upload_norm(device, src, &format!("{prefix}.norm.weight"))?,
    }))
}

/// Wczytuje jedną warstwę uwagi DeepSeeka V4.
///
/// Kompresor i indekser są opcjonalne i ich obecność wynika z `compress_ratios`:
/// warstwy o stopniu 0 nie mają żadnego, o stopniu 4 mają oba, a o stopniu 128
/// tylko kompresor. Opis modelu został wcześniej przycięty do tego, co jest w
/// checkpoincie, więc brakująca rola oznacza tu warstwę bez tego elementu, a nie
/// uszkodzony plik.
fn load_deepseek_attention(
    device: &dyn Device,
    src: &dyn TensorSource,
    layer: usize,
) -> Result<DeepseekAttnWeights> {
    let p = format!("layers.{layer}.attn");
    let indexer = if src
        .fetch_optional(&format!("{p}.indexer.weights_proj.weight"))?
        .is_some()
    {
        let compressor = load_compressor(device, src, &format!("{p}.indexer.compressor"))?
            .ok_or_else(|| {
                ForgeError::Format(format!("warstwa {layer}: indekser bez własnego kompresora"))
            })?;
        Some(IndexerWeights {
            wq_b: upload_weight(
                device,
                fetch_matrix(src, &format!("{p}.indexer.wq_b.weight"))?,
            )?,
            weights_proj: upload_weight(
                device,
                fetch_matrix(src, &format!("{p}.indexer.weights_proj.weight"))?,
            )?,
            compressor,
        })
    } else {
        None
    };
    Ok(DeepseekAttnWeights {
        wq_a: upload_weight(device, fetch_matrix(src, &format!("{p}.wq_a.weight"))?)?,
        q_norm: upload_norm(device, src, &format!("{p}.q_norm.weight"))?,
        wq_b: upload_weight(device, fetch_matrix(src, &format!("{p}.wq_b.weight"))?)?,
        wkv: upload_weight(device, fetch_matrix(src, &format!("{p}.wkv.weight"))?)?,
        kv_norm: upload_norm(device, src, &format!("{p}.kv_norm.weight"))?,
        wo_a: upload_weight(device, fetch_matrix(src, &format!("{p}.wo_a.weight"))?)?,
        wo_b: upload_weight(device, fetch_matrix(src, &format!("{p}.wo_b.weight"))?)?,
        attn_sink: upload_f32(device, src, &format!("{p}.attn_sink"))?,
        compressor: load_compressor(device, src, &format!("{p}.compressor"))?,
        indexer,
    })
}

/// Wczytuje komplet warstw DeepSeeka V4.
///
/// Wywoływane zamiast wspólnej pętli warstw, bo ta architektura ma inny mikser,
/// hyper-connections zamiast rezyduum i eksperty zapisane pojedynczo. Kompresor,
/// indekser, bias routera i tablica routingu są opcjonalne — opis modelu został
/// wcześniej przycięty do tego, co jest w checkpoincie, więc brak roli oznacza
/// warstwę bez tego elementu.
fn load_deepseek_layers(
    device: &dyn Device,
    descriptor: &ModelDescriptor,
    src: &dyn TensorSource,
    spill: Option<&ExpertSpill>,
    budget: Option<&ExpertBudget>,
) -> Result<Vec<LayerWeights>> {
    let moe = descriptor
        .params
        .moe
        .as_ref()
        .ok_or_else(|| ForgeError::Format("DeepSeek V4 jest modelem MoE".into()))?;
    let mut layers = Vec::with_capacity(descriptor.params.block_count);
    for (index, layer_map) in descriptor.layers.iter().enumerate() {
        let name = |role: WeightRole| -> Result<&String> {
            layer_map
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("warstwa {index}: brak roli {role:?}")))
        };
        let attn = load_deepseek_attention(device, src, index)?;
        let router = match fetch_matrix(src, name(WeightRole::FfnGateInp)?)? {
            weight @ HostWeight::F16 { .. } => upload_weight(device, weight)?,
            _ => {
                return Err(ForgeError::Unsupported(format!(
                    "warstwa {index}: router MoE musi dać się zmaterializować jako f16"
                )))
            }
        };
        let shared = DenseFfn {
            gate_up: GateUpWeights::Split {
                gate: upload_weight(device, fetch_matrix(src, name(WeightRole::FfnGateShExp)?)?)?,
                up: upload_weight(device, fetch_matrix(src, name(WeightRole::FfnUpShExp)?)?)?,
            },
            down: upload_weight(device, fetch_matrix(src, name(WeightRole::FfnDownShExp)?)?)?,
        };
        let ffn = LayerFfn::Moe(Box::new(MoeFfn {
            router,
            gate_exps: upload_per_expert_stack(
                device,
                src,
                name(WeightRole::FfnGateExps)?,
                moe.n_experts,
                spill,
                budget,
            )?,
            up_exps: upload_per_expert_stack(
                device,
                src,
                name(WeightRole::FfnUpExps)?,
                moe.n_experts,
                spill,
                budget,
            )?,
            down_exps: upload_per_expert_stack(
                device,
                src,
                name(WeightRole::FfnDownExps)?,
                moe.n_experts,
                spill,
                budget,
            )?,
            shared: Some(shared),
            shared_gate: None,
            n_experts: moe.n_experts,
            n_experts_used: moe.n_experts_used,
            moe_inter: moe.moe_intermediate_size,
            norm_topk: moe.norm_topk_prob,
            gate_bias: match layer_map.get(&WeightRole::FfnGateBias) {
                Some(bias) => Some(upload_f32(device, src, bias)?),
                None => None,
            },
            tid2eid: match layer_map.get(&WeightRole::FfnGateTid2Eid) {
                Some(table) => Some(upload_raw(device, src, table)?),
                None => None,
            },
            usage: ExpertUsage::new(device, moe.n_experts)?,
        }));
        layers.push(LayerWeights {
            attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
            ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
            post_attn_norm: None,
            post_ffw_norm: None,
            layer_output_scale: None,
            mixer: LayerMixer::DeepseekAttention(Box::new(attn)),
            ffn,
            hc_attn: Some(HyperConnectionWeights {
                mix_fn: upload_f32(device, src, name(WeightRole::HcAttnFn)?)?,
                base: upload_f32(device, src, name(WeightRole::HcAttnBase)?)?,
                scale: upload_f32(device, src, name(WeightRole::HcAttnScale)?)?,
            }),
            hc_ffn: Some(HyperConnectionWeights {
                mix_fn: upload_f32(device, src, name(WeightRole::HcFfnFn)?)?,
                base: upload_f32(device, src, name(WeightRole::HcFfnBase)?)?,
                scale: upload_f32(device, src, name(WeightRole::HcFfnScale)?)?,
            }),
        });
    }
    Ok(layers)
}

/// Wgrywa tensor bajt w bajt, bez interpretacji — tablica routingu haszowanego
/// jest indeksowana na hoście, więc jej format jest wewnętrzną sprawą loadera.
fn upload_raw(device: &dyn Device, src: &dyn TensorSource, name: &str) -> Result<DevBuffer> {
    let (data, _, _, _) = src.fetch(name)?;
    upload(device, &data)
}

/// Wgrywa eksperty warstwy MoE zapisane POJEDYNCZO (`ffn.experts.{e}.w1`), a nie
/// jako jeden sklejony tensor.
///
/// Ten układ pasuje do rezydencji per ekspert bez żadnej pracy dodatkowej: każdy
/// blok jest już osobną alokacją, więc może leżeć w VRAM, w pamięci hosta albo
/// na dysku niezależnie od sąsiadów.
fn upload_per_expert_stack(
    device: &dyn Device,
    src: &dyn TensorSource,
    template: &str,
    n_experts: usize,
    spill: Option<&ExpertSpill>,
    budget: Option<&ExpertBudget>,
) -> Result<ExpertStack> {
    let (vram_slots, host_slots) = match budget {
        Some(budget) => budget.plan(n_experts),
        None => (n_experts, 0),
    };
    let mut slots = Vec::with_capacity(vram_slots + host_slots);
    let mut resident = Vec::with_capacity(vram_slots + host_slots);
    let mut spilled = vec![None; n_experts];
    let mut rows_per_expert = 0usize;
    let mut cols = 0usize;
    for expert in 0..n_experts {
        let name = template.replace("{expert}", &expert.to_string());
        let weight = fetch_matrix(src, &name)?;
        rows_per_expert = weight.rows();
        cols = weight.cols();
        let HostWeight::NvFp4Gguf {
            data,
            output_scale,
            rows,
            cols: weight_cols,
        } = weight
        else {
            return Err(ForgeError::Unsupported(format!(
                "{name}: eksperci DeepSeeka muszą być NVFP4"
            )));
        };
        let kind = if expert < vram_slots {
            MemKind::Device
        } else if expert < vram_slots + host_slots {
            MemKind::PinnedHost
        } else {
            match spill {
                Some(spill) => {
                    spilled[expert] = Some(spill.append(&data)?);
                    continue;
                }
                None => MemKind::Device,
            }
        };
        match upload_as(device, &data, kind) {
            Ok(buf) => {
                slots.push(DevWeight::NvFp4Gguf {
                    buf,
                    output_scale,
                    rows,
                    cols: weight_cols,
                    layout: Nvfp4GgufLayout::RowMajor36,
                });
                resident.push(expert);
            }
            Err(out_of_memory) => {
                let Some(spill) = spill else {
                    return Err(out_of_memory);
                };
                spilled[expert] = Some(spill.append(&data)?);
            }
        }
    }
    if slots.is_empty() {
        return Err(ForgeError::OutOfMemory {
            requested: rows_per_expert * cols,
            available: 0,
        });
    }
    ExpertStack::new(device, slots, resident, spilled, rows_per_expert, cols)
}

/// Wczytuje model DeepSeeka V4 obcięty do `layers` pierwszych warstw.
///
/// Istnieje dla testów: pełny model ma 157 GB i 43 warstwy po 256 ekspertów, a
/// poprawność ZŁOŻENIA przebiegu — embedding, bloki, głowa — trzeba sprawdzić
/// na prawdziwych wagach, nie na syntetyku. Obcięty model jest poprawnym
/// modelem o mniejszej liczbie warstw, więc ścieżka wykonania jest ta sama.
pub fn load_deepseek_prefix_for_test(
    device: Arc<dyn Device>,
    dir: &Path,
    layers: usize,
    host_budget: usize,
    spill: Option<&ExpertSpill>,
) -> Result<ModelWeights> {
    let config: HfConfig = HfConfig::load(dir.join("config.json"))?;
    let st = ShardedSafeTensors::load_dir(dir)?;
    let mut descriptor = ModelDescriptor::from_hf(&config)?;
    descriptor.prune_absent_optional(|name| st.tensor(name).is_some());
    descriptor.layers.truncate(layers);
    descriptor.layer_kinds.truncate(layers);
    descriptor.params.block_count = layers;
    let src = StSource {
        st: &st,
        scheme: NvFp4Scheme::detect(&config),
        fp8: false,
        deepseek_v4: true,
    };
    // Bez tej nakładki wagi, które nie zmieszczą się w VRAM, nie mają dokąd
    // pójść — a budżet rezydencji liczy pojemność jako VRAM PLUS pamięć hosta.
    let sink: Arc<dyn Device> = Arc::new(crate::weight_tier::TieredWeightDevice::new(
        device,
        host_budget,
    ));
    ModelWeights::load_deepseek_v4(sink.as_ref(), descriptor, &src, spill, host_budget)
}

/// Wczytuje samą warstwę uwagi DeepSeeka V4 z katalogu safetensors.
///
/// Istnieje dla testów: pełny model ma 157 GB, a geometrię i przejście przez
/// konwersje kwantyzacji trzeba sprawdzić na PRAWDZIWYCH wagach, nie na
/// syntetyku. Ładuje jedną warstwę, czyli około 200 MB.
pub fn load_deepseek_layer_for_test(
    device: &dyn Device,
    dir: &Path,
    layer: usize,
) -> Result<DeepseekAttnWeights> {
    let config: HfConfig = HfConfig::load(dir.join("config.json"))?;
    let st = ShardedSafeTensors::load_dir(dir)?;
    let src = StSource {
        st: &st,
        scheme: NvFp4Scheme::detect(&config),
        fp8: false,
        deepseek_v4: true,
    };
    load_deepseek_attention(device, &src, layer)
}

/// Ile bajtów ekspertów wolno zostawić w pamięci przy tym modelu.
///
/// Pojemność to wolne miejsce, jakie zgłasza warstwowe urządzenie (VRAM plus
/// budżet przypiętej pamięci hosta), minus wszystko, czego zrzucić na dysk się
/// nie da. `None`, gdy źródło nie potrafi podać rozmiarów albo urządzenie
/// pojemności — wtedy zostaje dotychczasowe zachowanie sterowane brakiem
/// pamięci.
fn expert_residency_budget(
    device: &dyn Device,
    descriptor: &ModelDescriptor,
    src: &dyn TensorSource,
    host_budget: usize,
) -> Option<ExpertBudget> {
    // Liczone PO wgraniu wag globalnych: embedding i głowa logitów bywają przy
    // ładowaniu przewalutowane (kwantyzacja z dysku -> f16), więc ich rozmiar w
    // pliku nie mówi nic o zajętej pamięci. Wolne miejsce zgłoszone teraz już
    // ten koszt uwzględnia, a pozostałe wagi warstw idą do pamięci takie, jakie
    // są na dysku — dla nich rachunek z pliku jest dokładny.
    let capacity = device.pool_available(Pool::Weights)?;
    let n_experts = descriptor.params.moe.as_ref().map(|moe| moe.n_experts);
    let mut expert_bytes = 0usize;
    let mut layer_bytes = 0usize;
    for layer in &descriptor.layers {
        for (role, name) in layer {
            match role {
                WeightRole::FfnGateExps | WeightRole::FfnUpExps | WeightRole::FfnDownExps => {
                    // Rola per ekspert jest SZABLONEM — rozmiar bierzemy z
                    // eksperta zerowego i mnożymy, bo wszyscy są równi.
                    let n = n_experts?;
                    let first = name.replace("{expert}", "0");
                    expert_bytes += src.byte_len(&first)? * n;
                }
                _ => layer_bytes += src.byte_len(name)?,
            }
        }
    }
    if expert_bytes == 0 {
        return None;
    }
    // Zapas na wyrównanie areny i drobne bufory rezydencji (tablice wskaźników,
    // liczniki popularności), których nie ma w pliku modelu.
    const SLACK: usize = 64 << 20;
    // Wagi warstw inne niż eksperci nie mają dokąd się wynieść, więc miejsce na
    // nie jest odejmowane od VRAM zanim eksperci cokolwiek zajmą.
    let vram_capacity = capacity.saturating_sub(host_budget);
    let vram_for_experts = vram_capacity.saturating_sub(layer_bytes + SLACK);
    // Każdy stos musi pomieścić naraz komplet wybrany przez router, z zapasem
    // na to, że kolejny token wybierze inny — inaczej eksperci wypieraliby się
    // nawzajem w obrębie jednej warstwy.
    let min_slots = descriptor
        .params
        .moe
        .as_ref()
        .map(|moe| moe.n_experts_used * 2)
        .unwrap_or(1);
    let budget = ExpertBudget::new(vram_for_experts, host_budget, expert_bytes, min_slots);
    tracing::info!(
        vram_for_experts_mib = vram_for_experts >> 20,
        host_mib = host_budget >> 20,
        expert_mib = expert_bytes >> 20,
        layer_other_mib = layer_bytes >> 20,
        resident_pct = format!("{:.1}", budget.resident_fraction() * 100.0),
        "budżet rezydencji ekspertów"
    );
    Some(budget)
}

/// Wgrywa sklejony stos ekspertów jako osobne bloki — po jednym na eksperta.
///
/// Bloki są alokowane po kolei, więc urządzenie warstwowe rozkłada je samo:
/// najpierw VRAM, potem przypięta pamięć hosta. Gdy skończy się jedno i drugie,
/// reszta idzie do pliku zrzutu i będzie stronicowana na żądanie. Bez pliku
/// zrzutu brak pamięci pozostaje twardym błędem — po cichu obciąć modelu nie
/// wolno.
fn upload_expert_stack(
    device: &dyn Device,
    stack: HostWeight,
    n_experts: usize,
    spill: Option<&ExpertSpill>,
    budget: Option<&ExpertBudget>,
) -> Result<ExpertStack> {
    type Rewrap = fn(DevBuffer, usize, usize) -> DevWeight;
    macro_rules! stack_parts {
        ($stack:expr, $($variant:ident),+ $(,)?) => {
            match $stack {
                $(HostWeight::$variant { data, rows, cols } => (
                    data,
                    rows,
                    cols,
                    (|buf, rows, cols| DevWeight::$variant { buf, rows, cols }) as Rewrap,
                ),)+
                other => {
                    return Err(ForgeError::Unsupported(format!(
                        "routed MoE experts in {:?} are not splittable per expert",
                        std::mem::discriminant(&other)
                    )))
                }
            }
        };
    }
    let (data, rows, cols, rewrap) = stack_parts!(
        stack, F16, Q8_0, Q4K, Q6K, Q5K, Q3K, Q2K, Q4_0, Q4_1, Q5_0, Q5_1, Iq4Nl, Iq4Xs, Mxfp4,
        Iq2Xs, Iq2S, Iq3S, Iq2Xxs, Iq3Xxs, Iq1S, Iq1M,
    );
    let (rows_per_expert, bytes_per_expert) = expert_slice_shape(rows, data.len(), n_experts)?;

    // Plan zapada PRZED alokacją i jest ten sam dla każdego stosu, więc żadna
    // warstwa nie zostaje bez rezydentnych ekspertów tylko dlatego, że ładuje
    // się jako ostatnia.
    let (vram_slots, host_slots) = match budget {
        Some(budget) => budget.plan(n_experts),
        None => (n_experts, 0),
    };
    let mut slots = Vec::with_capacity(vram_slots + host_slots);
    let mut resident = Vec::with_capacity(vram_slots + host_slots);
    let mut spilled = vec![None; n_experts];
    for expert in 0..n_experts {
        let bytes = &data[expert * bytes_per_expert..(expert + 1) * bytes_per_expert];
        let kind = if expert < vram_slots {
            MemKind::Device
        } else if expert < vram_slots + host_slots {
            MemKind::PinnedHost
        } else {
            match spill {
                Some(spill) => {
                    spilled[expert] = Some(spill.append(bytes)?);
                    continue;
                }
                // Bez pliku zrzutu nie ma trzeciej warstwy — próbujemy zwykłej
                // ścieżki i pozwalamy brakowi pamięci być błędem.
                None => MemKind::Device,
            }
        };
        match upload_as(device, bytes, kind) {
            Ok(buf) => {
                slots.push(rewrap(buf, rows_per_expert, cols));
                resident.push(expert);
            }
            Err(out_of_memory) => {
                let Some(spill) = spill else {
                    return Err(out_of_memory);
                };
                spilled[expert] = Some(spill.append(bytes)?);
            }
        }
    }
    let on_disk = spilled.iter().filter(|r| r.is_some()).count();
    if on_disk > 0 {
        tracing::debug!(
            slots = slots.len(),
            on_disk,
            "stos ekspertów częściowo zrzucony na dysk"
        );
    }
    if slots.is_empty() {
        return Err(ForgeError::OutOfMemory {
            requested: bytes_per_expert,
            available: 0,
        });
    }
    ExpertStack::new(device, slots, resident, spilled, rows_per_expert, cols)
}

/// Wiersze i bajty przypadające na jednego eksperta; obie wielkości muszą
/// dzielić się bez reszty, inaczej stos nie jest jednorodny.
fn expert_slice_shape(rows: usize, bytes: usize, n_experts: usize) -> Result<(usize, usize)> {
    if n_experts == 0 || !rows.is_multiple_of(n_experts) || !bytes.is_multiple_of(n_experts) {
        return Err(ForgeError::Format(format!(
            "expert stack of {rows} rows / {bytes} B does not split into {n_experts} experts"
        )));
    }
    Ok((rows / n_experts, bytes / n_experts))
}

/// Upload a host matrix as-is.
fn upload_weight(device: &dyn Device, w: HostWeight) -> Result<DevWeight> {
    match w {
        HostWeight::Fp8Row {
            data,
            scales,
            rows,
            cols,
        } => Ok(DevWeight::Fp8Row {
            buf: upload(device, &data)?,
            scales: upload(device, bytemuck::cast_slice(&scales))?,
            rows,
            cols,
        }),
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
            names: _,
            packed,
            scales,
            global_scale,
            rows,
            cols,
        } => Ok(DevWeight::NvFp4 {
            storage: NvFp4CtStorage::RowMajorE4M3 {
                packed: upload(device, &packed)?,
                scales: upload(device, &scales)?,
            },
            inv_global_scale: 1.0 / global_scale,
            rows,
            cols,
        }),
        HostWeight::NvFp4Gguf {
            data,
            output_scale,
            rows,
            cols,
        } => Ok(DevWeight::NvFp4Gguf {
            buf: upload(device, &data)?,
            output_scale,
            rows,
            cols,
            layout: Nvfp4GgufLayout::RowMajor36,
        }),
    }
}

fn upload_weight_with_nvfp4_ct(
    device: &dyn Device,
    weight: HostWeight,
    context: Option<&NvFp4CtUploadContext<'_>>,
) -> Result<DevWeight> {
    let Some(context) = context else {
        return upload_weight(device, weight);
    };
    let HostWeight::NvFp4 {
        names,
        packed,
        scales,
        global_scale,
        rows,
        cols,
    } = weight
    else {
        return upload_weight(device, weight);
    };
    if context.plan == NvFp4CtLoadPlan::RowMajorE4M3 {
        return upload_weight(
            device,
            HostWeight::NvFp4 {
                names,
                packed,
                scales,
                global_scale,
                rows,
                cols,
            },
        );
    }
    if !rows.is_multiple_of(64) || !cols.is_multiple_of(128) {
        return Err(ForgeError::Unsupported(format!(
            "S0 N64/K128 nie obsługuje kształtu [{rows}, {cols}]"
        )));
    }
    let upload_identity =
        nvfp4_ct_upload_identity(names.clone(), rows, cols, global_scale.to_bits());
    if !context.expected_identities.contains(&upload_identity)
        || context
            .completed_identities
            .borrow()
            .contains(&upload_identity)
    {
        return Err(ForgeError::Format(format!(
            "NVFP4 CT: upload spoza manifestu: {names:?}"
        )));
    }
    let target_bytes = nvfp4_ct_s0_resident_bytes(rows, cols)?;
    let target = device.alloc(target_bytes, MemKind::Device, Pool::Weights)?;
    let packed_scratch = context
        .packed_scratch
        .as_ref()
        .ok_or_else(|| ForgeError::Kernel("NVFP4 CT: brak stagingu packed".into()))?;
    let scale_scratch = context
        .scale_scratch
        .as_ref()
        .ok_or_else(|| ForgeError::Kernel("NVFP4 CT: brak stagingu scales".into()))?;
    let mut row_offset = 0usize;
    while row_offset < rows {
        let chunk_rows = context.chunk_rows.min(rows - row_offset);
        let packed_row_bytes = cols / 2;
        let scale_row_bytes = cols / 16;
        let packed_start = row_offset * packed_row_bytes;
        let scale_start = row_offset * scale_row_bytes;
        let packed_bytes = chunk_rows * packed_row_bytes;
        let scale_bytes = chunk_rows * scale_row_bytes;
        device.write(
            &packed[packed_start..packed_start + packed_bytes],
            packed_scratch,
            0,
        )?;
        device.write(
            &scales[scale_start..scale_start + scale_bytes],
            scale_scratch,
            0,
        )?;
        context.kernels.repack_nvfp4_ct_s0_n64k128_into(
            &target,
            packed_scratch,
            scale_scratch,
            rows,
            cols,
            chunk_rows,
            row_offset,
            context.stream,
        )?;
        context.stream.synchronize()?;
        row_offset += chunk_rows;
    }
    context
        .completed_identities
        .borrow_mut()
        .insert(upload_identity);
    Ok(DevWeight::NvFp4 {
        storage: NvFp4CtStorage::S0N64K128 { data: target },
        inv_global_scale: 1.0 / global_scale,
        rows,
        cols,
    })
}

fn upload_target_weight(
    device: &dyn Device,
    weight: HostWeight,
    target_tile: Option<(&Kernels, &forge_hal::Stream, &Cell<usize>)>,
    nvfp4_ct: Option<&NvFp4CtUploadContext<'_>>,
) -> Result<DevWeight> {
    if matches!(weight, HostWeight::NvFp4 { .. }) {
        return upload_weight_with_nvfp4_ct(device, weight, nvfp4_ct);
    }
    let Some((kernels, stream, repacked_weights)) = target_tile else {
        return upload_weight(device, weight);
    };
    let HostWeight::NvFp4Gguf {
        data,
        output_scale,
        rows,
        cols,
    } = weight
    else {
        return upload_weight(device, weight);
    };
    if !rows.is_multiple_of(128) || !cols.is_multiple_of(64) {
        return upload_weight(
            device,
            HostWeight::NvFp4Gguf {
                data,
                output_scale,
                rows,
                cols,
            },
        );
    }

    let source = device.alloc(data.len(), MemKind::Device, Pool::Activations)?;
    let result = (|| {
        device.write(&data, &source, 0)?;
        let target = device.alloc(data.len(), MemKind::Device, Pool::Weights)?;
        kernels.repack_nvfp4_gguf_tile_n128_k64(&target, &source, rows, cols, stream)?;
        device.synchronize()?;
        Ok(target)
    })();
    drop(source);
    let reset = device.reset_activations();
    let target = match result {
        Ok(target) => {
            reset?;
            target
        }
        Err(error) => {
            let _ = reset;
            return Err(error);
        }
    };
    repacked_weights.set(repacked_weights.get().saturating_add(1));
    Ok(DevWeight::NvFp4Gguf {
        buf: target,
        output_scale,
        rows,
        cols,
        layout: Nvfp4GgufLayout::TileN128K64,
    })
}

fn preflight_target_tile(device: &dyn Device, descriptor: &ModelDescriptor) -> Result<()> {
    let p = &descriptor.params;
    let q_dim = p
        .n_heads
        .checked_mul(p.head_dim)
        .ok_or_else(|| ForgeError::Format("wymiar Q przekracza usize".into()))?;
    let kv_dim = p
        .n_kv_heads
        .checked_mul(p.head_dim)
        .ok_or_else(|| ForgeError::Format("wymiar KV przekracza usize".into()))?;
    let mut shapes = vec![
        (
            q_dim
                .checked_add(
                    kv_dim
                        .checked_mul(2)
                        .ok_or_else(|| ForgeError::Format("wymiar QKV przekracza usize".into()))?,
                )
                .ok_or_else(|| ForgeError::Format("wymiar QKV przekracza usize".into()))?,
            p.hidden_size,
        ),
        (
            p.intermediate_size
                .checked_mul(2)
                .ok_or_else(|| ForgeError::Format("wymiar gate/up przekracza usize".into()))?,
            p.hidden_size,
        ),
        (p.hidden_size, p.intermediate_size),
        (p.hidden_size, q_dim),
    ];
    if let Some(ssm) = &p.ssm {
        shapes.extend([
            (ssm.conv_dim(), p.hidden_size),
            (ssm.value_dim(), p.hidden_size),
            (ssm.n_v_heads(), p.hidden_size),
            (p.hidden_size, ssm.value_dim()),
        ]);
    }
    let max_bytes = shapes
        .into_iter()
        .filter(|(rows, cols)| rows.is_multiple_of(128) && cols.is_multiple_of(64))
        .try_fold(0usize, |maximum, (rows, cols)| {
            rows.checked_mul(cols / 64)
                .and_then(|blocks| blocks.checked_mul(36))
                .map(|bytes| maximum.max(bytes))
                .ok_or_else(|| ForgeError::Format("rozmiar stagingu NVFP4 przekracza usize".into()))
        })?;
    if let Some(available) = device.pool_available(Pool::Activations) {
        if max_bytes > available {
            return Err(ForgeError::OutOfMemory {
                requested: max_bytes,
                available,
            });
        }
    }
    Ok(())
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
/// output row (`scale[r] = absmax(row r) / 448`). Rows whose weights are all
/// zero get scale 0 (their e4m3 codes are zero too).
fn pack_fp8_host(w: &[f32], rows: usize, cols: usize) -> (Vec<u8>, Vec<u8>) {
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
    (codes, scale_bytes)
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
        HostWeight::Q4_0 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q4_0 { .. })) => {}
        HostWeight::Q4_1 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q4_1 { .. })) => {}
        HostWeight::Q5_0 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q5_0 { .. })) => {}
        HostWeight::Q5_1 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Q5_1 { .. })) => {}
        HostWeight::Iq4Nl { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq4Nl { .. })) => {
        }
        HostWeight::Iq4Xs { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq4Xs { .. })) => {
        }
        HostWeight::Mxfp4 { .. } if parts.iter().all(|p| matches!(p, HostWeight::Mxfp4 { .. })) => {
        }
        HostWeight::Iq2Xs { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq2Xs { .. })) => {
        }
        HostWeight::Iq2S { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq2S { .. })) => {}
        HostWeight::Iq3S { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq3S { .. })) => {}
        HostWeight::Iq2Xxs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq2Xxs { .. })) => {}
        HostWeight::Iq3Xxs { .. }
            if parts.iter().all(|p| matches!(p, HostWeight::Iq3Xxs { .. })) => {}
        HostWeight::Iq1S { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq1S { .. })) => {}
        HostWeight::Iq1M { .. } if parts.iter().all(|p| matches!(p, HostWeight::Iq1M { .. })) => {}
        HostWeight::NvFp4 { global_scale, .. } => {
            let gs = global_scale.to_bits();
            let ok = parts.iter().all(
                |p| matches!(p, HostWeight::NvFp4 { global_scale, .. } if global_scale.to_bits() == gs),
            );
            if !ok {
                return Err(parts);
            }
        }
        HostWeight::NvFp4Gguf { output_scale, .. } => {
            let scale = output_scale.to_bits();
            if !parts.iter().all(
                |p| matches!(p, HostWeight::NvFp4Gguf { output_scale, .. } if output_scale.to_bits() == scale),
            ) {
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
            | (HostWeight::Iq1M { data, .. }, HostWeight::Iq1M { data: d, .. })
            | (HostWeight::NvFp4Gguf { data, .. }, HostWeight::NvFp4Gguf { data: d, .. }) => {
                data.extend_from_slice(&d)
            }
            (
                HostWeight::NvFp4 {
                    names,
                    packed,
                    scales,
                    ..
                },
                HostWeight::NvFp4 {
                    names: names2,
                    packed: p2,
                    scales: s2,
                    ..
                },
            ) => {
                names.extend(names2);
                packed.extend_from_slice(&p2);
                scales.extend_from_slice(&s2);
            }
            _ => unreachable!("format equality checked above"),
        }
    }
    match &mut fused {
        HostWeight::Fp8Row { rows: r, .. }
        | HostWeight::F16 { rows: r, .. }
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
        | HostWeight::NvFp4 { rows: r, .. }
        | HostWeight::NvFp4Gguf { rows: r, .. } => *r = rows,
    }
    Ok(fused)
}

/// Fetch the embedding table as a host-resident f16 vector (row-major
/// [vocab*hidden]); returns `(table, vocab, hidden)`.
fn fetch_embedding_host(
    src: &dyn TensorSource,
    name: &str,
) -> Result<(Vec<f16>, HostWeight, usize, usize)> {
    let (data, dtype, quant, dims) = src.fetch(name)?;
    if dims.len() != 2 {
        return Err(ForgeError::Format(format!(
            "{name}: expected matrix, got {dims:?}"
        )));
    }
    let (rows, cols) = (dims[0], dims[1]);
    let output_scale = nvfp4_gguf_output_scale(src, name, quant)?;
    let mut f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    if quant == QuantKind::NVFP4Gguf {
        for value in &mut f32s {
            *value *= output_scale;
        }
    }
    let f16s = f32s.iter().map(|&v| f16::from_f32(v)).collect();
    let weight = quant_host_weight(name, data, dtype, quant, rows, cols, output_scale)?;
    Ok((f16s, weight, rows, cols))
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
    let output_scale = nvfp4_gguf_output_scale(src, name, quant)?;
    let mut f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
    if quant == QuantKind::NVFP4Gguf {
        for value in &mut f32s {
            *value *= output_scale;
        }
    }
    Ok((upload(device, &f32s_to_f16_bytes(&f32s))?, rows, cols))
}

impl ModelWeights {
    pub fn load_gguf(
        device: &Arc<dyn Device>,
        path: &Path,
        native_mtp: bool,
        target_tile: Option<(&Kernels, &forge_hal::Stream, &Cell<usize>)>,
        spill: Option<&ExpertSpill>,
        host_budget: usize,
        layer_range: Option<(usize, usize)>,
        shard: TpShard,
    ) -> Result<Self> {
        let gguf = Gguf::open(path)?;
        let mut descriptor = ModelDescriptor::detect(&gguf)?;
        if let Some((first, count)) = layer_range {
            descriptor.restrict_layers(first, count)?;
        }
        let src = GgufSource(&gguf);
        Self::load(
            device.as_ref(),
            descriptor,
            &src,
            native_mtp,
            target_tile,
            None,
            spill,
            host_budget,
            shard,
        )
    }

    pub fn load_safetensors_dir(
        device: &Arc<dyn Device>,
        dir: &Path,
        native_mtp: bool,
        target_tile: Option<(&Kernels, &forge_hal::Stream, &Cell<usize>)>,
        nvfp4_ct: (&Kernels, &forge_hal::Stream, NvFp4CtLayoutPolicy),
        spill: Option<&ExpertSpill>,
        host_budget: usize,
    ) -> Result<Self> {
        let config_text = std::fs::read_to_string(dir.join("config.json"))?;
        let config: HfConfig = serde_json::from_str::<HfConfig>(&config_text)
            .map_err(|e| ForgeError::Format(format!("config.json: {e}")))?;
        let mut descriptor = ModelDescriptor::from_hf(&config)?;
        let st = ShardedSafeTensors::load_dir(dir)?;
        // Opis z `config.json` deklaruje role opcjonalne dla wszystkich warstw;
        // dopiero tabela tensorów mówi, które z nich naprawdę istnieją.
        descriptor.prune_absent_optional(|name| st.tensor(name).is_some());
        let scheme = NvFp4Scheme::detect(&config);
        let fp8 = config
            .quantization_config
            .as_ref()
            .and_then(|qc| qc.get("format"))
            .and_then(|f| f.as_str())
            == Some("float-quantized");
        // Checkpoint MLX rozpoznaje się po bloku kwantyzacji w config.json i
        // dalej idzie tą samą ścieżką co każdy inny (`crate::mlx_source`).
        let mlx_src = crate::mlx_source::MlxSource::detect(&config_text, &st);
        let st_src = StSource {
            st: &st,
            scheme: scheme.clone(),
            fp8,
            deepseek_v4: descriptor.arch == "deepseek_v4",
        };
        let src: &dyn TensorSource = mlx_src.as_ref().map_or(&st_src, |s| s);
        let preflight = plan_nvfp4_ct_safetensors(
            &st,
            scheme.as_ref(),
            &descriptor,
            native_mtp,
            nvfp4_ct.2,
            nvfp4_ct.0.supports_nvfp4_ct_s0_n64k128_manual(),
        )?;
        let context =
            create_nvfp4_ct_upload_context(device.as_ref(), nvfp4_ct.0, nvfp4_ct.1, preflight)?;
        let used_staging = context.plan == NvFp4CtLoadPlan::S0N64K128;
        let result = Self::load(
            device.as_ref(),
            descriptor,
            src,
            native_mtp,
            target_tile,
            Some(&context),
            spill,
            host_budget,
            TpShard::new(0, 1)?,
        )
        .and_then(|weights| {
            validate_nvfp4_ct_upload_manifest(
                context.plan,
                &context.expected_identities,
                &context.completed_identities.borrow(),
            )?;
            Ok(weights)
        });
        if result.is_ok() && used_staging {
            tracing::info!(
                expected_uploads = context.expected_identities.len(),
                completed_uploads = context.completed_identities.borrow().len(),
                "manifest uploadu NVFP4 CT S0 ukończony"
            );
        }
        drop(context);
        let reset = if used_staging {
            device.reset_activations().map(|_| ())
        } else {
            Ok(())
        };
        if used_staging && reset.is_ok() {
            tracing::info!("staging NVFP4 CT S0 zwolniony z puli aktywacji");
        }
        finish_nvfp4_ct_load(result, reset)
    }

    fn load(
        device: &dyn Device,
        mut descriptor: ModelDescriptor,
        src: &dyn TensorSource,
        native_mtp: bool,
        target_tile: Option<(&Kernels, &forge_hal::Stream, &Cell<usize>)>,
        nvfp4_ct: Option<&NvFp4CtUploadContext<'_>>,
        spill: Option<&ExpertSpill>,
        host_budget: usize,
        shard: TpShard,
    ) -> Result<Self> {
        if target_tile.is_some() {
            preflight_target_tile(device, &descriptor)?;
        }
        // Hybrid attention/DeltaNet arches (qwen35moe) have per-layer weight
        // sets that differ by kind and a gated attention Q projection the
        // generic shape checks would reject; they take a dedicated loader.
        if shard.world > 1 && descriptor.arch == "deepseek_v4" {
            return Err(ForgeError::Unsupported(
                "podział tensor-parallel nie obejmuje uwagi latentnej DeepSeeka V4".into(),
            ));
        }
        // Naprzemienna geometria uwagi (Gemma 4) ma inne `q_dim`/`kv_dim` w
        // różnych warstwach, a plan podziału liczy się z JEDNEJ liczby głowic.
        // Podział po takim planie dałby randze cudze głowice w połowie warstw.
        if shard.world > 1 && descriptor.params.alt_attn.is_some() {
            return Err(ForgeError::Unsupported(
                "podział tensor-parallel nie obejmuje naprzemiennej geometrii uwagi".into(),
            ));
        }
        if descriptor.arch == "deepseek_v4" {
            return Self::load_deepseek_v4(device, descriptor, src, spill, host_budget);
        }
        if descriptor.params.ssm.is_some() {
            if w4a8_enabled() {
                return Err(ForgeError::Unsupported(
                    "FORGE_GEMM=w4a8 supports dense (non-hybrid) models only".into(),
                ));
            }
            return Self::load_hybrid(
                device,
                descriptor,
                src,
                native_mtp,
                target_tile,
                nvfp4_ct,
                spill,
                host_budget,
                shard,
            );
        }
        let global = |role: WeightRole| -> Result<&String> {
            descriptor
                .globals
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("missing global weight {role:?}")))
        };

        // Plan cięcia opisuje wycinek PEŁNEJ macierzy, więc liczy się z pełnych
        // hiperparametrów; deskryptor, który zostaje w modelu, ma już kształty
        // rangi, żeby cała reszta silnika widziała po prostu mniejszy model.
        let full_params = descriptor.params.clone();
        let plan = |role: WeightRole| -> Result<RoleShard> { shard.role_shard(&full_params, role) };
        let embd_name = global(WeightRole::TokenEmbd)?;
        let (token_embd_f16, vocab, hidden) = upload_embedding(device, src, embd_name)?;
        let output_norm = upload_norm(device, src, global(WeightRole::OutputNorm)?)?;
        let rope_freqs = match descriptor.globals.get(&WeightRole::RopeFreqs) {
            Some(n) => Some(upload_f32(device, src, n)?),
            None => None,
        };
        let neg_inf = if descriptor.params.suppress_tokens.is_empty() {
            None
        } else {
            Some(upload(device, &f32::NEG_INFINITY.to_le_bytes())?)
        };

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
                ..
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
        // Geometria bywa różna per warstwa (Gemma 4), więc wymiary liczymy dla
        // konkretnej warstwy, a nie raz dla całego modelu.
        let q_dim_at = |layer: usize| p.n_heads * p.head_dim_at(layer);
        let kv_dim_at = |layer: usize| p.n_kv_heads_at(layer) * p.head_dim_at(layer);
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

        let budget = expert_residency_budget(device, &descriptor, src, host_budget);
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

            // Wymiary tej konkretnej warstwy — przy naprzemiennej geometrii
            // różnią się między warstwami z oknem a globalnymi.
            let q_dim = q_dim_at(idx);
            let kv_dim = kv_dim_at(idx);
            let mut q = fetch_matrix(src, name(WeightRole::AttnQ)?)?;
            let mut k = fetch_matrix(src, name(WeightRole::AttnK)?)?;
            expect(&at("attn_q"), &q, q_dim, p.hidden_size)?;
            expect(&at("attn_k"), &k, kv_dim, p.hidden_size)?;
            // Rodzina Llama wymaga rotacji przeplatanej, a wszystkie kernele
            // RoPE w silniku liczą wariant NeoX. Przestawienie wierszy Q i K raz
            // przy ładowaniu sprowadza jedno do drugiego bez kosztu w czasie
            // wykonania — patrz `permute_rope_pairs`.
            if descriptor.rope_interleaved() && src.stores_original_rope_order() {
                let head_dim = p.head_dim_at(idx);
                permute_rope_pairs(&mut q, head_dim)?;
                permute_rope_pairs(&mut k, head_dim)?;
            }
            let (q, k) = (
                shard_host_weight(q, &plan(WeightRole::AttnQ)?)?,
                shard_host_weight(k, &plan(WeightRole::AttnK)?)?,
            );
            // Brak projekcji V oznacza wariant, w którym V = K (warstwy
            // globalne Gemmy 4); wtedy fuzja bierze K drugi raz.
            let v = if p.has_v_proj(idx) {
                let v = fetch_matrix(src, name(WeightRole::AttnV)?)?;
                expect(&at("attn_v"), &v, kv_dim, p.hidden_size)?;
                shard_host_weight(v, &plan(WeightRole::AttnV)?)?
            } else {
                shard_host_weight(
                    fetch_matrix(src, name(WeightRole::AttnK)?)?,
                    &plan(WeightRole::AttnK)?,
                )?
            };
            // Modele z naprzemienną geometrią uwagi (Gemma 4) nie scalają q|k|v:
            // dekodowanie musi liczyć normy i rope per warstwa (dwie podstawy
            // rope, head_dim 256/512), czego fused `qkv_post` nie wyraża.
            let fused_qkv_allowed = p.alt_attn.is_none();
            let attn_qkv = match if fused_qkv_allowed {
                fuse_rows(vec![q, k, v])
            } else {
                Err(vec![q, k, v])
            } {
                Ok(fused) => {
                    fused_qkv_layers += 1;
                    QkvWeights::Fused(upload_target_weight(device, fused, target_tile, nvfp4_ct)?)
                }
                Err(mut parts) => {
                    let v = parts.pop().expect("three parts");
                    let k = parts.pop().expect("three parts");
                    let q = parts.pop().expect("three parts");
                    match if fused_qkv_allowed {
                        fuse_rows(vec![q, k])
                    } else {
                        Err(vec![q, k])
                    } {
                        Ok(qk) => {
                            fused_qk_layers += 1;
                            QkvWeights::FusedQk {
                                qk: upload_target_weight(device, qk, target_tile, nvfp4_ct)?,
                                v: upload_target_weight(device, v, target_tile, nvfp4_ct)?,
                            }
                        }
                        Err(mut qk_parts) => {
                            let k = qk_parts.pop().expect("two parts");
                            let q = qk_parts.pop().expect("two parts");
                            QkvWeights::Split {
                                q: upload_target_weight(device, q, target_tile, nvfp4_ct)?,
                                k: upload_target_weight(device, k, target_tile, nvfp4_ct)?,
                                v: upload_target_weight(device, v, target_tile, nvfp4_ct)?,
                            }
                        }
                    }
                }
            };

            let attn_o = fetch_matrix(src, name(WeightRole::AttnO)?)?;
            expect(&at("attn_o"), &attn_o, p.hidden_size, q_dim)?;
            let attn_o = shard_host_weight(attn_o, &plan(WeightRole::AttnO)?)?;

            let ffn = match &p.moe {
                None => {
                    let gate = fetch_matrix(src, name(WeightRole::FfnGate)?)?;
                    let up = fetch_matrix(src, name(WeightRole::FfnUp)?)?;
                    expect(&at("ffn_gate"), &gate, p.intermediate_size, p.hidden_size)?;
                    expect(&at("ffn_up"), &up, p.intermediate_size, p.hidden_size)?;
                    let gate = shard_host_weight(gate, &plan(WeightRole::FfnGate)?)?;
                    let up = shard_host_weight(up, &plan(WeightRole::FfnUp)?)?;
                    let gate_up = match fuse_rows(vec![gate, up]) {
                        Ok(fused) => {
                            fused_gate_up_layers += 1;
                            GateUpWeights::Fused(upload_target_weight(
                                device,
                                fused,
                                target_tile,
                                nvfp4_ct,
                            )?)
                        }
                        Err(mut parts) => {
                            let up = parts.pop().expect("two parts");
                            let gate = parts.pop().expect("two parts");
                            GateUpWeights::Split {
                                gate: upload_target_weight(device, gate, target_tile, nvfp4_ct)?,
                                up: upload_target_weight(device, up, target_tile, nvfp4_ct)?,
                            }
                        }
                    };
                    let down = fetch_matrix(src, name(WeightRole::FfnDown)?)?;
                    expect(&at("ffn_down"), &down, p.hidden_size, p.intermediate_size)?;
                    let down = shard_host_weight(down, &plan(WeightRole::FfnDown)?)?;
                    LayerFfn::Dense(DenseFfn {
                        gate_up,
                        down: upload_target_weight(device, down, target_tile, nvfp4_ct)?,
                    })
                }
                Some(moe) => {
                    if shard.world > 1 {
                        return Err(ForgeError::Unsupported(
                            "podział tensor-parallel nie obejmuje jeszcze warstw MoE".into(),
                        ));
                    }
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
                        gate_exps: upload_expert_stack(
                            device,
                            gate_exps,
                            moe.n_experts,
                            spill,
                            budget.as_ref(),
                        )?,
                        up_exps: upload_expert_stack(
                            device,
                            up_exps,
                            moe.n_experts,
                            spill,
                            budget.as_ref(),
                        )?,
                        down_exps: upload_expert_stack(
                            device,
                            down_exps,
                            moe.n_experts,
                            spill,
                            budget.as_ref(),
                        )?,
                        shared,
                        shared_gate: None,
                        n_experts: moe.n_experts,
                        n_experts_used: moe.n_experts_used,
                        moe_inter: moe.moe_intermediate_size,
                        norm_topk: moe.norm_topk_prob,
                        gate_bias: None,
                        tid2eid: None,
                        usage: ExpertUsage::new(device, moe.n_experts)?,
                    }))
                }
            };

            layers.push(LayerWeights {
                hc_attn: None,
                hc_ffn: None,
                attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
                ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
                post_attn_norm: match layer_map.get(&WeightRole::PostAttnNorm) {
                    Some(n) => Some(upload_norm(device, src, n)?),
                    None => None,
                },
                post_ffw_norm: match layer_map.get(&WeightRole::PostFfwNorm) {
                    Some(n) => Some(upload_norm(device, src, n)?),
                    None => None,
                },
                layer_output_scale: match layer_map.get(&WeightRole::LayerOutputScale) {
                    Some(n) => Some(load_scalar_f32(src, n)?),
                    None => None,
                },
                mixer: LayerMixer::Attention(Box::new(AttnWeights {
                    q_norm: match layer_map.get(&WeightRole::AttnQNorm) {
                        Some(n) => Some(upload_norm(device, src, n)?),
                        None => None,
                    },
                    k_norm: match layer_map.get(&WeightRole::AttnKNorm) {
                        Some(n) => Some(upload_norm(device, src, n)?),
                        None => None,
                    },
                    v_norm: if descriptor.params.v_rms_norm {
                        Some(upload(
                            device,
                            &f32s_to_f16_bytes(&vec![1.0f32; descriptor.params.max_head_dim()]),
                        )?)
                    } else {
                        None
                    },
                    attn_qkv,
                    attn_o: upload_target_weight(device, attn_o, target_tile, nvfp4_ct)?,
                })),
                ffn,
            });
        }

        // Deskryptor zostaje w modelu z kształtami RANGI: mniej głowic, mniejszy
        // wymiar pośredni. Dzięki temu KV, bufory aktywacji i cała pętla warstw
        // widzą po prostu mniejszy model, bez przewlekania zakresu głowic.
        descriptor.params = full_params.shard(shard)?;
        let mtp = if native_mtp {
            let mtp_descriptor = descriptor.mtp.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("model nie zawiera głowy MTP/NextN".into())
            })?;
            let mut loader = SourceMtpLoader {
                device,
                source: src,
                nvfp4_ct,
            };
            Some(MtpWeights::load(
                mtp_descriptor,
                &descriptor.params,
                &mut loader,
                &token_embd_f16,
                MtpEmbedding::Device(DevWeight::F16 {
                    buf: token_embd_f16.clone(),
                    rows: vocab,
                    cols: hidden,
                }),
                &lm_head,
            )?)
        } else {
            None
        };

        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            token_embd_host: None,
            output_norm,
            rope_freqs,
            neg_inf,
            lm_head,
            fp8_lm_head: None,
            layers,
            fused_qkv_layers,
            fused_qk_layers,
            fused_gate_up_layers,
            w4a8: None,
            fp8: None,
            fp8_ffn: None,
            fp8_modular: false,
            hc_head: None,
            mtp,
            nvfp4_repacked_weights: 0,
        })
    }

    /// Load the qwen35moe hybrid stack: interleaved gated-attention and
    /// Gated-DeltaNet layers, each with a routed + gated-shared MoE FFN. The
    /// attention Q projection is gated (width `2*n_heads*head_dim`) so it is
    /// stored split (no q/k/v fusion); DeltaNet layers carry the SSM weight set.
    /// Składa model DeepSeeka V4. Osobna ścieżka, bo ta architektura ma inny
    /// mikser, hyper-connections zamiast rezyduum, eksperty zapisane pojedynczo
    /// i głowę wyjściową z własną redukcją kopii strumienia.
    fn load_deepseek_v4(
        device: &dyn Device,
        descriptor: ModelDescriptor,
        src: &dyn TensorSource,
        spill: Option<&ExpertSpill>,
        host_budget: usize,
    ) -> Result<Self> {
        let embd_name = descriptor
            .globals
            .get(&WeightRole::TokenEmbd)
            .ok_or_else(|| ForgeError::Format("DeepSeek V4: brak embeddingu".into()))?
            .clone();
        let norm_name = descriptor
            .globals
            .get(&WeightRole::OutputNorm)
            .ok_or_else(|| ForgeError::Format("DeepSeek V4: brak normy wyjściowej".into()))?
            .clone();
        let head_name = descriptor
            .globals
            .get(&WeightRole::LmHead)
            .ok_or_else(|| ForgeError::Format("DeepSeek V4: brak głowy logitów".into()))?
            .clone();

        let embedding = upload_weight(device, fetch_matrix(src, &embd_name)?)?;
        let DevWeight::F16 {
            buf: token_embd_f16,
            ..
        } = embedding
        else {
            return Err(ForgeError::Unsupported(
                "embedding DeepSeeka musi dać się zmaterializować jako f16".into(),
            ));
        };
        let output_norm = upload_norm(device, src, &norm_name)?;
        let lm_head = upload_weight(device, fetch_matrix(src, &head_name)?)?;
        let head_role = |role: WeightRole| -> Result<String> {
            descriptor
                .globals
                .get(&role)
                .cloned()
                .ok_or_else(|| ForgeError::Format(format!("DeepSeek V4: brak roli {role:?}")))
        };
        let head_fn = head_role(WeightRole::HcHeadFn)?;
        let head_base = head_role(WeightRole::HcHeadBase)?;
        let head_scale = head_role(WeightRole::HcHeadScale)?;
        let budget = expert_residency_budget(device, &descriptor, src, host_budget);
        let layers = load_deepseek_layers(device, &descriptor, src, spill, budget.as_ref())?;
        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            token_embd_host: None,
            output_norm,
            rope_freqs: None,
            neg_inf: None,
            lm_head,
            fp8_lm_head: None,
            layers,
            fused_qkv_layers: 0,
            fused_qk_layers: 0,
            fused_gate_up_layers: 0,
            w4a8: None,
            fp8: None,
            fp8_ffn: None,
            fp8_modular: false,
            hc_head: Some(HyperConnectionWeights {
                mix_fn: upload_f32(device, src, &head_fn)?,
                base: upload_f32(device, src, &head_base)?,
                scale: upload_f32(device, src, &head_scale)?,
            }),
            mtp: None,
            nvfp4_repacked_weights: 0,
        })
    }

    fn load_hybrid(
        device: &dyn Device,
        descriptor: ModelDescriptor,
        src: &dyn TensorSource,
        native_mtp: bool,
        target_tile: Option<(&Kernels, &forge_hal::Stream, &Cell<usize>)>,
        nvfp4_ct: Option<&NvFp4CtUploadContext<'_>>,
        spill: Option<&ExpertSpill>,
        host_budget: usize,
        shard: TpShard,
    ) -> Result<Self> {
        let budget = expert_residency_budget(device, &descriptor, src, host_budget);
        // Plan cięcia opisuje wycinek PEŁNEJ macierzy, więc liczy się z pełnych
        // hiperparametrów; deskryptor, który zostaje w modelu, ma już kształty
        // rangi, żeby cała reszta silnika widziała po prostu mniejszy model.
        let full_params = descriptor.params.clone();
        let plan = |role: WeightRole| -> Result<RoleShard> { shard.role_shard(&full_params, role) };
        let global = |role: WeightRole| -> Result<&String> {
            descriptor
                .globals
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("missing global weight {role:?}")))
        };
        let embd_name = global(WeightRole::TokenEmbd)?;
        // The embedding table (~1 GiB f16) stays on the host so the 22 GB of
        // quantized weights fit VRAM; the gather runs host-side per token.
        let (host_embed, host_embedding, vocab, hidden) = fetch_embedding_host(src, embd_name)?;
        let token_embd_f16 = upload(device, &vec![0u8; hidden * 2])?;
        let output_norm = upload_norm(device, src, global(WeightRole::OutputNorm)?)?;
        let rope_freqs = match descriptor.globals.get(&WeightRole::RopeFreqs) {
            Some(n) => Some(upload_f32(device, src, n)?),
            None => None,
        };
        let neg_inf = if descriptor.params.suppress_tokens.is_empty() {
            None
        } else {
            Some(upload(device, &f32::NEG_INFINITY.to_le_bytes())?)
        };
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
                ..
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
        let moe = p.moe.clone();

        let mut layers = Vec::with_capacity(p.block_count);
        for (idx, layer_map) in descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };

            let mixer = match descriptor.layer_kinds[idx] {
                LayerKind::Attention => {
                    let sharded = |role: WeightRole| -> Result<HostWeight> {
                        shard_host_weight(fetch_matrix(src, name(role)?)?, &plan(role)?)
                    };
                    let q = upload_target_weight(
                        device,
                        sharded(WeightRole::AttnQ)?,
                        target_tile,
                        nvfp4_ct,
                    )?;
                    let k = upload_target_weight(
                        device,
                        sharded(WeightRole::AttnK)?,
                        target_tile,
                        nvfp4_ct,
                    )?;
                    let v = upload_target_weight(
                        device,
                        sharded(WeightRole::AttnV)?,
                        target_tile,
                        nvfp4_ct,
                    )?;
                    let attn_o = upload_target_weight(
                        device,
                        sharded(WeightRole::AttnO)?,
                        target_tile,
                        nvfp4_ct,
                    )?;
                    LayerMixer::Attention(Box::new(AttnWeights {
                        q_norm: Some(upload_norm(device, src, name(WeightRole::AttnQNorm)?)?),
                        k_norm: Some(upload_norm(device, src, name(WeightRole::AttnKNorm)?)?),
                        v_norm: None,
                        attn_qkv: QkvWeights::Split { q, k, v },
                        attn_o,
                    }))
                }
                LayerKind::DeltaNet => {
                    let sharded = |role: WeightRole| -> Result<HostWeight> {
                        shard_host_weight(fetch_matrix(src, name(role)?)?, &plan(role)?)
                    };
                    let d_conv = full_params
                        .ssm
                        .as_ref()
                        .expect("hybryda ma parametry SSM")
                        .d_conv;
                    LayerMixer::DeltaNet(Box::new(DeltaNetWeights {
                        in_proj: upload_target_weight(
                            device,
                            sharded(WeightRole::SsmInProj)?,
                            target_tile,
                            nvfp4_ct,
                        )?,
                        gate_proj: upload_target_weight(
                            device,
                            sharded(WeightRole::SsmGate)?,
                            target_tile,
                            nvfp4_ct,
                        )?,
                        // Splot ma `d_conv` wartości na kanał, a plan liczy
                        // kanały — te same zakresy co wiersze `in_proj`.
                        conv1d: upload_norm_shard(
                            device,
                            src,
                            name(WeightRole::SsmConv1d)?,
                            &plan(WeightRole::SsmConv1d)?,
                            d_conv,
                        )?,
                        dt_bias: upload_norm_shard(
                            device,
                            src,
                            name(WeightRole::SsmDt)?,
                            &plan(WeightRole::SsmDt)?,
                            1,
                        )?,
                        a: upload_norm_shard(
                            device,
                            src,
                            name(WeightRole::SsmA)?,
                            &plan(WeightRole::SsmA)?,
                            1,
                        )?,
                        beta_proj: upload_target_weight(
                            device,
                            sharded(WeightRole::SsmBeta)?,
                            target_tile,
                            nvfp4_ct,
                        )?,
                        alpha_proj: upload_target_weight(
                            device,
                            sharded(WeightRole::SsmAlpha)?,
                            target_tile,
                            nvfp4_ct,
                        )?,
                        ssm_norm: upload_norm(device, src, name(WeightRole::SsmNorm)?)?,
                        out_proj: upload_target_weight(
                            device,
                            sharded(WeightRole::SsmOut)?,
                            target_tile,
                            nvfp4_ct,
                        )?,
                    }))
                }
            };

            let ffn = if let Some(moe) = &moe {
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
                let sh_gate =
                    upload_weight(device, fetch_matrix(src, name(WeightRole::FfnGateShExp)?)?)?;
                let sh_up =
                    upload_weight(device, fetch_matrix(src, name(WeightRole::FfnUpShExp)?)?)?;
                let sh_down =
                    upload_weight(device, fetch_matrix(src, name(WeightRole::FfnDownShExp)?)?)?;
                let shared_gate =
                    load_vector_weight(device, src, name(WeightRole::FfnGateInpShExp)?)?;

                LayerFfn::Moe(Box::new(MoeFfn {
                    router,
                    gate_exps: upload_expert_stack(
                        device,
                        gate_exps,
                        moe.n_experts,
                        spill,
                        budget.as_ref(),
                    )?,
                    up_exps: upload_expert_stack(
                        device,
                        up_exps,
                        moe.n_experts,
                        spill,
                        budget.as_ref(),
                    )?,
                    down_exps: upload_expert_stack(
                        device,
                        down_exps,
                        moe.n_experts,
                        spill,
                        budget.as_ref(),
                    )?,
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
                    gate_bias: None,
                    tid2eid: None,
                    usage: ExpertUsage::new(device, moe.n_experts)?,
                }))
            } else {
                let sharded = |role: WeightRole| -> Result<HostWeight> {
                    shard_host_weight(fetch_matrix(src, name(role)?)?, &plan(role)?)
                };
                let gate = upload_target_weight(
                    device,
                    sharded(WeightRole::FfnGate)?,
                    target_tile,
                    nvfp4_ct,
                )?;
                let up = upload_target_weight(
                    device,
                    sharded(WeightRole::FfnUp)?,
                    target_tile,
                    nvfp4_ct,
                )?;
                let down = upload_target_weight(
                    device,
                    sharded(WeightRole::FfnDown)?,
                    target_tile,
                    nvfp4_ct,
                )?;
                LayerFfn::Dense(DenseFfn {
                    gate_up: GateUpWeights::Split { gate, up },
                    down,
                })
            };

            layers.push(LayerWeights {
                attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
                ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
                post_attn_norm: match layer_map.get(&WeightRole::PostAttnNorm) {
                    Some(n) => Some(upload_norm(device, src, n)?),
                    None => None,
                },
                post_ffw_norm: match layer_map.get(&WeightRole::PostFfwNorm) {
                    Some(n) => Some(upload_norm(device, src, n)?),
                    None => None,
                },
                layer_output_scale: match layer_map.get(&WeightRole::LayerOutputScale) {
                    Some(n) => Some(load_scalar_f32(src, n)?),
                    None => None,
                },
                mixer,
                ffn,
                hc_attn: None,
                hc_ffn: None,
            });
        }

        let mtp = if native_mtp {
            let mtp_descriptor = descriptor.mtp.as_ref().ok_or_else(|| {
                ForgeError::Unsupported("model nie zawiera głowy MTP/NextN".into())
            })?;
            let embedding_bytes = host_embedding.mtp_device_bytes().ok_or_else(|| {
                ForgeError::Unsupported(
                    "MTP hybrid obsługuje embedding Q8_0, Q4_K lub GGUF NVFP4".into(),
                )
            })?;
            let mut loader = SourceMtpLoader {
                device,
                source: src,
                nvfp4_ct,
            };
            let mut weights = MtpWeights::load(
                mtp_descriptor,
                &descriptor.params,
                &mut loader,
                &token_embd_f16,
                MtpEmbedding::HostF16,
                &lm_head,
            )?;
            if matches!(&weights.embedding, MtpEmbedding::HostF16) {
                let aligned_bytes = embedding_bytes
                    .checked_add(255)
                    .map(|bytes| bytes & !255)
                    .ok_or_else(|| ForgeError::OutOfMemory {
                        requested: embedding_bytes,
                        available: device.pool_available(Pool::Weights).unwrap_or(0),
                    })?;
                let requested =
                    aligned_bytes
                        .checked_add(64 << 20)
                        .ok_or_else(|| ForgeError::OutOfMemory {
                            requested: aligned_bytes,
                            available: device.pool_available(Pool::Weights).unwrap_or(0),
                        })?;
                let available = device.pool_available(Pool::Weights).unwrap_or(0);
                let embedding_mode =
                    std::env::var("FORGE_MTP_EMBEDDING").unwrap_or_else(|_| "auto".into());
                let use_device = match embedding_mode.as_str() {
                    "auto" => available >= requested,
                    "device" if available >= requested => true,
                    "device" => {
                        return Err(ForgeError::OutOfMemory {
                            requested,
                            available,
                        })
                    }
                    "host" => false,
                    value => {
                        return Err(ForgeError::Unsupported(format!(
                            "FORGE_MTP_EMBEDDING={value}: oczekiwano auto, device lub host"
                        )))
                    }
                };
                if use_device {
                    match upload_weight(device, host_embedding) {
                        Ok(embedding) => weights.embedding = MtpEmbedding::Device(embedding),
                        Err(_) if embedding_mode == "auto" => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            Some(weights)
        } else {
            None
        };

        // Deskryptor, który zostaje w modelu, ma kształty RANGI — dzięki temu
        // cała reszta silnika (bufory, KV, stan DeltaNet, kernele) widzi po
        // prostu mniejszy model i nie potrzebuje ani jednego wpięcia.
        let mut descriptor = descriptor;
        descriptor.params = full_params.shard(shard)?;
        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            token_embd_host: Some(host_embed),
            output_norm,
            rope_freqs,
            neg_inf,
            lm_head,
            fp8_lm_head: None,
            layers,
            fused_qkv_layers: 0,
            fused_qk_layers: 0,
            fused_gate_up_layers: 0,
            w4a8: None,
            fp8: None,
            fp8_ffn: None,
            fp8_modular: false,
            hc_head: None,
            mtp,
            nvfp4_repacked_weights: 0,
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
        const ROLES: [WeightRole; 7] = [
            WeightRole::AttnQ,
            WeightRole::AttnK,
            WeightRole::AttnV,
            WeightRole::AttnO,
            WeightRole::FfnGate,
            WeightRole::FfnUp,
            WeightRole::FfnDown,
        ];
        for (idx, layer_map) in self.descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };
            // The dequant + e4m3 code computation dominates the build (f32
            // expansion of every projection); run the layer's seven
            // projections on worker threads and keep the device uploads on
            // this thread. Peak host memory stays one layer of f32.
            let mut packed =
                std::thread::scope(|scope| -> Result<Vec<(Vec<u8>, Vec<u8>, usize, usize)>> {
                    let handles: Vec<_> = ROLES
                        .iter()
                        .map(|&role| {
                            let src = &src;
                            scope.spawn(move || -> Result<(Vec<u8>, Vec<u8>, usize, usize)> {
                                let (w, r, c) = dequant_matrix_f32_fp8(src, name(role)?)?;
                                let (codes, scales) = pack_fp8_host(&w, r, c);
                                Ok((codes, scales, r, c))
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join().map_err(|_| {
                                ForgeError::Format("fp8 pack worker panicked".into())
                            })?
                        })
                        .collect()
                })?
                .into_iter();
            let mut next = || -> Result<Fp8Weight> {
                let (codes, scales, rows, cols) = packed.next().expect("seven packed projections");
                Ok(Fp8Weight {
                    qweight: upload(device, &codes)?,
                    scales: upload(device, &scales)?,
                    rows,
                    cols,
                })
            };
            out.push(Fp8Layer {
                q: next()?,
                k: next()?,
                v: next()?,
                attn_o: next()?,
                gate: next()?,
                up: next()?,
                down: next()?,
            });
        }
        Ok(out)
    }

    /// Whether this model uses routed Mixture-of-Experts FFN blocks.
    pub fn is_moe(&self) -> bool {
        self.descriptor.params.moe.is_some()
    }

    /// Whether every dense projection has a small-batch decode kernel family
    /// (NVFP4 B4/B8/B16/BM32 GEMV; Q4_K/Q6_K/Q8_0 weight-stationary batch),
    /// so a batched decode step costs roughly one weight sweep instead of a
    /// fixed >=64-token GEMM tile. Decides the batched-path engagement
    /// default: 2 with small-batch kernels, else the tile cost only amortizes
    /// at ~12 concurrent sequences.
    pub fn small_batch_decode_capable(&self) -> bool {
        let small = |w: &DevWeight| {
            matches!(
                w,
                DevWeight::NvFp4 { .. }
                    | DevWeight::NvFp4Gguf { .. }
                    | DevWeight::Q4K { .. }
                    | DevWeight::Q6K { .. }
                    | DevWeight::Q8_0 { .. }
            )
        };
        self.layers.iter().all(|layer| {
            let mixer_ok = match &layer.mixer {
                // DeepSeek V4 nie przechodzi przepakowaniem docelowego kafla.
                LayerMixer::DeepseekAttention(_) => false,
                LayerMixer::Attention(a) => {
                    let qkv_ok = match &a.attn_qkv {
                        QkvWeights::Fused(w) => small(w),
                        QkvWeights::FusedQk { qk, v } => small(qk) && small(v),
                        QkvWeights::Split { q, k, v } => small(q) && small(k) && small(v),
                    };
                    qkv_ok && small(&a.attn_o)
                }
                LayerMixer::DeltaNet(_) => false,
            };
            let ffn_ok = match &layer.ffn {
                LayerFfn::Dense(d) => {
                    let gu_ok = match &d.gate_up {
                        GateUpWeights::Fused(w) => small(w),
                        GateUpWeights::Split { gate, up } => small(gate) && small(up),
                    };
                    gu_ok && small(&d.down)
                }
                LayerFfn::Moe(_) => false,
            };
            mixer_ok && ffn_ok
        })
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
        m.insert("fused_qkv_layers".into(), self.fused_qkv_layers.to_string());
        m.insert("fused_qk_layers".into(), self.fused_qk_layers.to_string());
        m.insert(
            "fused_gate_up_layers".into(),
            self.fused_gate_up_layers.to_string(),
        );
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nvfp4EmbeddingSource {
        scale: f32,
        scale_dims: Vec<usize>,
    }

    impl TensorSource for Nvfp4EmbeddingSource {
        fn byte_len(&self, _name: &str) -> Option<usize> {
            None
        }

        fn fetch(&self, name: &str) -> Result<TensorFetch> {
            match name {
                "token_embd.weight" => {
                    let mut block = vec![0u8; 36];
                    block[0] = 0x38;
                    block[4] = 0x01;
                    Ok((block, DType::U8, QuantKind::NVFP4Gguf, vec![1, 64]))
                }
                "token_embd.scale" => Ok((
                    self.scale.to_le_bytes().to_vec(),
                    DType::F32,
                    QuantKind::None,
                    self.scale_dims.clone(),
                )),
                _ => Err(ForgeError::Format(format!("brak tensora {name}"))),
            }
        }

        fn fetch_optional(&self, name: &str) -> Result<Option<TensorFetch>> {
            if name == "token_embd.scale" {
                self.fetch(name).map(Some)
            } else {
                Ok(None)
            }
        }

        fn fetch_nvfp4(&self, _name: &str) -> Result<Option<NvFp4Host>> {
            Ok(None)
        }

        fn fetch_fp8(&self, _name: &str) -> Result<Option<Fp8Host>> {
            Ok(None)
        }
    }

    #[test]
    fn embedding_nvfp4_respektuje_skale_companion() {
        let source = Nvfp4EmbeddingSource {
            scale: 0.25,
            scale_dims: vec![1],
        };
        let (host, weight, rows, cols) =
            fetch_embedding_host(&source, "token_embd.weight").unwrap();
        assert_eq!((rows, cols), (1, 64));
        assert_eq!(host[0], f16::from_f32(0.125));
        assert!(matches!(
            weight,
            HostWeight::NvFp4Gguf {
                output_scale: 0.25,
                ..
            }
        ));
    }

    #[test]
    fn embedding_nvfp4_odrzuca_nieprawidlowa_skale_companion() {
        for scale in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            let source = Nvfp4EmbeddingSource {
                scale,
                scale_dims: vec![1],
            };
            assert!(fetch_embedding_host(&source, "token_embd.weight").is_err());
        }
        let source = Nvfp4EmbeddingSource {
            scale: 1.0,
            scale_dims: vec![2],
        };
        assert!(fetch_embedding_host(&source, "token_embd.weight").is_err());
    }

    #[test]
    fn keeps_gguf_nvfp4_in_native_layout() {
        let mut block = vec![0u8; 36];
        block[..4].copy_from_slice(&[0x38, 0x40, 0x48, 0x7f]);
        for subblock in 0..4 {
            block[4 + subblock * 8..4 + (subblock + 1) * 8]
                .copy_from_slice(&[0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe]);
        }

        let expected = block.clone();
        let converted = quant_host_weight(
            "weight",
            block,
            DType::U8,
            QuantKind::NVFP4Gguf,
            1,
            64,
            0.25,
        )
        .expect("zachowaj NVFP4");
        let HostWeight::NvFp4Gguf {
            data,
            output_scale,
            rows,
            cols,
        } = converted
        else {
            panic!("oczekiwano wagi GGUF NVFP4");
        };
        assert_eq!(rows, 1);
        assert_eq!(cols, 64);
        assert_eq!(output_scale, 0.25);
        assert_eq!(data, expected);
    }

    #[test]
    fn rejects_invalid_gguf_nvfp4_layout() {
        assert!(quant_host_weight(
            "weight",
            vec![0; 36],
            DType::U8,
            QuantKind::NVFP4Gguf,
            1,
            63,
            1.0,
        )
        .is_err());
        for output_scale in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            assert!(quant_host_weight(
                "weight",
                vec![0; 36],
                DType::U8,
                QuantKind::NVFP4Gguf,
                1,
                64,
                output_scale,
            )
            .is_err());
        }
    }

    #[test]
    fn nvfp4_ct_cleanup_zachowuje_pierwotny_blad_ladowania() {
        let load = Err::<(), _>(ForgeError::Format("load".into()));
        let reset = Err(ForgeError::Kernel("reset".into()));
        let error = finish_nvfp4_ct_load(load, reset).unwrap_err();
        assert!(matches!(error, ForgeError::Format(message) if message == "load"));
    }

    #[test]
    fn nvfp4_ct_cleanup_zwraca_blad_reset_po_sukcesie() {
        let reset = Err(ForgeError::Kernel("reset".into()));
        let error = finish_nvfp4_ct_load(Ok(()), reset).unwrap_err();
        assert!(matches!(error, ForgeError::Kernel(message) if message == "reset"));
    }

    #[test]
    fn nvfp4_ct_druga_alokacja_scratch_resetuje_generation() {
        let allocations = Cell::new(0usize);
        let resets = Cell::new(0usize);
        let error = allocate_nvfp4_ct_scratch(
            7,
            1,
            |bytes| {
                allocations.set(allocations.get() + 1);
                if allocations.get() == 2 {
                    Err(ForgeError::OutOfMemory {
                        requested: bytes,
                        available: 0,
                    })
                } else {
                    Ok(bytes)
                }
            },
            || {
                resets.set(resets.get() + 1);
                Err(ForgeError::Kernel("reset".into()))
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ForgeError::OutOfMemory {
                requested: 1,
                available: 0
            }
        ));
        assert_eq!(resets.get(), 1);
    }

    #[test]
    fn nvfp4_ct_policy_jest_fail_closed() {
        assert_eq!(
            resolve_nvfp4_ct_plan(NvFp4CtLayoutPolicy::Auto, false, true, 12).unwrap(),
            NvFp4CtLoadPlan::RowMajorE4M3
        );
        assert!(resolve_nvfp4_ct_plan(NvFp4CtLayoutPolicy::S0N64K128, false, true, 12).is_err());
        assert!(resolve_nvfp4_ct_plan(NvFp4CtLayoutPolicy::S0N64K128, true, false, 12).is_err());
        assert_eq!(
            resolve_nvfp4_ct_plan(NvFp4CtLayoutPolicy::Auto, true, true, 12).unwrap(),
            NvFp4CtLoadPlan::S0N64K128
        );
        assert_eq!(
            crate::model::ModelConfig::default().nvfp4_ct_layout,
            NvFp4CtLayoutPolicy::RowMajorE4M3
        );
    }

    #[test]
    fn nvfp4_ct_okno_wymaga_pelnych_kafli_n64() {
        let device = forge_hal::cpu::CpuDevice::new();
        let data = device
            .alloc(256 * 128 * 9 / 16, MemKind::Device, Pool::Weights)
            .unwrap();
        let weight = DevWeight::NvFp4 {
            storage: NvFp4CtStorage::S0N64K128 { data },
            inv_global_scale: 1.0,
            rows: 256,
            cols: 128,
        };
        let window = weight.nvfp4_ct_row_window(64, 128).unwrap();
        assert_eq!(window.row_offset(), 64);
        assert_eq!(window.rows(), 128);
        assert_eq!(window.physical_rows(), 256);
        assert_eq!(window.cols(), 128);
        assert!(weight.nvfp4_ct_row_window(1, 64).is_err());
        assert!(weight.nvfp4_ct_row_window(64, 63).is_err());
        assert!(weight.nvfp4_ct_row_window(192, 128).is_err());
        let row_major = DevWeight::NvFp4 {
            storage: NvFp4CtStorage::RowMajorE4M3 {
                packed: device.alloc(64, MemKind::Device, Pool::Weights).unwrap(),
                scales: device.alloc(8, MemKind::Device, Pool::Weights).unwrap(),
            },
            inv_global_scale: 1.0,
            rows: 64,
            cols: 128,
        };
        assert!(row_major.nvfp4_ct_row_window(0, 64).is_err());
        for bytes in [256 * 128 * 9 / 16 - 1, 256 * 128 * 9 / 16 + 1] {
            let malformed = DevWeight::NvFp4 {
                storage: NvFp4CtStorage::S0N64K128 {
                    data: device.alloc(bytes, MemKind::Device, Pool::Weights).unwrap(),
                },
                inv_global_scale: 1.0,
                rows: 256,
                cols: 128,
            };
            assert!(malformed.nvfp4_ct_row_window(0, 64).is_err());
        }
    }

    #[test]
    fn nvfp4_ct_resident_sprawdza_wyrownanie_i_przepelnienie() {
        assert_eq!(nvfp4_ct_s0_resident_bytes(64, 128).unwrap(), 4608);
        assert!(nvfp4_ct_s0_resident_bytes(0, 128).is_err());
        assert!(nvfp4_ct_s0_resident_bytes(64, 0).is_err());
        assert!(nvfp4_ct_s0_resident_bytes(63, 128).is_err());
        assert!(nvfp4_ct_s0_resident_bytes(64, 127).is_err());
        let overflowing_rows = (usize::MAX / 64) * 64;
        assert!(nvfp4_ct_s0_resident_bytes(overflowing_rows, 128).is_err());
    }

    #[test]
    fn nvfp4_ct_preflight_odrzuca_zerowe_i_bledne_metadane() {
        assert_eq!(
            validate_nvfp4_ct_packed_metadata("packed", DType::U8, &[64, 64], 16).unwrap(),
            (64, 128)
        );
        assert!(validate_nvfp4_ct_packed_metadata("packed", DType::F16, &[64, 64], 16).is_err());
        assert!(validate_nvfp4_ct_packed_metadata("packed", DType::U8, &[64], 16).is_err());
        assert!(validate_nvfp4_ct_packed_metadata("packed", DType::U8, &[0, 64], 16).is_err());
        assert!(validate_nvfp4_ct_packed_metadata("packed", DType::U8, &[64, 0], 16).is_err());
        assert!(
            validate_nvfp4_ct_packed_metadata("packed", DType::U8, &[64, usize::MAX], 16).is_err()
        );

        assert_eq!(
            validate_nvfp4_ct_scale_metadata("scale", DType::F8E4M3, &[64, 8], 64, 128).unwrap(),
            512
        );
        assert!(validate_nvfp4_ct_scale_metadata("scale", DType::U8, &[64, 8], 64, 128).is_err());
        assert!(validate_nvfp4_ct_scale_metadata("scale", DType::F8E4M3, &[512], 64, 128).is_err());
        assert!(validate_nvfp4_ct_scale_metadata(
            "scale",
            DType::F8E4M3,
            &[usize::MAX, 8],
            usize::MAX,
            128,
        )
        .is_err());

        assert!(validate_nvfp4_ct_global_scale_metadata(
            "global",
            DType::F32,
            &[1],
            &1.0f32.to_le_bytes(),
        )
        .is_ok());
        assert!(validate_nvfp4_ct_global_scale_metadata(
            "global",
            DType::F16,
            &[1],
            &1.0f32.to_le_bytes(),
        )
        .is_err());
        assert!(validate_nvfp4_ct_global_scale_metadata(
            "global",
            DType::F32,
            &[],
            &1.0f32.to_le_bytes(),
        )
        .is_err());
    }

    #[test]
    fn nvfp4_ct_preflight_odrzuca_nan_i_ujemne_skale_e4m3() {
        assert!(validate_nvfp4_ct_scale_bytes("scale", &[0x00, 0x01, 0x7e]).is_ok());
        for invalid in [0x7f, 0x80, 0xff] {
            assert!(validate_nvfp4_ct_scale_bytes("scale", &[invalid]).is_err());
        }
    }

    #[test]
    fn nvfp4_ct_preflight_wymaga_pelnej_trojki_w_obie_strony() {
        assert!(!validate_nvfp4_ct_companions("w", [false; 3]).unwrap());
        assert!(validate_nvfp4_ct_companions("w", [true; 3]).unwrap());
        for present in [
            [true, false, false],
            [false, true, false],
            [false, false, true],
            [true, true, false],
            [true, false, true],
            [false, true, true],
        ] {
            assert!(validate_nvfp4_ct_companions("w", present).is_err());
        }
    }

    #[test]
    fn nvfp4_ct_preflight_i_upload_musza_miec_ten_sam_zbior() {
        let qkv = nvfp4_ct_upload_identity(
            vec!["q".to_string(), "k".to_string(), "v".to_string()],
            64,
            128,
            1.0f32.to_bits(),
        );
        let down = nvfp4_ct_upload_identity(vec!["down".to_string()], 64, 128, 1.0f32.to_bits());
        let expected = HashSet::from([qkv.clone(), down]);
        let same_count_wrong = HashSet::from([
            qkv,
            nvfp4_ct_upload_identity(
                vec!["gate".to_string(), "up".to_string()],
                64,
                128,
                1.0f32.to_bits(),
            ),
        ]);
        assert!(validate_nvfp4_ct_upload_manifest(
            NvFp4CtLoadPlan::S0N64K128,
            &expected,
            &expected,
        )
        .is_ok());
        assert!(validate_nvfp4_ct_upload_manifest(
            NvFp4CtLoadPlan::S0N64K128,
            &expected,
            &same_count_wrong,
        )
        .is_err());
        assert!(validate_nvfp4_ct_upload_manifest(
            NvFp4CtLoadPlan::RowMajorE4M3,
            &expected,
            &HashSet::new(),
        )
        .is_ok());
    }

    #[test]
    fn nvfp4_ct_tozsamosc_jest_strukturalna() {
        let separator = '\u{1f}';
        let one_name =
            nvfp4_ct_upload_identity(vec![format!("q{separator}k")], 64, 128, 1.0f32.to_bits());
        let two_names = nvfp4_ct_upload_identity(
            vec!["q".to_string(), "k".to_string()],
            64,
            128,
            1.0f32.to_bits(),
        );
        let wrong_rows =
            nvfp4_ct_upload_identity(vec![format!("q{separator}k")], 128, 128, 1.0f32.to_bits());
        let wrong_cols =
            nvfp4_ct_upload_identity(vec![format!("q{separator}k")], 64, 256, 1.0f32.to_bits());
        let wrong_scale =
            nvfp4_ct_upload_identity(vec![format!("q{separator}k")], 64, 128, 2.0f32.to_bits());

        assert_ne!(one_name, two_names);
        assert_ne!(one_name, wrong_rows);
        assert_ne!(one_name, wrong_cols);
        assert_ne!(one_name, wrong_scale);
        assert_eq!(
            HashSet::from([one_name, two_names, wrong_rows, wrong_cols, wrong_scale]).len(),
            5
        );
    }
}

/// Wczytuje głowę logitów z GGUF jako surowe bajty gotowe do podziału.
///
/// Zwraca `None`, gdy głowa jest współdzielona z tablicą embeddingów albo ma
/// format bez kernela z wyjściem f32 — wtedy zostaje w całości na karcie modelu.
/// Głowa jest czytana raz na token, więc jej podział liczy się w dekodowaniu tyle
/// samo co kilka warstw FFN.
pub fn load_lm_head_shard_source(path: &Path) -> Result<Option<(Vec<u8>, usize, usize, QuantKind)>> {
    let gguf = Gguf::open(path)?;
    let descriptor = ModelDescriptor::detect(&gguf)?;
    let Some(name) = descriptor.globals.get(&WeightRole::LmHead) else {
        return Ok(None);
    };
    let src = GgufSource(&gguf);
    match fetch_matrix(&src, name)? {
        HostWeight::Q8_0 { data, rows, cols } => Ok(Some((data, rows, cols, QuantKind::Q8_0))),
        _ => Ok(None),
    }
}

/// Wczytuje dwie duże projekcje wejściowe DeltaNet — `in_proj` i `gate_proj`.
///
/// Na tym modelu to 1350 MiB i 1530 MiB, czyli razem 16,5% całego odczytu na
/// token. Obie czytają ten sam `x` i na karcie idą jednym uruchomieniem
/// grupowym, więc dzieli się je TYM SAMYM udziałem wierszy — rozdzielenie ich
/// między karty w całości rozbijało grupowanie i było wolniejsze.
///
/// Wpis `None` znaczy „ta warstwa nie jest DeltaNetem".
pub fn load_delta_projection_source(
    path: &Path,
) -> Result<Vec<Option<(crate::tensor_parallel::DeltaMatrix, crate::tensor_parallel::DeltaMatrix)>>>
{
    let gguf = Gguf::open(path)?;
    let descriptor = ModelDescriptor::detect(&gguf)?;
    let src = GgufSource(&gguf);
    let matrix = |name: &str| -> Result<Option<crate::tensor_parallel::DeltaMatrix>> {
        Ok(match fetch_matrix(&src, name)? {
            HostWeight::NvFp4Gguf {
                data,
                output_scale,
                rows,
                cols,
            } => Some(crate::tensor_parallel::DeltaMatrix {
                data,
                rows,
                cols,
                quant: QuantKind::NVFP4Gguf,
                output_scale,
            }),
            HostWeight::Q8_0 { data, rows, cols } => Some(crate::tensor_parallel::DeltaMatrix {
                data,
                rows,
                cols,
                quant: QuantKind::Q8_0,
                output_scale: 1.0,
            }),
            HostWeight::Q4K { data, rows, cols } => Some(crate::tensor_parallel::DeltaMatrix {
                data,
                rows,
                cols,
                quant: QuantKind::Q4K,
                output_scale: 1.0,
            }),
            _ => None,
        })
    };
    let mut out = Vec::with_capacity(descriptor.layers.len());
    for layer in descriptor.layers.iter() {
        let (Some(in_name), Some(gate_name)) = (
            layer.get(&WeightRole::SsmInProj),
            layer.get(&WeightRole::SsmGate),
        ) else {
            out.push(None);
            continue;
        };
        out.push(match (matrix(in_name)?, matrix(gate_name)?) {
            (Some(a), Some(b)) if a.cols == b.cols => Some((a, b)),
            _ => None,
        });
    }
    Ok(out)
}

/// Wczytuje wagi FFN modelu GGUF i rozkłada je na karty klastra.
///
/// Osobna ścieżka obok głównego ładowania: silnik trzyma swoje wagi na jednej
/// karcie (prefill liczy je macierzowo), a tensor parallel potrzebuje TYCH
/// SAMYCH macierzy pociętych inaczej. Czytanie ich drugi raz z pliku jest
/// tańsze niż wprowadzanie podziału w środek loadera — i nie dotyka ścieżki,
/// którą jedzie każdy inny model.
pub fn load_ffn_shards_gguf(
    path: &Path,
    cluster: &crate::cluster::Cluster,
    caps: &[crate::multi_gpu::DeviceCapability],
    layer_range: Option<(usize, usize)>,
    forced: Option<&[usize]>,
) -> Result<Vec<crate::tensor_parallel::FfnShards>> {
    let gguf = Gguf::open(path)?;
    let mut descriptor = ModelDescriptor::detect(&gguf)?;
    if let Some((first, count)) = layer_range {
        descriptor.restrict_layers(first, count)?;
    }
    let params = descriptor.params.clone();
    if params.moe.is_some() {
        return Err(ForgeError::Unsupported(
            "podział FFN na karty obejmuje modele gęste".into(),
        ));
    }
    let src = GgufSource(&gguf);
    // Format bloku bierzemy z SAMYCH wag, a nie z deklaracji: `Q4_K_M` trzyma
    // część macierzy w Q4_K, a część w Q6_K, więc nazwa pliku nic tu nie
    // rozstrzyga. Wszystkie trzy macierze FFN warstwy muszą mieć ten sam format,
    // bo dzieli je jeden plan.
    let bytes = |what: &str,
                 w: HostWeight,
                 rows: usize,
                 cols: usize|
     -> Result<(Vec<u8>, crate::tensor_parallel::BlockFormat)> {
        let (data, r, c, quant, scale) = match w {
            HostWeight::Q8_0 { data, rows, cols } => (data, rows, cols, QuantKind::Q8_0, 1.0),
            HostWeight::Q4K { data, rows, cols } => (data, rows, cols, QuantKind::Q4K, 1.0),
            HostWeight::Q6K { data, rows, cols } => (data, rows, cols, QuantKind::Q6K, 1.0),
            HostWeight::NvFp4Gguf {
                data,
                output_scale,
                rows,
                cols,
            } => (data, rows, cols, QuantKind::NVFP4Gguf, output_scale),
            _ => {
                return Err(ForgeError::Unsupported(format!(
                    "podział FFN na karty obejmuje wagi Q8_0, Q4_K, Q6_K i GGUF NVFP4, {what} jest w innym formacie"
                )));
            }
        };
        if r != rows || c != cols {
            return Err(ForgeError::Format(format!(
                "{what}: kształt [{r}, {c}], wymagano [{rows}, {cols}]"
            )));
        }
        Ok((data, crate::tensor_parallel::BlockFormat::of(quant, scale)?))
    };

    // Plan podziału liczony RAZ, z pojemnością na wszystkie warstwy naraz.
    let mut plan: Option<Vec<usize>> = forced.map(|columns| columns.to_vec());
    let mut out = Vec::with_capacity(descriptor.layers.len());
    for (idx, layer_map) in descriptor.layers.iter().enumerate() {
        let name = |role: WeightRole| -> Result<&String> {
            layer_map
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
        };
        let (gate, gate_fmt) = bytes(
            &format!("layer {idx} ffn_gate"),
            fetch_matrix(&src, name(WeightRole::FfnGate)?)?,
            params.intermediate_size,
            params.hidden_size,
        )?;
        let (up, up_fmt) = bytes(
            &format!("layer {idx} ffn_up"),
            fetch_matrix(&src, name(WeightRole::FfnUp)?)?,
            params.intermediate_size,
            params.hidden_size,
        )?;
        let (down, down_fmt) = bytes(
            &format!("layer {idx} ffn_down"),
            fetch_matrix(&src, name(WeightRole::FfnDown)?)?,
            params.hidden_size,
            params.intermediate_size,
        )?;
        // `gate` i `up` dzielą się po WIERSZACH, więc muszą mieć wspólny format
        // (liczy je jedno wywołanie), ale `down` może mieć inny — dzieli się po
        // kolumnach i tylko on stawia warunek na granicę bloku. `Q4_K_M` trzyma
        // właśnie `down` w Q6_K, więc wymaganie jednego formatu dla całej trójki
        // odrzucałoby najpopularniejszą kwantyzację w obiegu.
        // Porównanie idzie po kwantyzacji, nie po całym opisie: skala tensora
        // NVFP4 należy do konkretnej macierzy i `gate` z `up` mają swoje własne.
        if gate_fmt.quant != up_fmt.quant {
            return Err(ForgeError::Unsupported(format!(
                "layer {idx}: gate i up muszą mieć ten sam format, jest {:?} i {:?}",
                gate_fmt.quant, up_fmt.quant
            )));
        }
        let columns = match &plan {
            Some(columns) => columns.clone(),
            None => {
                let columns = crate::tensor_parallel::plan_ffn_split(
                    caps,
                    params.hidden_size,
                    params.intermediate_size,
                    crate::multi_gpu::WorkKind::MemoryBound,
                    gate_fmt,
                    down_fmt,
                    descriptor.layers.len(),
                )?;
                plan = Some(columns.clone());
                columns
            }
        };
        out.push(crate::tensor_parallel::upload_ffn_split(
            cluster,
            caps,
            &gate,
            &up,
            &down,
            params.hidden_size,
            params.intermediate_size,
            crate::multi_gpu::WorkKind::MemoryBound,
            gate_fmt,
            up_fmt,
            down_fmt,
            Some(&columns),
        )?);
    }
    Ok(out)
}


#[cfg(test)]
mod rope_permute_tests {
    use super::*;

    /// Permutacja musi ruszyc OBA bufory NVFP4 compressed-tensors, kazdy swoim
    /// krokiem. Wczesniej ten format w ogole nie mial deklaracji ukladu i
    /// wpadal w galaz "nieobslugiwane"; po dopisaniu go na wyczucie latwo bylo
    /// pomylic krok skal (cols/16) z krokiem wartosci (cols/2).
    #[test]
    fn nvfp4_ct_permutes_values_and_scales_with_their_own_stride() {
        let rows = 4usize; // head_dim = 4, jedna glowa
        let cols = 32usize;
        let packed_row = cols / 2; // 16 B
        let scale_row = cols / 16; // 2 B
        let mut w = HostWeight::NvFp4 {
            names: vec!["t".into()],
            packed: (0..rows)
                .flat_map(|r| std::iter::repeat(r as u8).take(packed_row))
                .collect(),
            scales: (0..rows)
                .flat_map(|r| std::iter::repeat(100 + r as u8).take(scale_row))
                .collect(),
            global_scale: 1.0,
            rows,
            cols,
        };
        permute_rope_pairs(&mut w, 4).expect("permutacja");
        let HostWeight::NvFp4 { packed, scales, .. } = &w else {
            panic!("wariant zmieniony");
        };
        // [0,1,2,3] -> [0,2,1,3]
        let want = [0u8, 2, 1, 3];
        for (i, r) in want.iter().enumerate() {
            assert_eq!(packed[i * packed_row], *r, "wartosci, wiersz {i}");
            assert_eq!(scales[i * scale_row], 100 + *r, "skale, wiersz {i}");
        }
    }

    /// Format jednobuforowy zachowuje sie tak samo — jeden kontrakt dla obu.
    #[test]
    fn single_buffer_format_uses_the_same_contract() {
        let rows = 4usize;
        let row_bytes = 8usize;
        let mut w = HostWeight::F16 {
            data: (0..rows)
                .flat_map(|r| std::iter::repeat(r as u8).take(row_bytes))
                .collect(),
            rows,
            cols: row_bytes / 2,
        };
        permute_rope_pairs(&mut w, 4).expect("permutacja");
        let HostWeight::F16 { data, .. } = &w else {
            panic!("wariant zmieniony");
        };
        for (i, r) in [0u8, 2, 1, 3].iter().enumerate() {
            assert_eq!(data[i * row_bytes], *r, "wiersz {i}");
        }
    }
}
