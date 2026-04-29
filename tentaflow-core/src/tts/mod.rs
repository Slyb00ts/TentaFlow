// =============================================================================
// Plik: tts/mod.rs
// Opis: Embedded TTS engines — wkompilowane bezposrednio w binarke przez
//       Cargo features. Symetria do `inference/` (LLM) i `stt/` (Whisper).
//
//       Aktualnie wspierane:
//         - `inference-sherpa` (sherpa-onnx VITS Piper)
//         - apple-tts (AVSpeechSynthesizer, ZAWSZE na macOS/iOS — bez feature flag)
//         - `inference-mlx-kokoro` (Kokoro 82M przez mlx-swift, macOS/iOS)
// =============================================================================

/// Cache regul czyszczenia TTS (emoji strip + reguly z `tts_cleaning_rules`).
/// Modul niezalezny od backendu — uzywany przez routing/tts.rs przed dispatch
/// oraz przez flow_engine adapter `tts_clean`.
pub mod clean_cache;

#[cfg(feature = "inference-sherpa")]
pub mod sherpa;

// Apple TTS jest ZAWSZE skompilowany na macOS/iOS — bez feature flag.
// AVSpeechSynthesizer to systemowy silnik, nie wymaga zewnetrznych deps,
// uzytkownik nie ma jak go wylaczyc i nie powinien.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple_tts;

#[cfg(feature = "inference-mlx-kokoro")]
pub mod mlx_kokoro;

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

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

// =============================================================================
// TtsManager — analog SttManager. Trzyma zarejestrowane engine'y po nazwie i
// pozwala routerowi syntezowac przez wybrany backend.
// =============================================================================

static SHARED_TTS: std::sync::OnceLock<Arc<RwLock<TtsManager>>> = std::sync::OnceLock::new();

pub fn shared_tts_manager() -> Arc<RwLock<TtsManager>> {
    SHARED_TTS
        .get_or_init(|| Arc::new(RwLock::new(TtsManager::new())))
        .clone()
}

/// Manager wszystkich embedded silnikow TTS. Klucz = backend_name (z manifestu
/// `engine.id`). Rejestracja przez `register(name, engine)`; deploy handler
/// w `deploy/runner.rs` woła `register` + `load_model` przy embedded native deploy.
pub struct TtsManager {
    engines: std::collections::HashMap<String, Box<dyn TtsEngine>>,
}

impl TtsManager {
    pub fn new() -> Self {
        Self {
            engines: std::collections::HashMap::new(),
        }
    }

    pub fn register(&mut self, name: impl Into<String>, engine: Box<dyn TtsEngine>) {
        self.engines.insert(name.into(), engine);
    }

    pub fn unregister(&mut self, name: &str) {
        self.engines.remove(name);
    }

    pub fn has(&self, name: &str) -> bool {
        self.engines.contains_key(name)
    }

    pub fn list(&self) -> Vec<String> {
        self.engines.keys().cloned().collect()
    }

    /// Wybiera silnik po `engine_id` i wykonuje synteze. Jezeli silnik nie
    /// jest zarejestrowany, zwraca blad — caller (router) moze wtedy
    /// fallbackowac na zewnetrzny QUIC TTS sidecar.
    pub fn synthesize(
        &self,
        engine_id: &str,
        params: SynthesizeParams,
    ) -> anyhow::Result<SynthesizeResult> {
        let engine = self
            .engines
            .get(engine_id)
            .ok_or_else(|| anyhow::anyhow!("TTS engine '{}' nie zarejestrowany", engine_id))?;
        engine.synthesize(params)
    }

    pub fn model_info(&self, engine_id: &str) -> Option<TtsModelInfo> {
        self.engines
            .get(engine_id)
            .and_then(|e| e.model_info().cloned())
    }
}

impl Default for TtsManager {
    fn default() -> Self {
        Self::new()
    }
}
