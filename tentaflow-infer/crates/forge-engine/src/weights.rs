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
    F16 { buf: DevBuffer, rows: usize, cols: usize },
    /// GGML Q8_0 block stream for [rows, cols].
    Q8_0 { buf: DevBuffer, rows: usize, cols: usize },
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
            | DevWeight::NvFp4 { rows, .. } => *rows,
        }
    }

    pub fn cols(&self) -> usize {
        match self {
            DevWeight::F16 { cols, .. }
            | DevWeight::Q8_0 { cols, .. }
            | DevWeight::NvFp4 { cols, .. } => *cols,
        }
    }
}

/// Per-layer weight set (roles resolved by the arch registry).
pub struct LayerWeights {
    pub attn_norm: DevBuffer,
    pub ffn_norm: DevBuffer,
    /// Optional per-head QK norms (qwen3); f16 vectors of head_dim.
    pub q_norm: Option<DevBuffer>,
    pub k_norm: Option<DevBuffer>,
    pub attn_q: DevWeight,
    pub attn_k: DevWeight,
    pub attn_v: DevWeight,
    pub attn_o: DevWeight,
    pub ffn_gate: DevWeight,
    pub ffn_up: DevWeight,
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
}

/// Source-agnostic host-side tensor fetch: (bytes, dtype, quant, dims).
trait TensorSource {
    fn fetch(&self, name: &str) -> Result<(Vec<u8>, DType, QuantKind, Vec<usize>)>;
    /// NVFP4 triple fetch; None when the tensor is not NVFP4-packed.
    fn fetch_nvfp4(&self, name: &str) -> Result<Option<NvFp4Host>>;
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
}

struct StSource<'a> {
    st: &'a ShardedSafeTensors,
    scheme: Option<NvFp4Scheme>,
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

/// Upload a weight matrix in the most direct form a kernel can consume.
fn upload_matrix(device: &dyn Device, src: &dyn TensorSource, name: &str) -> Result<DevWeight> {
    if let Some(nv) = src.fetch_nvfp4(name)? {
        // Validate on CPU once so a corrupt checkpoint fails at load, not as
        // garbage tokens at runtime.
        nvfp4::dequantize_nvfp4(&nv.packed, &nv.scales, nv.global_scale, nv.rows, nv.cols, 16)?;
        return Ok(DevWeight::NvFp4 {
            packed: upload(device, &nv.packed)?,
            scales: upload(device, &nv.scales)?,
            inv_global_scale: 1.0 / nv.global_scale,
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
        QuantKind::Q8_0 => Ok(DevWeight::Q8_0 {
            buf: upload(device, &data)?,
            rows,
            cols,
        }),
        // Everything else goes through f32 → f16. This covers F16/F32/BF16
        // directly and any other GGML quant via the CPU reference dequant —
        // correctness first; fused kernels for more quants land per PLAN.
        _ => {
            let f32s = dequantize_to_f32(dtype, quant, &data, rows * cols)?;
            Ok(DevWeight::F16 {
                buf: upload(device, &f32s_to_f16_bytes(&f32s))?,
                rows,
                cols,
            })
        }
    }
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
        let src = StSource { st: &st, scheme };
        Self::load(device.as_ref(), descriptor, &src)
    }

    fn load(device: &dyn Device, descriptor: ModelDescriptor, src: &dyn TensorSource) -> Result<Self> {
        let global = |role: WeightRole| -> Result<&String> {
            descriptor
                .globals
                .get(&role)
                .ok_or_else(|| ForgeError::Format(format!("missing global weight {role:?}")))
        };

        let embd_name = global(WeightRole::TokenEmbd)?;
        let (token_embd_f16, vocab, hidden) = upload_embedding(device, src, embd_name)?;
        let output_norm = upload_norm(device, src, global(WeightRole::OutputNorm)?)?;

        let lm_head = if let Some(name) = descriptor.globals.get(&WeightRole::LmHead) {
            upload_matrix(device, src, name)?
        } else {
            // Tied embeddings: reuse the f16 host conversion by fetching again
            // as a matrix (upload_matrix handles quant → f16).
            upload_matrix(device, src, embd_name)?
        };
        if lm_head.rows() != vocab || lm_head.cols() != hidden {
            return Err(ForgeError::Format(format!(
                "lm_head shape [{}, {}] does not match embedding [{vocab}, {hidden}]",
                lm_head.rows(),
                lm_head.cols()
            )));
        }

        let mut layers = Vec::with_capacity(descriptor.params.block_count);
        for (idx, layer_map) in descriptor.layers.iter().enumerate() {
            let name = |role: WeightRole| -> Result<&String> {
                layer_map
                    .get(&role)
                    .ok_or_else(|| ForgeError::Format(format!("layer {idx}: missing {role:?}")))
            };
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
                attn_q: upload_matrix(device, src, name(WeightRole::AttnQ)?)?,
                attn_k: upload_matrix(device, src, name(WeightRole::AttnK)?)?,
                attn_v: upload_matrix(device, src, name(WeightRole::AttnV)?)?,
                attn_o: upload_matrix(device, src, name(WeightRole::AttnO)?)?,
                ffn_gate: upload_matrix(device, src, name(WeightRole::FfnGate)?)?,
                ffn_up: upload_matrix(device, src, name(WeightRole::FfnUp)?)?,
                ffn_down: upload_matrix(device, src, name(WeightRole::FfnDown)?)?,
            });
        }

        Ok(ModelWeights {
            descriptor,
            token_embd_f16,
            output_norm,
            lm_head,
            layers,
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
        m
    }
}
