// ===== File: weights.rs — Whisper HF-safetensors weight loading (f16 upload) =====
// Parses config.json / generation_config.json (Whisper's schema differs from
// the causal-LM HfConfig) and uploads every tensor to the device weights pool
// as f16. proj_out is tied to decoder.embed_tokens, so logits reuse that
// buffer.

use std::path::Path;

use forge_formats::safetensors::ShardedSafeTensors;
use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{DType, ForgeError, MemKind, Result};
use half::f16;
use serde::Deserialize;

/// Model hyperparameters from config.json (HF Whisper schema).
#[derive(Debug, Clone, Deserialize)]
pub struct WhisperConfig {
    pub d_model: usize,
    pub encoder_layers: usize,
    pub decoder_layers: usize,
    pub encoder_attention_heads: usize,
    pub decoder_attention_heads: usize,
    pub encoder_ffn_dim: usize,
    pub decoder_ffn_dim: usize,
    pub num_mel_bins: usize,
    pub max_source_positions: usize,
    pub max_target_positions: usize,
    pub vocab_size: usize,
    pub decoder_start_token_id: u32,
    pub eos_token_id: u32,
}

impl WhisperConfig {
    pub fn head_dim(&self) -> usize {
        self.d_model / self.encoder_attention_heads
    }
}

/// Sampling constraints from generation_config.json.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WhisperGenerationConfig {
    #[serde(default)]
    pub suppress_tokens: Vec<u32>,
    #[serde(default)]
    pub begin_suppress_tokens: Vec<u32>,
}

pub struct Attention {
    pub q_w: DevBuffer,
    pub q_b: DevBuffer,
    /// k_proj carries no bias in Whisper.
    pub k_w: DevBuffer,
    pub v_w: DevBuffer,
    pub v_b: DevBuffer,
    pub o_w: DevBuffer,
    pub o_b: DevBuffer,
}

pub struct LayerNormW {
    pub w: DevBuffer,
    pub b: DevBuffer,
}

pub struct EncoderLayer {
    pub self_attn_ln: LayerNormW,
    pub self_attn: Attention,
    pub final_ln: LayerNormW,
    pub fc1_w: DevBuffer,
    pub fc1_b: DevBuffer,
    pub fc2_w: DevBuffer,
    pub fc2_b: DevBuffer,
}

pub struct DecoderLayer {
    pub self_attn_ln: LayerNormW,
    pub self_attn: Attention,
    pub cross_attn_ln: LayerNormW,
    pub cross_attn: Attention,
    pub final_ln: LayerNormW,
    pub fc1_w: DevBuffer,
    pub fc1_b: DevBuffer,
    pub fc2_w: DevBuffer,
    pub fc2_b: DevBuffer,
}

pub struct WhisperWeights {
    pub config: WhisperConfig,
    pub generation: WhisperGenerationConfig,
    pub conv1_w: DevBuffer,
    pub conv1_b: DevBuffer,
    pub conv2_w: DevBuffer,
    pub conv2_b: DevBuffer,
    /// Host-side copy of encoder positional embeddings: they are added during
    /// the CPU transpose of the conv-stem output, never on device.
    pub enc_pos_host: Vec<f32>,
    pub enc_layers: Vec<EncoderLayer>,
    pub enc_ln: LayerNormW,
    /// Tied with proj_out: the logits GEMV reads this buffer directly.
    pub tok_emb: DevBuffer,
    pub dec_pos: DevBuffer,
    pub dec_layers: Vec<DecoderLayer>,
    pub dec_ln: LayerNormW,
}

fn to_f16_bytes(name: &str, dtype: DType, raw: &[u8]) -> Result<Vec<u8>> {
    match dtype {
        DType::F16 => Ok(raw.to_vec()),
        DType::F32 => {
            let src: &[f32] = bytemuck::cast_slice(raw);
            let dst: Vec<f16> = src.iter().map(|&v| f16::from_f32(v)).collect();
            Ok(bytemuck::cast_slice(&dst).to_vec())
        }
        other => Err(ForgeError::Unsupported(format!(
            "whisper tensor '{name}': dtype {other:?} not supported"
        ))),
    }
}

fn tensor_f32(st: &ShardedSafeTensors, name: &str) -> Result<Vec<f32>> {
    let t = st
        .tensor(name)
        .ok_or_else(|| ForgeError::Format(format!("whisper: missing tensor '{name}'")))?;
    let raw = st.data(name)?;
    match t.dtype {
        DType::F32 => Ok(bytemuck::cast_slice::<u8, f32>(raw).to_vec()),
        DType::F16 => Ok(bytemuck::cast_slice::<u8, f16>(raw)
            .iter()
            .map(|v| v.to_f32())
            .collect()),
        other => Err(ForgeError::Unsupported(format!(
            "whisper tensor '{name}': dtype {other:?} not supported"
        ))),
    }
}

struct Loader<'a> {
    device: &'a dyn Device,
    st: &'a ShardedSafeTensors,
}

impl Loader<'_> {
    fn upload(&self, name: &str, expect_shape: &[usize]) -> Result<DevBuffer> {
        let t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("whisper: missing tensor '{name}'")))?;
        if t.shape != expect_shape {
            return Err(ForgeError::Format(format!(
                "whisper tensor '{name}': shape {:?}, expected {:?}",
                t.shape, expect_shape
            )));
        }
        let bytes = to_f16_bytes(name, t.dtype, self.st.data(name)?)?;
        let buf = self
            .device
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)?;
        self.device.write(&bytes, &buf, 0)?;
        Ok(buf)
    }

    fn layer_norm(&self, prefix: &str, d: usize) -> Result<LayerNormW> {
        Ok(LayerNormW {
            w: self.upload(&format!("{prefix}.weight"), &[d])?,
            b: self.upload(&format!("{prefix}.bias"), &[d])?,
        })
    }

    fn attention(&self, prefix: &str, d: usize) -> Result<Attention> {
        Ok(Attention {
            q_w: self.upload(&format!("{prefix}.q_proj.weight"), &[d, d])?,
            q_b: self.upload(&format!("{prefix}.q_proj.bias"), &[d])?,
            k_w: self.upload(&format!("{prefix}.k_proj.weight"), &[d, d])?,
            v_w: self.upload(&format!("{prefix}.v_proj.weight"), &[d, d])?,
            v_b: self.upload(&format!("{prefix}.v_proj.bias"), &[d])?,
            o_w: self.upload(&format!("{prefix}.out_proj.weight"), &[d, d])?,
            o_b: self.upload(&format!("{prefix}.out_proj.bias"), &[d])?,
        })
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)
        .map_err(|e| ForgeError::Format(format!("whisper: read {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ForgeError::Format(format!("whisper: parse {}: {e}", path.display())))
}

impl WhisperWeights {
    pub fn load(device: &dyn Device, dir: &Path) -> Result<Self> {
        let config: WhisperConfig = read_json(&dir.join("config.json"))?;
        let generation: WhisperGenerationConfig = read_json(&dir.join("generation_config.json"))?;
        if config.head_dim() != 64 && config.head_dim() != 128 {
            return Err(ForgeError::Unsupported(format!(
                "whisper head_dim {} has no attention specialization",
                config.head_dim()
            )));
        }
        if config.encoder_attention_heads != config.decoder_attention_heads {
            return Err(ForgeError::Unsupported(
                "whisper: differing encoder/decoder head counts".into(),
            ));
        }

        let st = ShardedSafeTensors::load_dir(dir)?;
        let l = Loader {
            device,
            st: &st,
        };
        let d = config.d_model;
        let mels = config.num_mel_bins;

        let mut enc_layers = Vec::with_capacity(config.encoder_layers);
        for i in 0..config.encoder_layers {
            let p = format!("model.encoder.layers.{i}");
            enc_layers.push(EncoderLayer {
                self_attn_ln: l.layer_norm(&format!("{p}.self_attn_layer_norm"), d)?,
                self_attn: l.attention(&format!("{p}.self_attn"), d)?,
                final_ln: l.layer_norm(&format!("{p}.final_layer_norm"), d)?,
                fc1_w: l.upload(&format!("{p}.fc1.weight"), &[config.encoder_ffn_dim, d])?,
                fc1_b: l.upload(&format!("{p}.fc1.bias"), &[config.encoder_ffn_dim])?,
                fc2_w: l.upload(&format!("{p}.fc2.weight"), &[d, config.encoder_ffn_dim])?,
                fc2_b: l.upload(&format!("{p}.fc2.bias"), &[d])?,
            });
        }

        let mut dec_layers = Vec::with_capacity(config.decoder_layers);
        for i in 0..config.decoder_layers {
            let p = format!("model.decoder.layers.{i}");
            dec_layers.push(DecoderLayer {
                self_attn_ln: l.layer_norm(&format!("{p}.self_attn_layer_norm"), d)?,
                self_attn: l.attention(&format!("{p}.self_attn"), d)?,
                cross_attn_ln: l.layer_norm(&format!("{p}.encoder_attn_layer_norm"), d)?,
                cross_attn: l.attention(&format!("{p}.encoder_attn"), d)?,
                final_ln: l.layer_norm(&format!("{p}.final_layer_norm"), d)?,
                fc1_w: l.upload(&format!("{p}.fc1.weight"), &[config.decoder_ffn_dim, d])?,
                fc1_b: l.upload(&format!("{p}.fc1.bias"), &[config.decoder_ffn_dim])?,
                fc2_w: l.upload(&format!("{p}.fc2.weight"), &[d, config.decoder_ffn_dim])?,
                fc2_b: l.upload(&format!("{p}.fc2.bias"), &[d])?,
            });
        }

        Ok(WhisperWeights {
            conv1_w: l.upload("model.encoder.conv1.weight", &[d, mels, 3])?,
            conv1_b: l.upload("model.encoder.conv1.bias", &[d])?,
            conv2_w: l.upload("model.encoder.conv2.weight", &[d, d, 3])?,
            conv2_b: l.upload("model.encoder.conv2.bias", &[d])?,
            enc_pos_host: tensor_f32(&st, "model.encoder.embed_positions.weight")?,
            enc_ln: l.layer_norm("model.encoder.layer_norm", d)?,
            tok_emb: l.upload("model.decoder.embed_tokens.weight", &[config.vocab_size, d])?,
            dec_pos: l.upload(
                "model.decoder.embed_positions.weight",
                &[config.max_target_positions, d],
            )?,
            dec_ln: l.layer_norm("model.decoder.layer_norm", d)?,
            enc_layers,
            dec_layers,
            config,
            generation,
        })
    }
}
