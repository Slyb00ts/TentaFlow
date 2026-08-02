// ===== File: weights.rs — Whisper HF-safetensors weight loading (f16 upload) =====
// Parses config.json / generation_config.json (Whisper's schema differs from
// the causal-LM HfConfig) and uploads every tensor to the device weights pool
// as f16. proj_out is tied to decoder.embed_tokens, so logits reuse that
// buffer.

use std::path::Path;

use forge_formats::safetensors::ShardedSafeTensors;
use forge_formats::{dequantize_affine, MlxAffineTensor, MlxParams, MlxQuantConfig};
use forge_hal::{DevBuffer, Device, Pool};
use forge_types::{DType, ForgeError, MemKind, Result};
use half::{bf16, f16};
use serde::Deserialize;

use crate::flavour::{conv_to_out_in_k, sinusoids, Names, OpenAiDims, WhisperFlavour};

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

    /// Builds the dimensions from either schema. The OpenAI variant keeps no
    /// token ids in config.json, so they come from generation_config.json —
    /// missing there is an error, not a default, because a wrong start token
    /// produces fluent output in the wrong language rather than a failure.
    pub fn parse(
        config: &serde_json::Value,
        generation: &WhisperGenerationConfig,
    ) -> Result<(Self, WhisperFlavour)> {
        let flavour = WhisperFlavour::detect(config)?;
        match flavour {
            WhisperFlavour::HfTransformers => {
                let cfg: WhisperConfig = serde_json::from_value(config.clone()).map_err(|e| {
                    ForgeError::Format(format!("whisper config.json (HF): {e}"))
                })?;
                Ok((cfg, flavour))
            }
            WhisperFlavour::MlxOpenAi => {
                let d: OpenAiDims = serde_json::from_value(config.clone()).map_err(|e| {
                    ForgeError::Format(format!("whisper config.json (OpenAI/MLX): {e}"))
                })?;
                let need = |what: &str, v: Option<u32>| {
                    v.ok_or_else(|| {
                        ForgeError::Format(format!(
                            "whisper: checkpoint MLX nie podaje `{what}` w generation_config.json"
                        ))
                    })
                };
                Ok((
                    WhisperConfig {
                        d_model: d.n_audio_state,
                        encoder_layers: d.n_audio_layer,
                        decoder_layers: d.n_text_layer,
                        encoder_attention_heads: d.n_audio_head,
                        decoder_attention_heads: d.n_text_head,
                        encoder_ffn_dim: OpenAiDims::ffn(d.n_audio_state),
                        decoder_ffn_dim: OpenAiDims::ffn(d.n_text_state),
                        num_mel_bins: d.n_mels,
                        max_source_positions: d.n_audio_ctx,
                        max_target_positions: d.n_text_ctx,
                        vocab_size: d.n_vocab,
                        decoder_start_token_id: need(
                            "decoder_start_token_id",
                            generation.decoder_start_token_id,
                        )?,
                        eos_token_id: need("eos_token_id", generation.eos_token_id)?,
                    },
                    flavour,
                ))
            }
        }
    }
}

/// Sampling constraints from generation_config.json.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WhisperGenerationConfig {
    #[serde(default)]
    pub suppress_tokens: Vec<u32>,
    #[serde(default)]
    pub begin_suppress_tokens: Vec<u32>,
    /// `false` on English-only (.en) exports. Older exports omit the field;
    /// the vocabulary size disambiguates then (51864 vs 51865+).
    pub is_multilingual: Option<bool>,
    /// The OpenAI-flavoured config.json carries no token ids at all, so for
    /// those checkpoints this file is the only source.
    pub decoder_start_token_id: Option<u32>,
    pub eos_token_id: Option<u32>,
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
    // Element-wise for the same reason as `Loader::params`: mapped tensor data
    // carries no alignment guarantee, and a wide cast panics rather than
    // returning an error anyone can handle.
    match t.dtype {
        DType::F32 => Ok(raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()),
        DType::F16 => Ok(raw
            .chunks_exact(2)
            .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
            .collect()),
        other => Err(ForgeError::Unsupported(format!(
            "whisper tensor '{name}': dtype {other:?} not supported"
        ))),
    }
}

struct Loader<'a> {
    device: &'a dyn Device,
    st: &'a ShardedSafeTensors,
    names: &'static Names,
    /// `Some` for a quantized checkpoint; then a weight may be a triple.
    quant: Option<MlxQuantConfig>,
}

impl Loader<'_> {
    fn alloc_write(&self, bytes: &[u8]) -> Result<DevBuffer> {
        let buf = self
            .device
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)?;
        self.device.write(bytes, &buf, 0)?;
        Ok(buf)
    }

    /// Reads the scales or zero points of a quantized weight in whatever float
    /// type the converter used: mlx-lm writes bf16, mlx-whisper writes f16.
    ///
    /// Decoded element by element rather than cast: safetensors places tensor
    /// data at arbitrary byte offsets, so a wide cast over the mapped bytes
    /// panics on alignment for exactly the tensors this path exists to read.
    fn params(&self, name: &str, raw: &[u8], dtype: DType) -> Result<MlxParamsOwned> {
        let bits = || -> Vec<u16> {
            raw.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        };
        match dtype {
            DType::BF16 => Ok(MlxParamsOwned::Bf16(
                bits().into_iter().map(bf16::from_bits).collect(),
            )),
            DType::F16 => Ok(MlxParamsOwned::F16(
                bits().into_iter().map(f16::from_bits).collect(),
            )),
            other => Err(ForgeError::Unsupported(format!(
                "whisper '{name}': skale w typie {other:?}"
            ))),
        }
    }

    /// Dequantizes an MLX triple to f16, or returns `None` when the tensor is
    /// stored plainly. The logical shape is checked AFTER unpacking, because
    /// the stored shape of a packed weight is narrower by the packing factor.
    fn upload_quantized(&self, name: &str, expect_shape: &[usize]) -> Result<Option<DevBuffer>> {
        let Some(cfg) = self.quant else {
            return Ok(None);
        };
        // Only a `.weight` can carry a quantization triple; biases and layer
        // norms are stored plainly even in a quantized checkpoint.
        let Some(base) = name.strip_suffix(".weight") else {
            return Ok(None);
        };
        let (scales_name, biases_name) = (format!("{base}.scales"), format!("{base}.biases"));
        let (Some(scales_t), Some(biases_t)) = (
            self.st.tensor(&scales_name),
            self.st.tensor(&biases_name),
        ) else {
            return Ok(None);
        };

        let packed_t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("whisper: missing tensor '{name}'")))?;
        if expect_shape.len() != 2 {
            return Err(ForgeError::Format(format!(
                "whisper '{name}': kwantyzacja dotyczy wyłącznie macierzy 2D"
            )));
        }
        let (rows, cols) = (expect_shape[0], expect_shape[1]);
        if packed_t.shape != [rows, cols / cfg.per_word()] {
            return Err(ForgeError::Format(format!(
                "whisper '{name}': kształt upakowany {:?}, oczekiwano {:?}",
                packed_t.shape,
                [rows, cols / cfg.per_word()]
            )));
        }

        let scales_raw = self.st.data(&scales_name)?;
        let biases_raw = self.st.data(&biases_name)?;
        let scales = self.params(&scales_name, scales_raw, scales_t.dtype)?;
        let biases = self.params(&biases_name, biases_raw, biases_t.dtype)?;
        let packed: Vec<u32> = self
            .st
            .data(name)?
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let tensor = MlxAffineTensor {
            packed: &packed,
            scales: scales.view(),
            biases: biases.view(),
            rows,
            cols,
        };
        let mut values = vec![0f32; rows * cols];
        dequantize_affine(&tensor, &cfg, &mut values)?;
        let half: Vec<f16> = values.iter().map(|&v| f16::from_f32(v)).collect();
        Ok(Some(self.alloc_write(bytemuck::cast_slice(&half))?))
    }

    fn upload(&self, name: &str, expect_shape: &[usize]) -> Result<DevBuffer> {
        if let Some(buf) = self.upload_quantized(name, expect_shape)? {
            return Ok(buf);
        }
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
        self.alloc_write(&bytes)
    }

    /// Convolution weights, normalised to [out, in, kernel]. A transposed
    /// kernel raises no error anywhere downstream — the model simply mishears.
    fn upload_conv(&self, name: &str, out_ch: usize, in_ch: usize, kernel: usize)
        -> Result<DevBuffer> {
        let t = self
            .st
            .tensor(name)
            .ok_or_else(|| ForgeError::Format(format!("whisper: missing tensor '{name}'")))?;
        let want = match self.names.conv_layout {
            crate::flavour::ConvLayout::OutInK => vec![out_ch, in_ch, kernel],
            crate::flavour::ConvLayout::OutKIn => vec![out_ch, kernel, in_ch],
        };
        if t.shape != want {
            return Err(ForgeError::Format(format!(
                "whisper conv '{name}': shape {:?}, expected {want:?}",
                t.shape
            )));
        }
        let values = tensor_f32(self.st, name)?;
        let fixed = conv_to_out_in_k(&values, out_ch, in_ch, kernel, self.names.conv_layout);
        let half: Vec<f16> = fixed.iter().map(|&v| f16::from_f32(v)).collect();
        self.alloc_write(bytemuck::cast_slice(&half))
    }

    fn layer_norm(&self, prefix: &str, d: usize) -> Result<LayerNormW> {
        Ok(LayerNormW {
            w: self.upload(&format!("{prefix}.weight"), &[d])?,
            b: self.upload(&format!("{prefix}.bias"), &[d])?,
        })
    }

    fn attention(&self, prefix: &str, d: usize) -> Result<Attention> {
        let n = self.names;
        Ok(Attention {
            q_w: self.upload(&format!("{prefix}.{}.weight", n.q), &[d, d])?,
            q_b: self.upload(&format!("{prefix}.{}.bias", n.q), &[d])?,
            k_w: self.upload(&format!("{prefix}.{}.weight", n.k), &[d, d])?,
            v_w: self.upload(&format!("{prefix}.{}.weight", n.v), &[d, d])?,
            v_b: self.upload(&format!("{prefix}.{}.bias", n.v), &[d])?,
            o_w: self.upload(&format!("{prefix}.{}.weight", n.o), &[d, d])?,
            o_b: self.upload(&format!("{prefix}.{}.bias", n.o), &[d])?,
        })
    }
}

/// Owns the borrowed slice so the caller can hand a `MlxParams` view to the
/// decoder without the two float types leaking into every signature.
enum MlxParamsOwned {
    Bf16(Vec<bf16>),
    F16(Vec<f16>),
}

impl MlxParamsOwned {
    fn view(&self) -> MlxParams<'_> {
        match self {
            MlxParamsOwned::Bf16(v) => MlxParams::Bf16(v),
            MlxParamsOwned::F16(v) => MlxParams::F16(v),
        }
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
        let raw_config: serde_json::Value = read_json(&dir.join("config.json"))?;
        let generation: WhisperGenerationConfig = read_json(&dir.join("generation_config.json"))?;
        let (config, flavour) = WhisperConfig::parse(&raw_config, &generation)?;
        let quant = MlxQuantConfig::from_config(&raw_config)?;
        let names = flavour.names();
        let st = ShardedSafeTensors::load_dir(dir)?;
        let l = Loader {
            device,
            st: &st,
            names,
            quant,
        };
        let d = config.d_model;
        let mels = config.num_mel_bins;

        let mut enc_layers = Vec::with_capacity(config.encoder_layers);
        for i in 0..config.encoder_layers {
            let p = format!("{}.{i}", names.enc_block);
            enc_layers.push(EncoderLayer {
                self_attn_ln: l.layer_norm(&format!("{p}.{}", names.self_attn_ln), d)?,
                self_attn: l.attention(&format!("{p}.{}", names.self_attn), d)?,
                final_ln: l.layer_norm(&format!("{p}.{}", names.mlp_ln), d)?,
                fc1_w: l.upload(
                    &format!("{p}.{}.weight", names.fc1),
                    &[config.encoder_ffn_dim, d],
                )?,
                fc1_b: l.upload(&format!("{p}.{}.bias", names.fc1), &[config.encoder_ffn_dim])?,
                fc2_w: l.upload(
                    &format!("{p}.{}.weight", names.fc2),
                    &[d, config.encoder_ffn_dim],
                )?,
                fc2_b: l.upload(&format!("{p}.{}.bias", names.fc2), &[d])?,
            });
        }

        let mut dec_layers = Vec::with_capacity(config.decoder_layers);
        for i in 0..config.decoder_layers {
            let p = format!("{}.{i}", names.dec_block);
            dec_layers.push(DecoderLayer {
                self_attn_ln: l.layer_norm(&format!("{p}.{}", names.self_attn_ln), d)?,
                self_attn: l.attention(&format!("{p}.{}", names.self_attn), d)?,
                cross_attn_ln: l.layer_norm(&format!("{p}.{}", names.cross_attn_ln), d)?,
                cross_attn: l.attention(&format!("{p}.{}", names.cross_attn), d)?,
                final_ln: l.layer_norm(&format!("{p}.{}", names.mlp_ln), d)?,
                fc1_w: l.upload(
                    &format!("{p}.{}.weight", names.fc1),
                    &[config.decoder_ffn_dim, d],
                )?,
                fc1_b: l.upload(&format!("{p}.{}.bias", names.fc1), &[config.decoder_ffn_dim])?,
                fc2_w: l.upload(
                    &format!("{p}.{}.weight", names.fc2),
                    &[d, config.decoder_ffn_dim],
                )?,
                fc2_b: l.upload(&format!("{p}.{}.bias", names.fc2), &[d])?,
            });
        }

        // OpenAI does not store the encoder positions; they are fixed sinusoids
        // and a loader that only looks them up finds nothing.
        let enc_pos_host = match names.enc_pos {
            Some(name) => tensor_f32(&st, name)?,
            None => sinusoids(config.max_source_positions, d)?,
        };

        Ok(WhisperWeights {
            conv1_w: l.upload_conv(&format!("{}.weight", names.conv1), d, mels, 3)?,
            conv1_b: l.upload(&format!("{}.bias", names.conv1), &[d])?,
            conv2_w: l.upload_conv(&format!("{}.weight", names.conv2), d, d, 3)?,
            conv2_b: l.upload(&format!("{}.bias", names.conv2), &[d])?,
            enc_pos_host,
            enc_ln: l.layer_norm(names.enc_ln, d)?,
            tok_emb: l.upload(names.tok_emb, &[config.vocab_size, d])?,
            dec_pos: l.upload(names.dec_pos, &[config.max_target_positions, d])?,
            dec_ln: l.layer_norm(names.dec_ln, d)?,
            enc_layers,
            dec_layers,
            config,
            generation,
        })
    }
}
