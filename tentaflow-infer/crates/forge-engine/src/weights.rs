// ===== File: weights.rs — model weight upload: GGUF / safetensors → device buffers =====
// Weight matrices stay in their storage quantization on the GPU (fused
// dequant-GEMV kernels read them directly); norms and the embedding table are
// materialized as f16 because they feed non-quantized kernels.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use forge_formats::nvfp4::{self, NvFp4Scheme, NvFp4TensorNames};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_formats::{dequantize_to_f32, Gguf, HfConfig, ModelDescriptor, WeightRole};
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
            | DevWeight::NvFp4 { rows, .. } => *rows,
        }
    }

    pub fn cols(&self) -> usize {
        match self {
            DevWeight::F16 { cols, .. }
            | DevWeight::Q8_0 { cols, .. }
            | DevWeight::Q4K { cols, .. }
            | DevWeight::NvFp4 { cols, .. } => *cols,
        }
    }
}

/// Q/K/V projections: one row-concatenated matrix when the three share a
/// storage format (single GEMV/GEMM launch, single copy in VRAM), separate
/// matrices otherwise. Fused row order is q, then k, then v.
pub enum QkvWeights {
    Fused(DevWeight),
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

/// Per-layer weight set (roles resolved by the arch registry).
pub struct LayerWeights {
    pub attn_norm: DevBuffer,
    pub ffn_norm: DevBuffer,
    /// Optional per-head QK norms (qwen3); f16 vectors of head_dim.
    pub q_norm: Option<DevBuffer>,
    pub k_norm: Option<DevBuffer>,
    pub attn_qkv: QkvWeights,
    pub attn_o: DevWeight,
    pub ffn_gate_up: GateUpWeights,
    pub ffn_down: DevWeight,
}

pub struct ModelWeights {
    pub descriptor: ModelDescriptor,
    /// Token embedding table, always f16 [vocab, hidden] (gather kernel input).
    pub token_embd_f16: DevBuffer,
    pub output_norm: DevBuffer,
    /// LM head. For tied embeddings this is a separate f16 view built from the
    /// same host data (kept simple; dedup is a later optimization).
    pub lm_head: DevWeight,
    pub layers: Vec<LayerWeights>,
    /// Layers whose Q/K/V (resp. gate/up) landed as one fused matrix — the
    /// rest fell back to split storage (format or NVFP4 global-scale mismatch).
    pub fused_qkv_layers: usize,
    pub fused_gate_up_layers: usize,
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
            | HostWeight::NvFp4 { rows, .. } => *rows,
        }
    }

    fn cols(&self) -> usize {
        match self {
            HostWeight::F16 { cols, .. }
            | HostWeight::Q8_0 { cols, .. }
            | HostWeight::Q4K { cols, .. }
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
    match quant {
        QuantKind::Q8_0 => Ok(HostWeight::Q8_0 { data, rows, cols }),
        // Whole 256-element superblocks per row keep every 144-byte block
        // 16-byte aligned for the fused kernels' wide loads (Q4K_MAX_SEGS in
        // gemv2.mojo bounds the shared x-sum staging).
        QuantKind::Q4K if cols.is_multiple_of(256) && cols <= 32768 => {
            Ok(HostWeight::Q4K { data, rows, cols })
        }
        // Everything else goes through f32 → f16. This covers F16/F32/BF16
        // directly and any other GGML quant via the CPU reference dequant —
        // correctness first; fused kernels for more quants land per PLAN.
        _ => {
            let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
            Ok(HostWeight::F16 {
                data: f32s_to_f16_bytes(&f32s),
                rows,
                cols,
            })
        }
    }
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
            | (HostWeight::Q4K { data, .. }, HostWeight::Q4K { data: d, .. }) => {
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
        | HostWeight::NvFp4 { rows: r, .. } => *r = rows,
    }
    Ok(fused)
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

        let mut layers = Vec::with_capacity(descriptor.params.block_count);
        let mut fused_qkv_layers = 0usize;
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
                    QkvWeights::Split {
                        q: upload_weight(device, q)?,
                        k: upload_weight(device, k)?,
                        v: upload_weight(device, v)?,
                    }
                }
            };

            let gate = fetch_matrix(src, name(WeightRole::FfnGate)?)?;
            let up = fetch_matrix(src, name(WeightRole::FfnUp)?)?;
            expect(&at("ffn_gate"), &gate, p.intermediate_size, p.hidden_size)?;
            expect(&at("ffn_up"), &up, p.intermediate_size, p.hidden_size)?;
            let ffn_gate_up = match fuse_rows(vec![gate, up]) {
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

            let attn_o = fetch_matrix(src, name(WeightRole::AttnO)?)?;
            expect(&at("attn_o"), &attn_o, p.hidden_size, q_dim)?;
            let ffn_down = fetch_matrix(src, name(WeightRole::FfnDown)?)?;
            expect(&at("ffn_down"), &ffn_down, p.hidden_size, p.intermediate_size)?;

            layers.push(LayerWeights {
                attn_norm: upload_norm(device, src, name(WeightRole::AttnNorm)?)?,
                ffn_norm: upload_norm(device, src, name(WeightRole::FfnNorm)?)?,
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
                ffn_gate_up,
                ffn_down: upload_weight(device, ffn_down)?,
            });
        }

        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            output_norm,
            lm_head,
            layers,
            fused_qkv_layers,
            fused_gate_up_layers,
        })
    }

    /// Weight-role → tensor-name map used for diagnostics.
    pub fn describe(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("arch".into(), self.descriptor.arch.clone());
        m.insert(
            "layers".into(),
            self.descriptor.params.block_count.to_string(),
        );
        m.insert(
            "fused_qkv_layers".into(),
            self.fused_qkv_layers.to_string(),
        );
        m.insert(
            "fused_gate_up_layers".into(),
            self.fused_gate_up_layers.to_string(),
        );
        m
    }
}
