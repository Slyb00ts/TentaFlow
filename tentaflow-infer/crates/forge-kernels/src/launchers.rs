// ===== File: launchers.rs — typed launch wrappers over kernel artifacts =====
// Argument order and meaning must mirror the Mojo kernel signatures exactly
// (kernels/mojo/src/*.mojo). Mojo `Int` marshals as a 64-bit scalar slot,
// `Float32` as f32.

use std::sync::{Arc, Mutex};

use forge_hal::{DevBuffer, Device, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_types::{DType, ForgeError, MemKind, Result};

use crate::registry::KernelArtifacts;

const BLOCK: u32 = 256;

/// Jedna projekcja surowego GGUF NVFP4 korzystająca ze wspólnej aktywacji Q8_1.
pub struct Nvfp4GgufQ8Projection<'a> {
    pub output: &'a DevBuffer,
    pub weights: &'a DevBuffer,
    pub rows: usize,
    pub output_scale: f32,
}

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

fn checked_buffer_bytes(name: &str, dimensions: &[usize], element_bytes: usize) -> Result<usize> {
    dimensions
        .iter()
        .try_fold(element_bytes, |bytes, dimension| {
            bytes.checked_mul(*dimension).ok_or_else(|| {
                ForgeError::Kernel(format!(
                    "{name}: przepełnienie rozmiaru bufora dla wymiarów {dimensions:?}"
                ))
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Nvfp4GgufDispatch {
    kernel: &'static str,
    token_tile: usize,
    row_tile: usize,
    block_threads: u32,
}

fn nvfp4_gguf_dispatch(
    n_tokens: usize,
    is_nvidia: bool,
    warp_size: u32,
    max_threads: u32,
) -> Result<Nvfp4GgufDispatch> {
    if n_tokens < 2 {
        return Err(ForgeError::Kernel(
            "gemm_nvfp4_gguf_f16 wymaga co najmniej dwóch tokenów".into(),
        ));
    }
    let (kernel, token_tile, row_tile, block_threads) = match n_tokens {
        2 => ("gemm_nvfp4_gguf_f16_b2", 2, 1, Some(warp_size)),
        3 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b3_nvidia",
            3,
            2,
            warp_size.checked_mul(2),
        ),
        4 if is_nvidia && warp_size == 32 => (
            "gemm_nvfp4_gguf_f16_b4_nvidia",
            4,
            2,
            warp_size.checked_mul(2),
        ),
        3 => ("gemm_nvfp4_gguf_f16_b3", 3, 1, Some(warp_size)),
        4 => ("gemm_nvfp4_gguf_f16_b4", 4, 1, Some(warp_size)),
        5..=8 => ("gemm_nvfp4_gguf_f16_b8", 8, 1, warp_size.checked_mul(8)),
        9..=16 => ("gemm_nvfp4_gguf_f16_b16", 16, 1, warp_size.checked_mul(16)),
        17..=32 if is_nvidia && warp_size == 32 => {
            ("gemm_nvfp4_gguf_mma_f16_bm32", 32, 64, Some(64))
        }
        _ if is_nvidia && warp_size == 32 => ("gemm_nvfp4_gguf_mma_f16_bm128", 128, 64, Some(256)),
        _ => {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_f16: backend bez NVIDIA MMA nie obsługuje T={n_tokens} > 16"
            )));
        }
    };
    let block_threads = block_threads.ok_or_else(|| {
        ForgeError::Kernel("gemm_nvfp4_gguf_f16: przepełnienie rozmiaru bloku".into())
    })?;
    if block_threads == 0 || block_threads > max_threads {
        return Err(ForgeError::Kernel(format!(
            "gemm_nvfp4_gguf_f16: blok {block_threads} przekracza limit urządzenia {max_threads}"
        )));
    }
    Ok(Nvfp4GgufDispatch {
        kernel,
        token_tile,
        row_tile,
        block_threads,
    })
}

fn raw_nvfp4_dp4a_supported(is_nvidia: bool, warp_size: u32) -> bool {
    is_nvidia && warp_size == 32
}

pub struct Kernels {
    device: Arc<dyn Device>,
    artifacts: KernelArtifacts,
    /// Codebook grid tables for the IQ formats, uploaded once at load
    /// (ggml iq2xs/iq2s/iq3s grids + ksigns; kernels take them as device
    /// pointers — the constant-table trick llama.cpp's CUDA kernels use).
    iq_tables: IqTables,
    /// Grow-only q8_1 scratch for the i8mma prefill GEMM: the activation tile is
    /// quantized ONCE (`quantize_act_q8_1`) into `xq` (int8 [T,K]) + `xd`/`xsm`
    /// (f32 [T,K/32]) here, then every weight-row block reads int8 X directly
    /// instead of re-quantizing f16 X per block. Sized to the largest (T*K) seen.
    prequant: Mutex<PrequantScratch>,
    /// Grow-only per-token int8 activation scratch for the W4A8 GEMM: `x` is
    /// quantized ONCE into `a_i8` (int8 [T,K]) + `ascales` (f16 [T]) by
    /// `w4a8_quant_act`, then `w4a8_gemm` reads them directly. Non-default path
    /// (FORGE_GEMM=w4a8); separate from the q8_1 `prequant` (different layout).
    w4a8_act: Mutex<W4A8ActScratch>,
    /// Grow-only per-token e4m3 activation scratch for the fp8 prefill GEMM
    /// (FORGE_GEMM=fp8).
    fp8_act: Mutex<Fp8ActScratch>,
    /// Grow-only scratch for the native-GGUF-layout Mojo int8 Q4_K prefill GEMM
    /// (`gemm_q4k_i8_native`): the MPAD-padded f16 activation, its int8 q8_1 codes
    /// and block-major da/sa scales. Separate layout from `prequant` (padded to
    /// the compile-time token ceiling MPAD, not the real token count).
    q4k_native: Mutex<Q4kNativeScratch>,
    /// Backend attention dla prefill F16 hd64/hd128. Domyślnie wybiera kernel
    /// Mojo skompilowany obecnie do PTX; obsługa AMDGPU i Metal wymaga osobnych
    /// backendów HAL i artefaktów. `FORGE_ATTN=fa` wybiera cubin tylko wtedy,
    /// gdy jest dostępny dla bieżącej architektury NVIDIA.
    attn: AttnBackend,
}

/// Dense prefill attention routing (FORGE_ATTN).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttnBackend {
    /// Scalar/SIMD online-softmax Mojo kernel (`attn_prefill`).
    Scalar,
    /// Tensor-core flash-attention CUDA cubin (`fattn_prefill.cu`).
    Cuda,
    /// Tensor-core flash-attention Mojo kernel (`attn_prefill_fa_mma`).
    Mojo,
}

/// Device-resident q8_1 activation scratch shared by the i8mma GEMM launches.
#[derive(Default)]
struct PrequantScratch {
    xq: Option<DevBuffer>,
    xd: Option<DevBuffer>,
    xsm: Option<DevBuffer>,
    /// Current int8-code capacity (elements) of `xq`.
    cap_codes: usize,
    /// Current f32 capacity (elements) of `xd`/`xsm`.
    cap_blocks: usize,
}

/// Device-resident per-token int8 activation scratch for the W4A8 GEMM.
#[derive(Default)]
struct W4A8ActScratch {
    a_i8: Option<DevBuffer>,
    ascales: Option<DevBuffer>,
    /// Current int8-code capacity (elements) of `a_i8`.
    cap_codes: usize,
    /// Current token capacity of `ascales`.
    cap_tokens: usize,
}

/// Device-resident per-token e4m3 activation scratch for the fp8 GEMM: `x` is
/// quantized ONCE into `xq` (e4m3 bytes [T,K]) + `xs` (f32 per-token scale [T])
/// by `quantize_act_fp8`, then `gemm_fp8` reads them directly. Non-default path
/// (FORGE_GEMM=fp8); separate layout from the q8_1 `prequant`.
#[derive(Default)]
struct Fp8ActScratch {
    xq: Option<DevBuffer>,
    xs: Option<DevBuffer>,
    /// Current e4m3-code capacity (elements) of `xq`.
    cap_codes: usize,
    /// Current token capacity of `xs`.
    cap_tokens: usize,
}

/// Device-resident scratch for the native-GGUF-layout int8 Q4_K prefill GEMM.
/// All buffers are sized to the padded token ceiling MPAD (grow-only).
#[derive(Default)]
struct Q4kNativeScratch {
    /// MPAD-padded f16 activation [MPAD, cols] (the real rows in the head, the
    /// tail allocated but never stored back).
    xpad: Option<DevBuffer>,
    /// int8 q8_1 codes [MPAD, cols].
    xq: Option<DevBuffer>,
    /// Block-major per-32 activation scale d [cols/32, MPAD].
    da: Option<DevBuffer>,
    /// Block-major per-32 activation sum d·Σcodes [cols/32, MPAD].
    sa: Option<DevBuffer>,
    /// Current f16/int8 element capacity of `xpad`/`xq` (MPAD·cols).
    cap_x: usize,
    /// Current f32 element capacity of `da`/`sa` ((cols/32)·MPAD).
    cap_blocks: usize,
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
            let buf = device.alloc(
                bytes.len(),
                forge_types::MemKind::Device,
                forge_hal::Pool::Weights,
            )?;
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
    /// Wyznacza długość zaakceptowanego draftu i token korekty na GPU.
    pub fn mtp_verify_decide(
        &self,
        decision: &DevBuffer,
        predictions: &DevBuffer,
        input_ids: &DevBuffer,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_verify_decide")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(decision)
            .buf(predictions)
            .buf(input_ids)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F16 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f16(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f16")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje wiersz F32 wskazany pierwszą wartością bufora decyzji.
    pub fn mtp_select_row_f32(
        &self,
        output: &DevBuffer,
        rows: &DevBuffer,
        decision: &DevBuffer,
        row_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("mtp_select_row_f32")?;
        let config = LaunchConfig {
            grid: ((row_size as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(rows)
            .buf(decision)
            .scalar(row_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    pub fn supports_fp8_modular_shape(&self, rows: usize, cols: usize) -> bool {
        self.artifacts.has(&format!("gemm_fp8_mod_{rows}_{cols}"))
    }

    pub fn supports_fp8_hybrid_packers(&self) -> bool {
        self.artifacts.has("pack_nvfp4_fp8") && self.artifacts.has("pack_f16_fp8")
    }

    pub fn supports_fp8_logits(&self) -> bool {
        self.artifacts.has("gemv_fp8_out_f32_v2")
    }

    pub fn supports_attn_decode_gqa4_f16_hd128(&self) -> bool {
        self.artifacts.has("attn_decode_split_gqa4_f16_hd128")
            && self.artifacts.has("attn_decode_combine_gqa2_f16_hd128")
    }

    pub fn load(device: Arc<dyn Device>) -> Result<Self> {
        let artifacts = KernelArtifacts::load(device.as_ref())?;
        let iq_tables = IqTables::upload(device.as_ref())?;
        let cuda_attn_available =
            artifacts.has("attn_prefill_fa_f16_hd64") && artifacts.has("attn_prefill_fa_f16_hd128");
        Ok(Self {
            device,
            artifacts,
            iq_tables,
            prequant: Mutex::new(PrequantScratch::default()),
            w4a8_act: Mutex::new(W4A8ActScratch::default()),
            fp8_act: Mutex::new(Fp8ActScratch::default()),
            q4k_native: Mutex::new(Q4kNativeScratch::default()),
            attn: match std::env::var("FORGE_ATTN").ok().as_deref() {
                Some("scalar") => AttnBackend::Scalar,
                Some("fa") | Some("cuda") if cuda_attn_available => AttnBackend::Cuda,
                _ => AttnBackend::Mojo,
            },
        })
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
        let args = LaunchArgs::new().buf(out).buf(a).buf(gate).scalar(n as i64);
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

    /// Jeden krok splotu dla wiersza macierzy batcha wskazanego offsetami.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16_at(
        &self,
        out: &DevBuffer,
        out_byte_off: usize,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        x_byte_off: usize,
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
            .buf_at(out, out_byte_off)?
            .buf(win_io)
            .buf_at(x_new, x_byte_off)?
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

    /// Scala przygotowanie krótkiego przebiegu DeltaNet dla 2-4 tokenów.
    /// Stan okna wejściowego pozostaje niezmieniony, a checkpointy są token-major.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_checkpoints: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let kernel_name = match n_steps {
            2 => "deltanet_prepare_t2_f16",
            3 => "deltanet_prepare_t3_f16",
            4 => "deltanet_prepare_t4_f16",
            _ => {
                return Err(ForgeError::Kernel(format!(
                    "deltanet_prepare wymaga T równego 2, 3 lub 4, otrzymano {n_steps}"
                )))
            }
        };
        let caps = self.device.caps();
        if n_k_heads == 0
            || n_v_heads == 0
            || !n_v_heads.is_multiple_of(n_k_heads)
            || d_state == 0
            || d_state.max(32) > caps.max_threads_per_block as usize
            || d_conv < 2
            || !eps.is_finite()
            || eps < 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "deltanet_prepare: niepoprawny kształt n_k={n_k_heads}, n_v={n_v_heads}, d_state={d_state}, d_conv={d_conv}, eps={eps}"
            )));
        }
        let key_heads = n_k_heads.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_heads = key_heads.checked_add(n_v_heads).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_dim = conv_heads
            .checked_mul(d_state)
            .ok_or_else(|| ForgeError::Kernel("deltanet_prepare: przepełnienie conv_dim".into()))?;
        let window = d_conv - 1;
        let vector_bytes = checked_buffer_bytes(
            "deltanet_prepare QKV output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let gate_f32_bytes =
            checked_buffer_bytes("deltanet_prepare gates output", &[n_steps, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("deltanet_prepare gates input", &[n_steps, n_v_heads], 2)?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_prepare conv checkpoints",
            &[n_steps, conv_dim, window],
            2,
        )?;
        let initial_bytes =
            checked_buffer_bytes("deltanet_prepare conv initial", &[conv_dim, window], 2)?;
        let mixed_bytes =
            checked_buffer_bytes("deltanet_prepare qkv mixed", &[n_steps, conv_dim], 2)?;
        let weight_bytes =
            checked_buffer_bytes("deltanet_prepare conv weight", &[conv_dim, d_conv], 2)?;
        let parameter_bytes = checked_buffer_bytes("deltanet_prepare parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_checkpoints.len() < checkpoint_bytes
            || conv_initial.len() < initial_bytes
            || qkv_mixed.len() < mixed_bytes
            || conv_weight.len() < weight_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_prepare: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x = u32::try_from(n_k_heads + n_v_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: liczba głów przekracza u32".into())
        })?;
        let block_x = u32::try_from(d_state.max(32)).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: rozmiar bloku przekracza u32".into())
        })?;
        let n_k_heads = i64::try_from(n_k_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_k_heads przekracza i64".into()))?;
        let n_v_heads = i64::try_from(n_v_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_v_heads przekracza i64".into()))?;
        let d_state = i64::try_from(d_state)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_state przekracza i64".into()))?;
        let d_conv = i64::try_from(d_conv)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_conv przekracza i64".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_checkpoints)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_k_heads)
            .scalar(n_v_heads)
            .scalar(d_state)
            .scalar(d_conv)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przyczynowy skan 2-4 kroków Gated-DeltaNet bez modyfikowania stanu
    /// wejściowego. Checkpointy mają układ [T, n_v_heads, d_state, d_state].
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_scan_f16_at(
            out,
            checkpoints,
            0,
            state_in,
            q,
            k,
            v,
            g,
            beta,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Przyczynowy skan Gated-DeltaNet zapisujący checkpointy od podanego
    /// przesunięcia bajtowego w większym buforze współdzielonym przez warstwy.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16_at(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        let tiled = d_state <= 128
            && matches!(n_steps, 3 | 4)
            && caps.warp_size > 0
            && caps.warp_size <= caps.max_threads_per_block
            && caps.warp_size <= 128;
        let kernel_name = match (n_steps, tiled) {
            (2, _) => "deltanet_gated_scan_t2_f16",
            (3, true) => "deltanet_gated_scan_t3_d128_f16",
            (4, true) => "deltanet_gated_scan_t4_d128_f16",
            (3, false) => "deltanet_gated_scan_t3_f16",
            (4, false) => "deltanet_gated_scan_t4_f16",
            _ => {
                return Err(ForgeError::Kernel(format!(
                    "deltanet_gated_scan wymaga T równego 2, 3 lub 4, otrzymano {n_steps}"
                )))
            }
        };
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_gated_scan wymaga n_v_heads > 0 i 1 <= d_state <= 1024, otrzymano n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let output_bytes = checked_buffer_bytes(
            "deltanet_gated_scan output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "deltanet_gated_scan state",
            &[n_v_heads, d_state, d_state],
            4,
        )?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_gated_scan checkpoints",
            &[n_steps, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes =
            checked_buffer_bytes("deltanet_gated_scan gates", &[n_steps, n_v_heads], 4)?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_gated_scan: przepełnienie offsetu checkpointów".into())
            })?;
        if out.len() < output_bytes
            || checkpoints.len() < checkpoint_end
            || state_in.len() < state_bytes
            || q.len() < output_bytes
            || k.len() < output_bytes
            || v.len() < output_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_gated_scan: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let block_x = if tiled {
            caps.warp_size
        } else {
            u32::try_from(d_state).map_err(|_| {
                ForgeError::Kernel("deltanet_gated_scan: d_state przekracza u32".into())
            })?
        };
        let head_tiles = if tiled {
            d_state.div_ceil(block_x as usize)
        } else {
            1
        };
        let grid_heads = n_v_heads.checked_mul(head_tiles).ok_or_else(|| {
            ForgeError::Kernel("deltanet_gated_scan: przepełnienie liczby kafli".into())
        })?;
        let grid_x = u32::try_from(grid_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_gated_scan: liczba głów przekracza u32".into())
        })?;
        let k_art = self.artifacts.get(kernel_name)?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Zatwierdza na GPU checkpoint wskazany przez urządzeniowy licznik i32.
    /// Wartość 0 pozostawia stan bez zmian, a wartości spoza [0, T] są ignorowane.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_commit_checkpoint_f32_at(
            state_out,
            checkpoints,
            0,
            accepted_index,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Zatwierdza checkpoint z fragmentu większego bufora zaczynającego się
    /// pod podanym przesunięciem bajtowym.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32_at(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !matches!(n_steps, 2..=4) {
            return Err(ForgeError::Kernel(format!(
                "deltanet_commit_checkpoint wymaga T równego 2, 3 lub 4, otrzymano {n_steps}"
            )));
        }
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_commit_checkpoint: niepoprawny kształt n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let state_elements = n_v_heads
            .checked_mul(d_state)
            .and_then(|elements| elements.checked_mul(d_state))
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie stanu".into())
            })?;
        let state_bytes = state_elements.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie bajtów stanu".into())
        })?;
        let checkpoint_bytes = state_bytes.checked_mul(n_steps).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie checkpointów".into())
        })?;
        let state_elements_i64 = i64::try_from(state_elements).map_err(|_| {
            ForgeError::Kernel("deltanet_commit_checkpoint: liczba elementów przekracza i64".into())
        })?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel(
                    "deltanet_commit_checkpoint: przepełnienie offsetu checkpointów".into(),
                )
            })?;
        if state_out.len() < state_bytes
            || checkpoints.len() < checkpoint_end
            || accepted_index.len() < std::mem::size_of::<i32>()
        {
            return Err(ForgeError::Kernel(
                "deltanet_commit_checkpoint: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x =
            u32::try_from(state_elements.div_ceil(BLOCK as usize).min(65_535)).map_err(|_| {
                ForgeError::Kernel("deltanet_commit_checkpoint: siatka przekracza u32".into())
            })?;
        let k_art = self.artifacts.get("deltanet_commit_checkpoint_f32")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(state_out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(accepted_index)
            .scalar(state_elements_i64)
            .scalar(n_steps as i64);
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

    /// Wariant batchowy pojedynczego wiersza z przesunięciem buforów wejścia
    /// i wyjścia; wektory parametrów warstwy zawsze zaczynają się od zera.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_log_decay_f32_at(
        &self,
        g_out: &DevBuffer,
        g_byte_off: usize,
        alpha_in: &DevBuffer,
        alpha_byte_off: usize,
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
            .buf_at(g_out, g_byte_off)?
            .buf_at(alpha_in, alpha_byte_off)?
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
        let args = LaunchArgs::new().buf(y).buf(w).buf(x).scalar(cols as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Mnoży y = W·x dla wag NVFP4 w układzie packed compressed-tensors.
    /// `inv_global_scale` jest odwrotnością `weight_global_scale`.
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

    /// Mnożenie macierz-wektor bezpośrednio z bloków GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_f16 wymaga rows > 0 i cols % 64 == 0, otrzymano rows={rows}, cols={cols}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[rows], 2)?;
        let weight_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_f16", &[cols], 2)?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("gemv_nvfp4_gguf_f16: siatka przekracza u32".into()))?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("gemv_nvfp4_gguf_f16")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols as i64)
            .scalar(output_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wykonuje pojedynczą projekcję F16 tą samą matematyką co NVIDIA B3/B4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_nvfp4_gguf_b1_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if !matches!(caps.vendor, forge_types::Vendor::Nvidia) || caps.warp_size != 32 {
            return Err(ForgeError::Unsupported(
                "gemv_nvfp4_gguf_b1_f16 wymaga NVIDIA z warpem 32".into(),
            ));
        }
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_b1_f16 wymaga rows > 0, cols % 64 == 0 i skończonej skali; rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let output_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 output", &[rows], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_b1_f16 input", &[cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_b1_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("gemm_nvfp4_gguf_f16_b1_nvidia")?;
        let grid_x = u32::try_from(rows.div_ceil(2)).map_err(|_| {
            ForgeError::Kernel("gemv_nvfp4_gguf_b1_f16: siatka przekracza u32".into())
        })?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(1i64)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kwantyzuje aktywację raz do Q8_1 i wykonuje grupę projekcji GGUF NVFP4
    /// przez dp4a. Q/K/V oraz gate/up mogą współdzielić ten sam prepass.
    pub fn gemv_nvfp4_gguf_q8_1_group_f16(
        &self,
        projections: &[Nvfp4GgufQ8Projection<'_>],
        x: &DevBuffer,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if projections.is_empty() || cols < 64 || !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemv_nvfp4_gguf_q8_1 wymaga projekcji i cols % 64 == 0, otrzymano projekcji={}, cols={cols}",
                projections.len()
            )));
        }
        let input_bytes = checked_buffer_bytes("gemv_nvfp4_gguf_q8_1 input", &[cols], 2)?;
        if x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemv_nvfp4_gguf_q8_1: bufor wejścia jest za mały".into(),
            ));
        }
        for projection in projections {
            if projection.rows == 0 || !projection.output_scale.is_finite() {
                return Err(ForgeError::Kernel(
                    "gemv_nvfp4_gguf_q8_1 wymaga rows > 0 i skończonej skali".into(),
                ));
            }
            let output_bytes =
                checked_buffer_bytes("gemv_nvfp4_gguf_q8_1 output", &[projection.rows], 2)?;
            let weight_bytes = checked_buffer_bytes(
                "gemv_nvfp4_gguf_q8_1 weights",
                &[projection.rows, cols / 64],
                36,
            )?;
            if projection.output.len() < output_bytes || projection.weights.len() < weight_bytes {
                return Err(ForgeError::Kernel(
                    "gemv_nvfp4_gguf_q8_1: bufor projekcji jest za mały".into(),
                ));
            }
        }
        let caps = self.device.caps();
        if !raw_nvfp4_dp4a_supported(
            matches!(caps.vendor, forge_types::Vendor::Nvidia),
            caps.warp_size,
        ) {
            for projection in projections {
                self.gemv_nvfp4_gguf_f16(
                    projection.output,
                    projection.weights,
                    x,
                    projection.rows,
                    cols,
                    projection.output_scale,
                    stream,
                )?;
            }
            return Ok(());
        }
        let need_codes = cols;
        let need_blocks = cols / 32;
        let mut scratch = self.prequant.lock().expect("prequant scratch poisoned");
        if scratch.cap_codes < need_codes {
            scratch.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            scratch.cap_codes = need_codes;
        }
        if scratch.cap_blocks < need_blocks {
            scratch.xd = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.xsm = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            scratch.cap_blocks = need_blocks;
        }
        let xq = scratch.xq.as_ref().expect("xq zaalokowane");
        let xd = scratch.xd.as_ref().expect("xd zaalokowane");
        let xsm = scratch.xsm.as_ref().expect("xsm zaalokowane");
        let quant = self.artifacts.get("quantize_act_q8_1")?;
        let quant_cfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let quant_args = LaunchArgs::new()
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .buf(x)
            .scalar(cols as i64)
            .scalar(1i64);
        self.device.launch(quant, &quant_cfg, &quant_args, stream)?;

        let kernel = self.artifacts.get("gemv_nvfp4_gguf_q8_1_f16")?;
        for projection in projections {
            let grid_x = u32::try_from(projection.rows.div_ceil(8)).map_err(|_| {
                ForgeError::Kernel("gemv_nvfp4_gguf_q8_1: siatka przekracza u32".into())
            })?;
            let config = LaunchConfig {
                grid: (grid_x, 1, 1),
                block: (BLOCK, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(projection.output)
                .buf(projection.weights)
                .buf(xq)
                .buf(xd)
                .scalar(cols as i64)
                .scalar(projection.rows as i64)
                .scalar(projection.output_scale);
            self.device.launch(kernel, &config, &args, stream)?;
        }
        Ok(())
    }

    /// Kafelkowane mnożenie wielu tokenów bezpośrednio z bloków GGUF NVFP4.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_nvfp4_gguf_f16(
        &self,
        y: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 64 || !cols.is_multiple_of(64) || !output_scale.is_finite() {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4_gguf_f16 wymaga rows > 0, cols % 64 == 0 i skończonej skali; otrzymano rows={rows}, cols={cols}, scale={output_scale}"
            )));
        }
        let caps = self.device.caps();
        let dispatch = nvfp4_gguf_dispatch(
            n_tokens,
            matches!(caps.vendor, forge_types::Vendor::Nvidia),
            caps.warp_size,
            caps.max_threads_per_block,
        )?;
        let output_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_f16 output", &[n_tokens, rows], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gemm_nvfp4_gguf_f16 weights", &[rows, cols / 64], 36)?;
        let input_bytes = checked_buffer_bytes("gemm_nvfp4_gguf_f16 input", &[n_tokens, cols], 2)?;
        if y.len() < output_bytes || weights.len() < weight_bytes || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_nvfp4_gguf_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(rows.div_ceil(dispatch.row_tile))
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: grid.x przekracza u32".into()))?;
        let grid_y = u32::try_from(n_tokens.div_ceil(dispatch.token_tile))
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: grid.y przekracza u32".into()))?;
        let rows = i64::try_from(rows)
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: rows przekracza i64".into()))?;
        let cols = i64::try_from(cols)
            .map_err(|_| ForgeError::Kernel("gemm_nvfp4_gguf_f16: cols przekracza i64".into()))?;
        let n_tokens = i64::try_from(n_tokens).map_err(|_| {
            ForgeError::Kernel("gemm_nvfp4_gguf_f16: liczba tokenów przekracza i64".into())
        })?;
        let kernel = self.artifacts.get(dispatch.kernel)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (dispatch.block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(weights)
            .buf(x)
            .scalar(cols)
            .scalar(rows)
            .scalar(n_tokens)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Scalone przygotowanie wejścia MTP i projekcja Q8_0 z 2H do H.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_prepare_f16(
        &self,
        output: &DevBuffer,
        embedding_row: &DevBuffer,
        target_hidden: &DevBuffer,
        enorm: &DevBuffer,
        hnorm: &DevBuffer,
        eh_proj: &DevBuffer,
        hidden_size: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if hidden_size == 0
            || hidden_size > 5120
            || !(2 * hidden_size).is_multiple_of(32)
            || !eps.is_finite()
            || eps <= 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "mtp_prepare_f16 wymaga 0 < H <= 5120, 2H % 32 == 0 i eps > 0; otrzymano H={hidden_size}, eps={eps}"
            )));
        }
        let output_bytes = checked_buffer_bytes("mtp_prepare_f16 output", &[hidden_size], 2)?;
        let vector_bytes = checked_buffer_bytes("mtp_prepare_f16 vector", &[hidden_size], 2)?;
        let projection_bytes = checked_buffer_bytes(
            "mtp_prepare_f16 eh_proj",
            &[hidden_size, (2 * hidden_size) / 32],
            34,
        )?;
        if output.len() < output_bytes
            || embedding_row.len() < vector_bytes
            || target_hidden.len() < vector_bytes
            || enorm.len() < vector_bytes
            || hnorm.len() < vector_bytes
            || eh_proj.len() < projection_bytes
        {
            return Err(ForgeError::Kernel(
                "mtp_prepare_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(hidden_size.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("mtp_prepare_f16: siatka przekracza u32".into()))?;
        let kernel = self.artifacts.get("mtp_prepare_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(embedding_row)
            .buf(target_hidden)
            .buf(enorm)
            .buf(hnorm)
            .buf(eh_proj)
            .scalar(hidden_size as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Ustawia metadane kroku MTP i opcjonalnie mapowanie nowej strony KV.
    #[allow(clippy::too_many_arguments)]
    pub fn mtp_stage_step(
        &self,
        position_out: &DevBuffer,
        seq_len_out: &DevBuffer,
        page_table: &DevBuffer,
        position: usize,
        seq_len: usize,
        logical_page: Option<usize>,
        physical_page: Option<i32>,
        stream: &Stream,
    ) -> Result<()> {
        if position_out.len() < 4 || seq_len_out.len() < 4 {
            return Err(ForgeError::Kernel(
                "mtp_stage_step wymaga 4-bajtowych buforów metadanych".into(),
            ));
        }
        let (logical_page, physical_page) = match (logical_page, physical_page) {
            (Some(logical), Some(physical)) if physical >= 0 => {
                let byte_end = logical
                    .checked_add(1)
                    .and_then(|entries| entries.checked_mul(4))
                    .ok_or_else(|| ForgeError::Kernel("mtp_stage_step: przepełnienie indeksu strony".into()))?;
                if byte_end > page_table.len() {
                    return Err(ForgeError::Kernel(format!(
                        "mtp_stage_step: strona logiczna {logical} wykracza poza page table"
                    )));
                }
                (i64::try_from(logical).map_err(|_| ForgeError::Kernel("mtp_stage_step: indeks strony przekracza i64".into()))?, i64::from(physical))
            }
            (None, None) => (-1, -1),
            _ => {
                return Err(ForgeError::Kernel(
                    "mtp_stage_step wymaga kompletnej pary stron logiczna/fizyczna".into(),
                ));
            }
        };
        let position = i64::try_from(position)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: pozycja przekracza i64".into()))?;
        let seq_len = i64::try_from(seq_len)
            .map_err(|_| ForgeError::Kernel("mtp_stage_step: długość przekracza i64".into()))?;
        let kernel = self.artifacts.get("mtp_stage_step")?;
        let config = LaunchConfig {
            grid: (1, 1, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(position_out)
            .buf(seq_len_out)
            .buf(page_table)
            .scalar(position)
            .scalar(seq_len)
            .scalar(logical_page)
            .scalar(physical_page);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kopiuje staged embedding row z dedykowanej tabeli F16 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_f16_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 {
            return Err(ForgeError::Kernel(
                "gather_f16_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("gather_f16_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes =
            checked_buffer_bytes("gather_f16_row_f16 weights", &[vocab_size, hidden_size], 2)?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_f16_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_f16_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje staged embedding row z tied Q8_0 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_q8_0_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(32) {
            return Err(ForgeError::Kernel(
                "gather_q8_0_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes = checked_buffer_bytes("gather_q8_0_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_q8_0_row_f16 weights",
            &[vocab_size, hidden_size / 32],
            34,
        )?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_q8_0_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_q8_0_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Dekwantyzuje staged embedding row z tied GGUF NVFP4 według ID na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn gather_nvfp4_gguf_row_f16(
        &self,
        output: &DevBuffer,
        weights: &DevBuffer,
        token: &DevBuffer,
        status: &DevBuffer,
        status_offset: usize,
        vocab_size: usize,
        hidden_size: usize,
        output_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if vocab_size == 0 || hidden_size == 0 || !hidden_size.is_multiple_of(64) {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_row_f16: niepoprawny kształt".into(),
            ));
        }
        let output_bytes =
            checked_buffer_bytes("gather_nvfp4_gguf_row_f16 output", &[hidden_size], 2)?;
        let weight_bytes = checked_buffer_bytes(
            "gather_nvfp4_gguf_row_f16 weights",
            &[vocab_size, hidden_size / 64],
            36,
        )?;
        if output.len() < output_bytes
            || weights.len() < weight_bytes
            || token.len() < 4
            || status_offset
                .checked_add(4)
                .is_none_or(|end| end > status.len())
        {
            return Err(ForgeError::Kernel(
                "gather_nvfp4_gguf_row_f16: zbyt mały bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("gather_nvfp4_gguf_row_f16")?;
        let config = LaunchConfig::linear(hidden_size as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(output)
            .buf(weights)
            .buf(token)
            .buf_at(status, status_offset)?
            .scalar(vocab_size as i64)
            .scalar(hidden_size as i64)
            .scalar(output_scale);
        self.device.launch(kernel, &config, &args, stream)
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

    /// Logity FP32 z wag E4M3 oraz jednej skali FP32 na wiersz.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_fp8_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_fp8_out_f32 wymaga cols % 256 == 0, otrzymano {cols}"
            )));
        }
        let output_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wyjścia".into())
        })?;
        let weight_bytes = rows.checked_mul(cols).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wag".into())
        })?;
        let input_bytes = cols.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wejścia".into())
        })?;
        let grid_x = u32::try_from(rows.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("gemv_fp8_out_f32: siatka przekracza u32".into()))?;
        if y_f32.len() < output_bytes
            || w.len() < weight_bytes
            || scales.len() < output_bytes
            || x.len() < input_bytes
        {
            return Err(ForgeError::Kernel(
                "gemv_fp8_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("gemv_fp8_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(scales)
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

    /// Kafel dla małego batcha NVFP4. BM32 zachowuje ten sam łańcuch MMA,
    /// ale nie wykonuje pustej drugiej połowy kafla BM64.
    fn gemm_nvfp4_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32) {
        if (2..=32).contains(&n_tokens) {
            ("_bm32", 64, 32)
        } else {
            Self::gemm_tile(rows, n_tokens)
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
        if rows == 0 || cols < 16 || !cols.is_multiple_of(16) || n_tokens == 0 {
            return Err(ForgeError::Kernel(format!(
                "gemm_nvfp4 requires rows > 0, cols >= 16, cols % 16 == 0 and n_tokens > 0, got rows={rows}, cols={cols}, n_tokens={n_tokens}"
            )));
        }
        let (kernel_name, block, bm) = if (2..=4).contains(&n_tokens)
            && self.artifacts.has("gemv_batch_nvfp4_f16_b4")
        {
            ("gemv_batch_nvfp4_f16_b4".to_string(), 256, n_tokens as u32)
        } else if (5..=8).contains(&n_tokens) && self.artifacts.has("gemv_batch_nvfp4_f16_b8") {
            ("gemv_batch_nvfp4_f16_b8".to_string(), 256, n_tokens as u32)
        } else if (9..=16).contains(&n_tokens) && self.artifacts.has("gemv_batch_nvfp4_f16_b16") {
            ("gemv_batch_nvfp4_f16_b16".to_string(), 256, n_tokens as u32)
        } else {
            let (mut suffix, mut block, mut bm) = Self::gemm_nvfp4_tile(rows, n_tokens);
            if !self.artifacts.has(&format!("gemm_nvfp4_f16{suffix}")) {
                (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
            }
            (format!("gemm_nvfp4_f16{suffix}"), block, bm)
        };
        let k = self.artifacts.get(&kernel_name)?;
        let cfg = LaunchConfig {
            grid: if kernel_name.starts_with("gemv_batch_") {
                ((rows as u32).div_ceil(8), 1, 1)
            } else {
                (
                    (rows as u32).div_ceil(64),
                    (n_tokens as u32).div_ceil(bm),
                    1,
                )
            },
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
        if rows == 0 || cols < 8 || !cols.is_multiple_of(8) || n_tokens == 0 {
            return Err(ForgeError::Kernel(format!(
                "gemm_f16_out_f32 requires rows > 0, cols >= 8, cols % 8 == 0 and n_tokens > 0, got rows={rows}, cols={cols}, n_tokens={n_tokens}"
            )));
        }
        let (kernel_name, block, bm) = if (2..=4).contains(&n_tokens)
            && self.artifacts.has("gemv_batch_f16_out_f32_b4")
        {
            (
                "gemv_batch_f16_out_f32_b4".to_string(),
                256,
                n_tokens as u32,
            )
        } else if (5..=8).contains(&n_tokens) && self.artifacts.has("gemv_batch_f16_out_f32_b8") {
            (
                "gemv_batch_f16_out_f32_b8".to_string(),
                256,
                n_tokens as u32,
            )
        } else if n_tokens <= 32 && self.artifacts.has("gemm_f16_out_f32_bm32") {
            ("gemm_f16_out_f32_bm32".to_string(), 64, 32)
        } else {
            let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
            (format!("gemm_f16_out_f32{suffix}"), block, bm)
        };
        let k = self.artifacts.get(&kernel_name)?;
        let cfg = LaunchConfig {
            grid: if kernel_name.starts_with("gemv_batch_") {
                ((rows as u32).div_ceil(8), 1, 1)
            } else {
                (
                    (rows as u32).div_ceil(64),
                    (n_tokens as u32).div_ceil(bm),
                    1,
                )
            },
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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

    /// Zapisuje K/V, odczytując pozycję bazową z bufora urządzenia.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_device_pos_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_batch_device_pos_f16")?;
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
            .buf(base_pos)
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
        // Tensor-core flash-attention paths. Only the f16 cache with head_dim
        // 64/128 has an FA specialization; every other shape falls through to
        // the Mojo scalar kernel so nothing breaks.
        if kv_dtype == DType::F16 && (head_dim == 64 || head_dim == 128) {
            match self.attn {
                AttnBackend::Cuda => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, false,
                    );
                }
                AttnBackend::Mojo => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, true,
                    );
                }
                AttnBackend::Scalar => {}
            }
        }
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

    /// Wykonuje prefill HD256 z pozycją bazową przechowywaną na urządzeniu.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_device_pos_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("attn_prefill_device_pos_f16_hd256")?;
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
            .buf(base_pos)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Tensor-core causal flash-attention prefill. Same I/O contract as
    /// `attn_prefill` (f16 cache, paged KV, GQA, causal) but QK^T and P·V run as
    /// f16 mma with an online softmax kept in registers. Grid: (ceil(T/64),
    /// n_q_heads); one block of 4 warps owns 64 query rows of one head. `mojo`
    /// selects the portable Mojo kernel (`attn_prefill_fa_mma`,
    /// kernels/mojo/src/prefill.mojo) over the CUDA cubin
    /// (kernels/cuda/fattn_prefill.cu) — byte-identical tiling contract.
    #[allow(clippy::too_many_arguments)]
    fn attn_prefill_fa(
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
        mojo: bool,
    ) -> Result<()> {
        let name = match (head_dim, mojo) {
            (64, false) => "attn_prefill_fa_f16_hd64",
            (128, false) => "attn_prefill_fa_f16_hd128",
            (64, true) => "attn_prefill_fa_mojo_f16_hd64",
            (128, true) => "attn_prefill_fa_mojo_f16_hd128",
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill_fa: head_dim {other} has no FA specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        // Kernel tiling contract (fattn_prefill.cu): BQ=64 queries per block,
        // 4 warps = 128 threads.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(64), n_q_heads as u32, 1),
            block: (128, 1, 1),
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

    /// int8 TENSOR-CORE MMQ prefill GEMM over Q8_0 weights.
    /// Y[t, row] = W·x[t]; `w_byte_off` addresses the window's first block.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_i8mma_at(
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
                "gemm_q8_0_i8mma requires cols % 32 == 0, got {cols}"
            )));
        }
        self.gemm_i8mma_run(
            "gemm_q8_0_i8mma",
            false,
            y,
            w_q8,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Krótki GEMM Q8_0 x Q8_1 zapisujący pełne logity F32 dla weryfikatora.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_i8mma_out_f32_at(
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
        if !cols.is_multiple_of(32) || !(3..=4).contains(&n_tokens) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_i8mma_out_f32 wymaga cols % 32 == 0 i T=3/4, otrzymano cols={cols}, T={n_tokens}"
            )));
        }
        self.gemm_i8mma_run(
            "gemm_q8_0_i8mma",
            true,
            y_f32,
            w_q8,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Dokładny krótki GEMM Q8_0 x F16 zapisujący logity F32 bez requantyzacji X.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q8_0_f16_exact_out_f32_at(
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
        if rows == 0 || !cols.is_multiple_of(32) || !matches!(n_tokens, 3 | 4) {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_f16_exact_out_f32 wymaga rows > 0, cols % 32 == 0 i T=3/4, otrzymano rows={rows}, cols={cols}, T={n_tokens}"
            )));
        }
        let output_bytes =
            checked_buffer_bytes("gemm_q8_0_f16_exact_out_f32 output", &[n_tokens, rows], 4)?;
        let weight_bytes = checked_buffer_bytes(
            "gemm_q8_0_f16_exact_out_f32 weights",
            &[rows, cols / 32],
            34,
        )?;
        let weight_end = w_byte_off.checked_add(weight_bytes).ok_or_else(|| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: przepełnienie zakresu wag".into())
        })?;
        let input_bytes =
            checked_buffer_bytes("gemm_q8_0_f16_exact_out_f32 input", &[n_tokens, cols], 2)?;
        if y_f32.len() < output_bytes || w_q8.len() < weight_end || x.len() < input_bytes {
            return Err(ForgeError::Kernel(
                "gemm_q8_0_f16_exact_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let caps = self.device.caps();
        let rows_per_block = 8u32;
        let block_threads = caps.warp_size.checked_mul(rows_per_block).ok_or_else(|| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: przepełnienie rozmiaru bloku".into())
        })?;
        if block_threads > caps.max_threads_per_block {
            return Err(ForgeError::Kernel(format!(
                "gemm_q8_0_f16_exact_out_f32: blok {block_threads} przekracza limit urządzenia {}",
                caps.max_threads_per_block
            )));
        }
        let grid_x = u32::try_from(rows.div_ceil(rows_per_block as usize)).map_err(|_| {
            ForgeError::Kernel("gemm_q8_0_f16_exact_out_f32: siatka przekracza u32".into())
        })?;
        let kernel_name = match n_tokens {
            3 => "gemm_q8_0_f16_exact_out_f32_b3",
            4 => "gemm_q8_0_f16_exact_out_f32_b4",
            _ => unreachable!(),
        };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf_at(w_q8, w_byte_off)?
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// int8 TENSOR-CORE MMQ prefill GEMM over Q4_K weights.
    /// Y[t, row] = W·x[t]; `w_byte_off` addresses the window's first superblock.
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_q4_k_i8mma_at(
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
                "gemm_q4_k_i8mma requires cols % 256 == 0, got {cols}"
            )));
        }
        // Universal DEFAULT (all arches): the native-GGUF-layout Mojo int8 Q4_K
        // multistage GEMM (reads the raw `DevWeight::Q4K.buf` bytes in-kernel, NO
        // repack; bit-exact vs Q4_K MMQ by construction). Prefill-sized batches
        // whose (rows,cols) has a committed (N,K,MPAD) instance and T ≤ 4096. A
        // shape/token count with no bucket (or decode-sized n_tokens < 64) falls
        // through to the portable hand int8-MMQ tiles.
        if n_tokens >= 64
            && self.gemm_q4k_i8_native(y, w_q4k, w_byte_off, x, rows, cols, n_tokens, stream)?
        {
            return Ok(());
        }
        self.gemm_i8mma_run(
            "gemm_q4_k_i8mma",
            false,
            y,
            w_q4k,
            w_byte_off,
            x,
            rows,
            cols,
            n_tokens,
            stream,
        )
    }

    /// Smallest committed MPAD bucket ≥ `n_tokens`, or `None` if `n_tokens`
    /// exceeds the largest committed ceiling (4096).
    fn q4k_native_mpad(n_tokens: usize) -> Option<usize> {
        [128usize, 256, 512, 1024, 2048, 4096]
            .into_iter()
            .find(|&m| m >= n_tokens)
    }

    /// Native-GGUF-layout Mojo int8 Q4_K multistage prefill GEMM (universal
    /// default). Zero-pads the f16 activation to the compile-time token ceiling
    /// MPAD (smallest bucket ≥ `n_tokens`), quantizes it to q8_1 over MPAD
    /// (block-major da/sa, stride MPAD), then runs the native GEMM reading the RAW
    /// `w_q4k` GGUF bytes at `w_byte_off` (144-byte block_q4_K de-interleaved
    /// in-kernel — TRUE 1× VRAM, no repacked weight/scale copy). The kernel guards
    /// stores by `m_real = n_tokens`, so the padded tail rows are computed but
    /// never written. Dynamic smem 53248 B (the >48 KB opt-in the HAL sets
    /// automatically). Returns `false` (caller falls back to the hand int8-MMQ
    /// tiles) when `(rows,cols)` has no committed instance or `n_tokens > 4096`.
    #[allow(clippy::too_many_arguments)]
    fn gemm_q4k_i8_native(
        &self,
        y: &DevBuffer,
        w_q4k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        let Some(mpad) = Self::q4k_native_mpad(n_tokens) else {
            return Ok(false);
        };
        let key = format!("gemm_q4k_i8_native_{rows}_{cols}_m{mpad}");
        let Ok(gk) = self.artifacts.get(&key) else {
            return Ok(false);
        };
        let qk = self.artifacts.get("quantize_act_q8_1")?;

        // Grow-only scratch: padded f16 activation [MPAD, cols], its int8 q8_1
        // codes [MPAD, cols] and block-major da/sa [cols/32, MPAD]. The padded
        // tail (rows n_tokens..MPAD) is allocated but never read for correctness
        // (its outputs are guarded off by m_real), so no zeroing is needed.
        let need_x = mpad * cols;
        let need_blocks = mpad * (cols / 32);
        let mut sc = self.q4k_native.lock().expect("q4k native scratch poisoned");
        if sc.cap_x < need_x {
            sc.xpad = Some(
                self.device
                    .alloc(need_x * 2, MemKind::Device, Pool::Activations)?,
            );
            sc.xq = Some(
                self.device
                    .alloc(need_x, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_x = need_x;
        }
        if sc.cap_blocks < need_blocks {
            sc.da = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.sa = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_blocks = need_blocks;
        }
        let xpad = sc.xpad.as_ref().expect("xpad allocated");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let da = sc.da.as_ref().expect("da allocated");
        let sa = sc.sa.as_ref().expect("sa allocated");

        // Copy the real activation [n_tokens, cols] f16 into the padded head.
        self.device
            .copy(x, 0, xpad, 0, n_tokens * cols * 2, stream)?;

        // q8_1 quant over the full MPAD ceiling → int8 codes + block-major da/sa
        // (stride MPAD, matching the native kernel's da[kb*MPAD + token] indexing).
        let qcfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(da)
            .buf(sa)
            .buf(xpad)
            .scalar(cols as i64)
            .scalar(mpad as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        // Native GEMM: grid (ceil(rows/128), MPAD/128); block 256; dynamic smem
        // 53248 B. Args mirror gemm_q4k_i8_native(y, a=xq, w, da, sa, m_real).
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(128), (mpad as u32) / 128, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 53248,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf_at(w_q4k, w_byte_off)?
            .buf(da)
            .buf(sa)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)?;
        Ok(true)
    }

    /// Native-GGUF-layout Mojo int8 Q6_K multistage prefill GEMM. Mirrors
    /// `gemm_q4k_i8_native`: shares the q8_1 activation quant + `q4k_native`
    /// scratch (identical int8 codes + block-major da), then runs the native GEMM
    /// reading the RAW `w_q6k` GGUF bytes (210-byte block_q6_K unpacked in-kernel,
    /// TRUE 1× VRAM). The kernel honors Q6_K's 16-element scale granularity with a
    /// double m16n8k32 mma per 32-region, so it is bit-exact vs Q6_K × q8_1. `sa`
    /// is passed for a shared signature but unused (Q6_K has no min term). Returns
    /// `false` (caller falls back to the f16 Q6_K kernel) when `(rows,cols)` has no
    /// committed instance or `n_tokens > 4096`.
    #[allow(clippy::too_many_arguments)]
    fn gemm_q6k_i8_native(
        &self,
        y: &DevBuffer,
        w_q6k: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        let Some(mpad) = Self::q4k_native_mpad(n_tokens) else {
            return Ok(false);
        };
        let key = format!("gemm_q6k_i8_native_{rows}_{cols}_m{mpad}");
        let Ok(gk) = self.artifacts.get(&key) else {
            return Ok(false);
        };
        let qk = self.artifacts.get("quantize_act_q8_1")?;

        let need_x = mpad * cols;
        let need_blocks = mpad * (cols / 32);
        let mut sc = self.q4k_native.lock().expect("q4k native scratch poisoned");
        if sc.cap_x < need_x {
            sc.xpad = Some(
                self.device
                    .alloc(need_x * 2, MemKind::Device, Pool::Activations)?,
            );
            sc.xq = Some(
                self.device
                    .alloc(need_x, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_x = need_x;
        }
        if sc.cap_blocks < need_blocks {
            sc.da = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.sa = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_blocks = need_blocks;
        }
        let xpad = sc.xpad.as_ref().expect("xpad allocated");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let da = sc.da.as_ref().expect("da allocated");
        let sa = sc.sa.as_ref().expect("sa allocated");

        self.device
            .copy(x, 0, xpad, 0, n_tokens * cols * 2, stream)?;

        let qcfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(da)
            .buf(sa)
            .buf(xpad)
            .scalar(cols as i64)
            .scalar(mpad as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(128), (mpad as u32) / 128, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 53248,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf_at(w_q6k, w_byte_off)?
            .buf(da)
            .buf(sa)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)?;
        Ok(true)
    }

    /// Pre-quantize the activation to q8_1 ONCE (`quantize_act_q8_1`) into the
    /// grow-only scratch, then run the int8-MMQ GEMM reading int8 X directly.
    /// This halves X read bandwidth and removes the redundant per-row-block
    /// requant the old in-kernel quant paid across the grid's `ceil(rows/64)`
    /// blocks. Both launches share one `stream`, so the GEMM sees the quantized
    /// X without an explicit sync.
    #[allow(clippy::too_many_arguments)]
    fn gemm_i8mma_run(
        &self,
        kernel_base: &str,
        output_f32: bool,
        y: &DevBuffer,
        w: &DevBuffer,
        w_byte_off: usize,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let qk = self.artifacts.get("quantize_act_q8_1")?;
        // Portable Mojo int8 tensor-core tiles (`.target sm_80`, JIT to any
        // sm_80+ part). This is the default Q4_K/Q6_K prefill GEMM on pre-Ada
        // GPUs and the Q8_0 prefill GEMM everywhere; on Ada the vendored MMQ
        // cubin intercepts Q4_K/Q6_K upstream (`gemm_q4_k_i8mma_at`).
        let need_codes = n_tokens * cols;
        let need_blocks = n_tokens * (cols / 32);

        let mut sc = self.prequant.lock().expect("prequant scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_blocks < need_blocks {
            sc.xd = Some(
                self.device
                    .alloc(need_blocks * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.xsm = Some(self.device.alloc(
                need_blocks * 4,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.cap_blocks = need_blocks;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xd = sc.xd.as_ref().expect("xd allocated");
        let xsm = sc.xsm.as_ref().expect("xsm allocated");

        let qcfg = LaunchConfig::linear(need_blocks as u32, BLOCK);
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        if kernel_base == "gemm_q8_0_i8mma" && (2..=4).contains(&n_tokens) {
            let caps = self.device.caps();
            let nvidia_dp4a = matches!(caps.vendor, forge_types::Vendor::Nvidia)
                && caps.warp_size == 32
                && matches!(n_tokens, 3 | 4);
            let rows_per_block = if nvidia_dp4a { 4 } else { 8 };
            let block_threads = caps.warp_size.checked_mul(rows_per_block).ok_or_else(|| {
                ForgeError::Kernel("gemm_q8_0 small: przepełnienie rozmiaru bloku".into())
            })?;
            if block_threads > caps.max_threads_per_block {
                return Err(ForgeError::Kernel(format!(
                    "gemm_q8_0 small: blok {block_threads} przekracza limit urządzenia {}",
                    caps.max_threads_per_block
                )));
            }
            let kernel_name = match (output_f32, n_tokens) {
                (false, 2) => "gemm_q8_0_i8mma_b2",
                (false, 3) if nvidia_dp4a => "gemm_q8_0_dp4a_b3_nvidia",
                (false, 4) if nvidia_dp4a => "gemm_q8_0_dp4a_b4_nvidia",
                (true, 3) if nvidia_dp4a => "gemm_q8_0_dp4a_out_f32_b3_nvidia",
                (true, 4) if nvidia_dp4a => "gemm_q8_0_dp4a_out_f32_b4_nvidia",
                (false, 3) => "gemm_q8_0_i8mma_b3",
                (false, 4) => "gemm_q8_0_i8mma_b4",
                (true, 3) => "gemm_q8_0_i8mma_out_f32_b3",
                (true, 4) => "gemm_q8_0_i8mma_out_f32_b4",
                _ => unreachable!(),
            };
            let kernel = self.artifacts.get(kernel_name)?;
            let cfg = LaunchConfig {
                grid: ((rows as u32).div_ceil(rows_per_block), 1, 1),
                block: (block_threads, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(y)
                .buf_at(w, w_byte_off)?
                .buf(xq)
                .buf(xd)
                .scalar(cols as i64)
                .scalar(rows as i64)
                .scalar(n_tokens as i64);
            return self.device.launch(kernel, &cfg, &args, stream);
        }

        let (suffix, bm, bn, threads) = Self::gemm_i8mma_tile(rows, n_tokens);
        let gk = self.artifacts.get(&format!("{kernel_base}{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(bn),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf_at(w, w_byte_off)?
            .buf(xq)
            .buf(xd)
            .buf(xsm)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Tile selection for the i8mma GEMM: `(suffix, BM, BN, block_threads)`.
    ///
    /// The `_big` variant (BM=128 x BN=128, 512-thread/16-warp block) doubles
    /// the rows-per-block so the activation X — re-read `ceil(rows/BN)` times —
    /// is fetched half as often, raising the mma:bytes-loaded ratio. It keeps
    /// the per-warp accumulator (and thus the 127-reg / 1-CTA-per-SM = 16-warp
    /// occupancy, matching the old 2x256-thread = 16-warp footprint) fixed by
    /// adding warps instead of n-tiles/warp. Bit-identical to the old BM=128
    /// kernel (integer mma is exact).
    ///
    /// The 512-thread block halves the block count of a given GEMM (BM=128 x
    /// BN=128 vs the 256-thread kernel's BM=128 x BN=64 at 2 CTAs/SM), so it
    /// only wins when the GEMM is big enough to keep the ~128 SMs busy at the
    /// coarser granularity. Two conditions must both hold:
    ///  * `n_tokens >= 1024` (a full `MAX_PREFILL_CHUNK`): at a 512-token chunk
    ///    the whole prefill is tiny and the coarse blocks underfill the SMs for
    ///    the small attention projections, regressing the Mistral 512 prefill
    ///    ~11%.
    ///  * `ceil(rows/128) * ceil(n_tokens/128) >= 256` (>= 2 full waves on the
    ///    128 SMs at 1 CTA/SM): small-model projections (Qwen3-0.6B rows<=3072)
    ///    make too few blocks and `_big` regresses that GEMM ~19%.
    ///
    /// Otherwise fall back to the committed 256-thread BM=128 (2 CTAs/SM) or
    /// BM=64 kernel. `_big` is bit-identical to BM=128 (integer mma), so this is
    /// a pure perf gate. Measured on the RTX 4090: Mistral-7B Q4_K 4096 prefill
    /// 2588 -> 2827 tok/s (+9%), 8192 2246 -> 2343 (+4%); Qwen3-0.6B and the 512
    /// prefill stay on the committed kernel (no regression).
    fn gemm_i8mma_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32, u32) {
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("_big", 128, 128, 512)
        } else if n_tokens >= 256 {
            ("", 128, 64, 256)
        } else {
            ("_bm64", 64, 64, 256)
        }
    }

    /// QServe W4A8 CTA config for `M` tokens and `K` cols, mirroring
    /// `gemm_forward_cuda`'s host dispatch. Returns
    /// `(registry_key, CTA_M, CTA_N, CTA_K, num_warps, dynamic_smem_bytes)`.
    fn w4a8_config(m: usize, k: usize) -> (&'static str, u32, u32, u32, u32, u32) {
        if m > 128 {
            ("w4a8_gemm_m128", 128, 64, 64, 4, 41472)
        } else if m == 128 {
            if k <= 4096 {
                ("w4a8_gemm_m64_ksm", 64, 64, 64, 4, 25088)
            } else {
                ("w4a8_gemm_m64_klg", 64, 64, 128, 8, 37248)
            }
        } else {
            ("w4a8_gemm_m32", 32, 64, 128, 4, 24960)
        }
    }

    /// W4A8 (int4-weight x int8-activation) prefill GEMM: `y[t,row] = W·x[t]`.
    /// Non-default (routed only under `FORGE_GEMM=w4a8`). Consumes activations
    /// ALREADY quantized to per-token int8 (`a_i8` + `ascales`); the weight
    /// buffers are QServe-packed (`forge_formats::w4a8`). `rows` (N) must be a
    /// multiple of 64 and `cols` (K) a multiple of 128 (the kernel's group).
    #[allow(clippy::too_many_arguments)]
    pub fn w4a8_gemm(
        &self,
        y: &DevBuffer,
        a_i8: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        ascales: &DevBuffer,
        n_tokens: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !rows.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires rows % 64 == 0, got {rows}"
            )));
        }
        if !cols.is_multiple_of(128) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires cols % 128 == 0, got {cols}"
            )));
        }
        let (key, cta_m, cta_n, cta_k, warps, smem) = Self::w4a8_config(n_tokens, cols);
        if !cols.is_multiple_of(cta_k as usize) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 config {key} needs cols % {cta_k} == 0, got {cols}"
            )));
        }
        let gk = self.artifacts.get(key)?;
        let num_blocks_n = (rows as u32) / cta_n;
        let num_blocks_m = (n_tokens as u32).div_ceil(cta_m);
        let log_tile = if num_blocks_m >= 6 {
            3
        } else if num_blocks_m >= 3 {
            2
        } else if num_blocks_m >= 2 {
            1
        } else {
            0
        };
        let tile_shift = 1u32 << log_tile;
        let cfg = LaunchConfig {
            grid: (
                num_blocks_n * tile_shift,
                num_blocks_m.div_ceil(tile_shift),
                1,
            ),
            block: (32, warps, 1),
            shared_mem_bytes: smem,
        };
        let args = LaunchArgs::new()
            .buf(a_i8)
            .buf(qweight)
            .buf(s2_zeros)
            .buf(s2_scales)
            .buf(wscales)
            .buf(ascales)
            .buf(y)
            .scalar(n_tokens as i64)
            .scalar(rows as i64)
            .scalar(cols as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Per-token int8 activation quant + W4A8 GEMM in one call: quantizes the
    /// f16 activation `x` [n_tokens, cols] to symmetric int8 codes + per-token
    /// f16 scale (QServe layout) into grow-only scratch, then runs the int4-
    /// weight x int8-activation GEMM. `y` is f16 [n_tokens, rows]. `inv_smooth`
    /// is the per-input-channel SmoothQuant reciprocal `1/s` (f16 [cols]);
    /// activations are multiplied by it before the int8 quant, matching the
    /// packed weight's per-column `s` scaling. Pass an all-ones buffer for the
    /// identity (no smoothing). Both launches share `stream` (no explicit sync).
    /// Non-default (FORGE_GEMM=w4a8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        inv_smooth: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let need_codes = n_tokens * cols;
        let mut sc = self.w4a8_act.lock().expect("w4a8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.a_i8 = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.ascales = Some(self.device.alloc(
                n_tokens * 2,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.cap_tokens = n_tokens;
        }
        let a_i8 = sc.a_i8.as_ref().expect("a_i8 allocated");
        let ascales = sc.ascales.as_ref().expect("ascales allocated");

        let qk = self.artifacts.get("w4a8_quant_act")?;
        let block = (cols as u32).clamp(32, 1024);
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(x)
            .buf(a_i8)
            .buf(ascales)
            .buf(inv_smooth)
            .scalar(n_tokens as i64)
            .scalar(cols as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        self.w4a8_gemm(
            y, a_i8, qweight, s2_zeros, s2_scales, wscales, ascales, n_tokens, rows, cols, stream,
        )
    }

    /// Tile selection for the fp8 GEMM: `(suffix, BM, BN, block_threads)`. The
    /// f32 mma accumulate is exact across tile shapes (bit-identical, like the
    /// integer i8mma), so this is a pure perf gate; mirrors `gemm_i8mma_tile`.
    fn gemm_fp8_tile(rows: usize, n_tokens: usize) -> (&'static str, u32, u32, u32) {
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("_big", 128, 128, 512)
        } else if n_tokens >= 256 {
            ("", 128, 64, 256)
        } else {
            ("_bm64", 64, 64, 256)
        }
    }

    /// Per-token e4m3 activation quant + fp8 (e4m3-weight × e4m3-activation)
    /// prefill GEMM in one call: quantizes f16 `x` [n_tokens, cols] to e4m3
    /// codes + per-token f32 scale into grow-only scratch, then runs the fp8
    /// tensor-core GEMM. `w` is e4m3 bytes [rows, cols], `wscales` the per-row
    /// f32 scale [rows]. `y` is f16 [n_tokens, rows]. Both launches share
    /// `stream` (no explicit sync). `cols % 32 == 0`. Non-default
    /// (FORGE_GEMM=fp8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8 requires cols % 32 == 0, got {cols}"
            )));
        }
        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant: one block per token, block-wide absmax
        // reduction over K (block <= 1024 to fit the shared reduction array).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        let (suffix, bm, bn, threads) = Self::gemm_fp8_tile(rows, n_tokens);
        let gk = self.artifacts.get(&format!("gemm_fp8_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(bn),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(wscales)
            .buf(xq)
            .buf(xs)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Przepakowuje zakres wierszy rezydentnej macierzy NVFP4 do E4M3 na GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn pack_nvfp4_fp8(
        &self,
        output: &DevBuffer,
        output_scales: &DevBuffer,
        packed: &DevBuffer,
        scales: &DevBuffer,
        cols: usize,
        source_row_offset: usize,
        rows: usize,
        inv_global_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols < 16 || !cols.is_multiple_of(16) {
            return Err(ForgeError::Kernel(format!(
                "pack_nvfp4_fp8 wymaga rows > 0 oraz cols >= 16 podzielnego przez 16, otrzymano [{rows}, {cols}]"
            )));
        }
        let source_end = source_row_offset.checked_add(rows).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie zakresu wierszy".into())
        })?;
        let output_bytes = rows.checked_mul(cols).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru wyjścia".into())
        })?;
        let packed_bytes = source_end.checked_mul(cols / 2).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru packed".into())
        })?;
        let scale_bytes = source_end.checked_mul(cols / 16).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru scales".into())
        })?;
        let output_scale_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("pack_nvfp4_fp8: przepełnienie rozmiaru skal wyjściowych".into())
        })?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack_nvfp4_fp8: siatka przekracza u32".into()))?;
        if output.len() < output_bytes
            || output_scales.len() < output_scale_bytes
            || packed.len() < packed_bytes
            || scales.len() < scale_bytes
        {
            return Err(ForgeError::Kernel(
                "pack_nvfp4_fp8: bufor jest mniejszy od żądanego zakresu".into(),
            ));
        }
        let kernel = self.artifacts.get("pack_nvfp4_fp8")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(output_scales)
            .buf(packed)
            .buf(scales)
            .scalar(cols as i64)
            .scalar(source_row_offset as i64)
            .scalar(rows as i64)
            .scalar(inv_global_scale);
        self.device.launch(kernel, &cfg, &args, stream)
    }

    /// Przepakowuje rezydentną macierz F16 do E4M3 na GPU.
    pub fn pack_f16_fp8(
        &self,
        output: &DevBuffer,
        output_scales: &DevBuffer,
        source: &DevBuffer,
        cols: usize,
        rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || cols == 0 {
            return Err(ForgeError::Kernel(
                "pack_f16_fp8 wymaga niezerowego kształtu".into(),
            ));
        }
        let elements = rows
            .checked_mul(cols)
            .ok_or_else(|| ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru".into()))?;
        let source_bytes = elements.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru źródła".into())
        })?;
        let scale_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("pack_f16_fp8: przepełnienie rozmiaru skal".into())
        })?;
        let grid_x = u32::try_from(rows)
            .map_err(|_| ForgeError::Kernel("pack_f16_fp8: siatka przekracza u32".into()))?;
        if output.len() < elements
            || output_scales.len() < scale_bytes
            || source.len() < source_bytes
        {
            return Err(ForgeError::Kernel(
                "pack_f16_fp8: bufor jest mniejszy od żądanego kształtu".into(),
            ));
        }
        let kernel = self.artifacts.get("pack_f16_fp8")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(output_scales)
            .buf(source)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(kernel, &cfg, &args, stream)
    }

    /// Per-token e4m3 activation quant + Modular's multistage cp.async fp8 GEMM
    /// (one kernel per (rows,cols); docs/CODEGEN_PROOF.md Finding G). Same fp8
    /// weight pack + activation quant as `gemm_fp8`, but the GEMM is the deeply
    /// pipelined `multistage_gemm_kernel` (dynamic-M wrapper) that runs at
    /// 260–313 TFLOPS on Ada — 1.3–1.5× the CUDA MMQ — with the per-token ×
    /// per-row scale + f16 downcast fused into its epilogue (no extra HBM pass).
    /// Grid (ceil(rows/128), ceil(n_tokens/128)); block 128; dynamic smem 65536
    /// (the >48 KB opt-in the HAL sets automatically). Non-default
    /// (`FORGE_GEMM=fp8mod`); errors if no committed PTX matches (rows,cols).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular requires cols % 64 == 0, got {cols}"
            )));
        }
        let gk = self
            .artifacts
            .get(&format!("gemm_fp8_mod_{rows}_{cols}"))
            .map_err(|_| {
                ForgeError::Kernel(format!(
                    "gemm_fp8_modular: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
                ))
            })?;

        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant → e4m3 codes + f32 scale (shared with the
        // hand fp8 path).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        // multistage GEMM: y = diag(xs)·(xq·wᵀ)·diag(ws), fused epilogue. Params
        // mirror gemm_fp8_mod(y, a=xq, b=w, xs, ws, m=n_tokens).
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(128),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (128, 1, 1),
            shared_mem_bytes: 65536,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Grow the shared fp8 activation scratch to hold `n_tokens × cols` e4m3
    /// codes + `n_tokens` f32 scales. Called by the fused rmsnorm→fp8 path
    /// (which fills it) and the prequant GEMM (which reads it).
    fn fp8_act_ensure(&self, need_codes: usize, n_tokens: usize) -> Result<()> {
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        Ok(())
    }

    /// Fused RMSNorm → shared fp8 activation: writes the f16 normed row to
    /// `out_f16` AND the per-token e4m3 codes + f32 scale into the shared fp8
    /// activation scratch, so the following q/k/v (or gate/up) projections read
    /// ONE quantized activation via `gemm_fp8_modular_prequant` instead of
    /// re-quantizing per projection. The fp8mod analog of a fused norm→quant for the fp8mod
    /// path. `cols` is the hidden size (the projection K).
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused residual-add + RMSNorm → shared fp8 activation: `residual_io += x`,
    /// normed row to `out_f16`, shared per-token e4m3 codes + scale to scratch.
    /// See `rmsnorm_fp8_shared`.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_residual_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_residual_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Modular multistage fp8 GEMM over an EXTERNALLY prequantized activation:
    /// reads the shared fp8 activation scratch (`xq`/`xs`) that the preceding
    /// fused rmsnorm→fp8 emitted — NO per-projection quantize pass. `cols` (the
    /// projection K) must match the fused norm's hidden size that filled the
    /// scratch. Otherwise identical to `gemm_fp8_modular`. (`FORGE_GEMM=fp8mod`).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular_prequant(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular_prequant requires cols % 64 == 0, got {cols}"
            )));
        }
        let gk = self
            .artifacts
            .get(&format!("gemm_fp8_mod_{rows}_{cols}"))
            .map_err(|_| {
                ForgeError::Kernel(format!(
                    "gemm_fp8_modular_prequant: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
                ))
            })?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < n_tokens * cols || sc.cap_tokens < n_tokens {
            return Err(ForgeError::Kernel(
                "gemm_fp8_modular_prequant: shared fp8 activation scratch not sized \
                 by a preceding rmsnorm_fp8_shared"
                    .into(),
            ));
        }
        let xq = sc.xq.as_ref().expect("xq filled by fused norm");
        let xs = sc.xs.as_ref().expect("xs filled by fused norm");
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(128),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (128, 1, 1),
            shared_mem_bytes: 65536,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
        // Prefer the native-GGUF-layout Mojo int8 Q6_K multistage GEMM (reads the
        // raw `DevWeight::Q6K.buf` bytes in-kernel, bit-exact vs Q6_K × q8_1 by
        // construction). Prefill-sized batches whose (rows,cols) has a committed
        // (N,K,MPAD) instance and T ≤ 4096; anything else falls through to the f16
        // Q6_K kernel below.
        if n_tokens >= 64
            && self.gemm_q6k_i8_native(y, w_q6k, w_byte_off, x, rows, cols, n_tokens, stream)?
        {
            return Ok(());
        }
        let (suffix, block, bm) = Self::gemm_tile(rows, n_tokens);
        let k = self.artifacts.get(&format!("gemm_q6_k_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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

    /// Dokładny batch flash-decode korzystający ze wspólnej tablicy stron.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_batch_exact_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("attn_decode_batch_exact_f16_hd256")?;
        let config = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
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
        self.device.launch(kernel, &config, &args, stream)
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
            grid: ((rows as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
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
        let rpw = 3usize;
        let cfg = LaunchConfig {
            grid: ((inter as u32).div_ceil(16 * rpw as u32), 1, 1),
            block: (512, 1, 1),
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

    /// Split attention F16 dla GQA 4:1, współdzielący odczyt K/V między głowicami Q.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split_gqa4_f16_hd128(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let expected_q_heads = n_kv_heads.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: liczba głowic przekracza zakres".into())
        })?;
        if n_seqs == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || n_q_heads != expected_q_heads
            || page_size == 0
            || max_pages == 0
            || n_splits == 0
        {
            return Err(ForgeError::Kernel(format!(
                "attn_decode_split_gqa4 wymaga niezerowych wymiarów i GQA 4:1, otrzymano seqs={n_seqs}, heads={n_q_heads}:{n_kv_heads}, page={page_size}, max_pages={max_pages}, splits={n_splits}"
            )));
        }
        if !q_byte_off.is_multiple_of(2)
            || !k_byte_off.is_multiple_of(2)
            || !v_byte_off.is_multiple_of(2)
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4 wymaga offsetów wyrównanych do F16".into(),
            ));
        }
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        let q_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 q", &[n_seqs, n_q_heads, 128], 2)?;
        let kv_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 kv", &[n_seqs, n_kv_heads, 128], 2)?;
        let cache_page_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 cache",
            &[n_kv_heads, page_size, 128],
            2,
        )?;
        let page_table_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 page_table", &[n_seqs, max_pages], 4)?;
        let metadata_bytes = checked_buffer_bytes("attn_decode_split_gqa4 metadata", &[n_seqs], 4)?;
        let q_end = q_byte_off.checked_add(q_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu Q".into())
        })?;
        let k_end = k_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu K".into())
        })?;
        let v_end = v_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu V".into())
        })?;
        if parts.len() < parts_bytes
            || q_in.len() < q_end
            || k_in.len() < k_end
            || v_in.len() < v_end
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || page_table.len() < page_table_bytes
            || seq_lens.len() < metadata_bytes
            || positions.len() < metadata_bytes
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza zakres siatki".into())
        })?;
        let grid_z = u32::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza zakres siatki".into())
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_kv_heads_i64 = i64::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza ABI Mojo".into())
        })?;
        let page_size_i64 = i64::try_from(page_size).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: page_size przekracza ABI Mojo".into())
        })?;
        let max_pages_i64 = i64::try_from(max_pages).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: max_pages przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_split_gqa4_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, grid_z),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_in)
            .buf(k_in)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads_i64)
            .scalar(n_kv_heads_i64)
            .scalar(page_size_i64)
            .scalar(max_pages_i64)
            .scalar(n_splits_i64)
            .scalar(0i64)
            .scalar(0i64)
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

    /// Laczy partiale GQA hd128, przetwarzajac dwie glowice Q w jednym CTA.
    pub fn attn_decode_combine_gqa2_f16_hd128(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_seqs == 0 || n_q_heads == 0 || n_splits == 0 {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2 wymaga niezerowych wymiarów".into(),
            ));
        }
        let out_bytes =
            checked_buffer_bytes("attn_decode_combine_gqa2 out", &[n_seqs, n_q_heads, 128], 2)?;
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_combine_gqa2 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        if out.len() < out_bytes || parts.len() < parts_bytes {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_q_heads.div_ceil(2)).map_err(|_| {
            ForgeError::Kernel(
                "attn_decode_combine_gqa2: n_q_heads przekracza zakres siatki".into(),
            )
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_combine_gqa2_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads_i64)
            .scalar(n_splits_i64);
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
        let k = self
            .artifacts
            .get(&format!("gemm_mxfp4_gguf_f16{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
            grid: (
                (rows as u32).div_ceil(64),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
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
    pub fn relu_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("relu_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sigmoid(x) over n f32 elements.
    pub fn sigmoid_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
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
    pub fn sqrt_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
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

#[cfg(test)]
mod nvfp4_gguf_dispatch_tests {
    use super::{nvfp4_gguf_dispatch, raw_nvfp4_dp4a_supported};

    #[test]
    fn wybiera_dokladne_buckety_weryfikatora() {
        for (tokens, expected, block) in [
            (2, "gemm_nvfp4_gguf_f16_b2", 32),
            (3, "gemm_nvfp4_gguf_f16_b3_nvidia", 64),
            (4, "gemm_nvfp4_gguf_f16_b4_nvidia", 64),
            (5, "gemm_nvfp4_gguf_f16_b8", 256),
            (8, "gemm_nvfp4_gguf_f16_b8", 256),
            (9, "gemm_nvfp4_gguf_f16_b16", 512),
            (16, "gemm_nvfp4_gguf_f16_b16", 512),
        ] {
            let dispatch = nvfp4_gguf_dispatch(tokens, true, 32, 1024).unwrap();
            assert_eq!(dispatch.kernel, expected);
            assert_eq!(dispatch.block_threads, block);
        }
        for tokens in 2..=4 {
            let dispatch = nvfp4_gguf_dispatch(tokens, false, 64, 1024).unwrap();
            assert_eq!(dispatch.block_threads, 64);
        }
        assert_eq!(
            nvfp4_gguf_dispatch(3, false, 64, 1024).unwrap().kernel,
            "gemm_nvfp4_gguf_f16_b3"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(4, false, 64, 1024).unwrap().kernel,
            "gemm_nvfp4_gguf_f16_b4"
        );
    }

    #[test]
    fn wybiera_mma_tylko_dla_nvidia() {
        assert_eq!(
            nvfp4_gguf_dispatch(17, true, 32, 1024).unwrap().kernel,
            "gemm_nvfp4_gguf_mma_f16_bm32"
        );
        assert_eq!(
            nvfp4_gguf_dispatch(128, true, 32, 1024).unwrap().kernel,
            "gemm_nvfp4_gguf_mma_f16_bm128"
        );
        assert!(nvfp4_gguf_dispatch(17, false, 64, 1024).is_err());
        assert!(nvfp4_gguf_dispatch(17, true, 64, 1024).is_err());
    }

    #[test]
    fn odrzuca_nieprawidlowy_rozmiar_bloku() {
        assert!(nvfp4_gguf_dispatch(1, true, 32, 1024).is_err());
        assert!(nvfp4_gguf_dispatch(16, false, 64, 512).is_err());
        assert!(nvfp4_gguf_dispatch(3, false, 0, 1024).is_err());
    }

    #[test]
    fn dp4a_wymaga_nvidia_i_warp32() {
        assert!(raw_nvfp4_dp4a_supported(true, 32));
        assert!(!raw_nvfp4_dp4a_supported(true, 64));
        assert!(!raw_nvfp4_dp4a_supported(false, 32));
        assert!(!raw_nvfp4_dp4a_supported(false, 64));
    }
}
