// =============================================================================
// Plik: tts/mod.rs
// Opis: Embedded TTS engines — wkompilowane bezposrednio w binarke przez
//       Cargo features. Aktualnie tylko `inference-sherpa` (sherpa-onnx
//       VITS Piper). Symetria do `inference/` (LLM) i `stt/` (Whisper).
// =============================================================================

#[cfg(feature = "inference-sherpa")]
pub mod sherpa;

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Informacje o zaladowanym modelu TTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsModelInfo {
    pub name: String,
    pub backend: String,
    pub sample_rate: u32,
    pub speakers: u32,
}

/// Wynik syntezy: surowe sample float32 + sample rate. Caller (FastAPI/
/// SSE/QUIC) konwertuje do WAV/PCM/Opus wedlug zapotrzebowania.
#[derive(Debug, Clone)]
pub struct SynthesizeResult {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Parametry syntezy. `speaker_id` dla modeli multi-speaker (VITS Piper
/// czesto ma 1, niektore np. cmu-arctic maja kilkadziesiat). `speed` to
/// tempo (1.0 = normalne, 0.5 = 2x wolniej, 2.0 = 2x szybciej).
#[derive(Debug, Clone)]
pub struct SynthesizeParams {
    pub text: String,
    pub speaker_id: i32,
    pub speed: f32,
}

impl Default for SynthesizeParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            speaker_id: 0,
            speed: 1.0,
        }
    }
}

/// Trait dla embedded TTS engines.
pub trait TtsEngine: Send + Sync {
    fn backend_name(&self) -> &str;
    fn load_model(&mut self, model_dir: &Path) -> anyhow::Result<TtsModelInfo>;
    fn synthesize(&self, params: SynthesizeParams) -> anyhow::Result<SynthesizeResult>;
    fn model_info(&self) -> Option<&TtsModelInfo>;
}
