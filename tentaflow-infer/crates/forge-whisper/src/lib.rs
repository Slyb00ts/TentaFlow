// ===== File: lib.rs — forge-whisper: Whisper speech-to-text over FORGE HAL/kernels =====
// v0 scope: one 30 s window per request (longer audio is truncated), greedy
// decoding, single sequence. Encoder linears run as one GEMV launch per
// position over a shared stream — correct first; batched GEMM is the known
// optimization. All device memory (weights AND scratch) comes from the
// weights pool so the engine never interacts with the activations ring of a
// co-resident LLM engine.

pub mod audio;
pub mod mel;
pub mod weights;

mod decoder;
mod encoder;

use std::path::Path;
use std::sync::Arc;

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_kernels::Kernels;
use forge_tokenize::Tokenizer;
use forge_types::{ForgeError, MemKind, Result};

use weights::WhisperWeights;

/// LayerNorm epsilon used by HF Whisper (nn.LayerNorm default).
const LN_EPS: f32 = 1e-5;

/// Device scratch reused across requests; sized once at load for the maximum
/// window (1500 encoder positions, max_target_positions decoder steps).
struct Scratch {
    mel: DevBuffer,
    conv1_out: DevBuffer,
    conv2_out: DevBuffer,
    /// Encoder residual stream [T, d].
    enc_h: DevBuffer,
    /// Encoder normed sublayer input [T, d].
    enc_x: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn: DevBuffer,
    /// Sublayer output before the residual add [T, d].
    proj: DevBuffer,
    ffn: DevBuffer,
    enc_states: DevBuffer,
    /// Per-decoder-layer cross-attention K/V over encoder states [T, d].
    cross_k: Vec<DevBuffer>,
    cross_v: Vec<DevBuffer>,
    /// Per-decoder-layer self-attention K/V caches [max_target, d].
    self_k: Vec<DevBuffer>,
    self_v: Vec<DevBuffer>,
    dec_h: DevBuffer,
    dec_x: DevBuffer,
    dec_q: DevBuffer,
    dec_attn: DevBuffer,
    dec_o: DevBuffer,
    dec_ffn: DevBuffer,
    pos_row: DevBuffer,
    /// f32 logits [vocab_size].
    logits: DevBuffer,
    /// i32 [1] token id for embedding gathers.
    ids: DevBuffer,
    /// i32 [1] position id for positional-embedding gathers.
    pos_ids: DevBuffer,
}

pub struct WhisperModel {
    device: Arc<dyn Device>,
    kernels: Kernels,
    weights: WhisperWeights,
    tokenizer: Tokenizer,
    stream: Stream,
    scratch: Scratch,
    sot: u32,
    /// `Some` on multilingual checkpoints only; `None` marks an English-only
    /// model prompted without language/task tokens.
    transcribe_task: Option<u32>,
    no_timestamps: u32,
    suppress: Vec<u32>,
    begin_suppress: Vec<u32>,
}

impl WhisperModel {
    pub fn load(device: Arc<dyn Device>, dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let kernels = Kernels::load(device.clone())?;
        let weights = WhisperWeights::load(device.as_ref(), dir)?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))?;

        let special = |name: &str| {
            tokenizer.token_to_id(name).ok_or_else(|| {
                ForgeError::Format(format!("whisper tokenizer: missing special token {name}"))
            })
        };
        let sot = special("<|startoftranscript|>")?;
        let no_timestamps = special("<|notimestamps|>")?;

        let cfg = &weights.config;
        let d = cfg.d_model;
        let t_enc = cfg.max_source_positions;
        let t_in = t_enc * 2;
        let t_dec = cfg.max_target_positions;
        let ffn = cfg.encoder_ffn_dim.max(cfg.decoder_ffn_dim);
        let n_dec = cfg.decoder_layers;

        let f16buf = |elems: usize| device.alloc(elems * 2, MemKind::Device, Pool::Weights);
        let per_layer = |elems: usize| -> Result<Vec<DevBuffer>> {
            (0..n_dec).map(|_| f16buf(elems)).collect()
        };

        let scratch = Scratch {
            mel: f16buf(cfg.num_mel_bins * t_in)?,
            conv1_out: f16buf(d * t_in)?,
            conv2_out: f16buf(d * t_enc)?,
            enc_h: f16buf(t_enc * d)?,
            enc_x: f16buf(t_enc * d)?,
            q: f16buf(t_enc * d)?,
            k: f16buf(t_enc * d)?,
            v: f16buf(t_enc * d)?,
            attn: f16buf(t_enc * d)?,
            proj: f16buf(t_enc * d)?,
            ffn: f16buf(t_enc * ffn)?,
            enc_states: f16buf(t_enc * d)?,
            cross_k: per_layer(t_enc * d)?,
            cross_v: per_layer(t_enc * d)?,
            self_k: per_layer(t_dec * d)?,
            self_v: per_layer(t_dec * d)?,
            dec_h: f16buf(d)?,
            dec_x: f16buf(d)?,
            dec_q: f16buf(d)?,
            dec_attn: f16buf(d)?,
            dec_o: f16buf(d)?,
            dec_ffn: f16buf(ffn)?,
            pos_row: f16buf(d)?,
            logits: device.alloc(cfg.vocab_size * 4, MemKind::Device, Pool::Weights)?,
            ids: device.alloc(4, MemKind::Device, Pool::Weights)?,
            pos_ids: device.alloc(4, MemKind::Device, Pool::Weights)?,
        };

        let stream = device.create_stream()?;
        let suppress = weights.generation.suppress_tokens.clone();
        let begin_suppress = weights.generation.begin_suppress_tokens.clone();

        // English-only (.en) checkpoints are prompted [sot, notimestamps]
        // with no language/task tokens. Their tokenizers still CONTAIN the
        // language tokens, so token presence is not a valid signal; the
        // generation config carries `is_multilingual` (older exports omit it,
        // where the vocabulary size disambiguates: 51864 = English-only,
        // 51865+ = multilingual).
        let multilingual = weights
            .generation
            .is_multilingual
            .unwrap_or(weights.config.vocab_size >= 51_865);
        let transcribe_task = if multilingual {
            Some(special("<|transcribe|>")?)
        } else {
            None
        };
        Ok(Self {
            device,
            kernels,
            weights,
            tokenizer,
            stream,
            scratch,
            sot,
            transcribe_task,
            no_timestamps,
            suppress,
            begin_suppress,
        })
    }

    /// Resolve a language code ("en", "pl", …) to its Whisper language token.
    fn language_token(&self, language: Option<&str>) -> Result<u32> {
        let lang = language.unwrap_or("en");
        self.tokenizer
            .token_to_id(&format!("<|{lang}|>"))
            .ok_or_else(|| {
                ForgeError::Unsupported(format!("whisper: unknown language code {lang:?}"))
            })
    }

    /// Transcribe 16 kHz mono samples. v0: a single 30 s window — longer
    /// input is truncated by the mel frontend.
    pub fn transcribe(
        &mut self,
        samples_16k_mono: &[f32],
        language: Option<&str>,
    ) -> Result<String> {
        if samples_16k_mono.is_empty() {
            return Err(ForgeError::Format("whisper: empty audio".into()));
        }
        let prompt: Vec<u32> = match self.transcribe_task {
            Some(task) => vec![
                self.sot,
                self.language_token(language)?,
                task,
                self.no_timestamps,
            ],
            None => {
                if let Some(lang) = language {
                    tracing::warn!("whisper: language {lang:?} ignored — English-only checkpoint");
                }
                vec![self.sot, self.no_timestamps]
            }
        };
        let features = mel::log_mel_spectrogram(samples_16k_mono, self.weights.config.num_mel_bins);
        self.encode(&features)?;

        let tokens = self.greedy_decode(&prompt)?;
        let text = self.tokenizer.decode(&tokens, true)?;
        Ok(text.trim().to_string())
    }
}
