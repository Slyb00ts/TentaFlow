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

#[cfg(feature = "inference-supertonic")]
pub mod supertonic;

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
    /// Voice preset (np. `M1`/`F2` dla Supertonic, `af_heart` dla Kokoro).
    /// `None` => silnik bierze swoj domyslny glos. Niezalezne od `speaker_id`,
    /// ktore VITS Piper uzywa jako numeryczny indeks mowcy.
    pub voice: Option<String>,
    /// Jezyk syntezy (ISO-639-1: "pl", "en", ...). Multilingual silniki
    /// (Supertonic) owijaja tekst tagiem jezyka; jednojezyczne ignoruja.
    pub language: Option<String>,
}

impl Default for SynthesizeParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            speaker_id: 0,
            speed: 1.0,
            voice: None,
            language: None,
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

/// Laduje embedded silnik TTS do `shared_tts_manager()` pod kluczem
/// `engine_id` (== manifest `engine.id`) — tego samego klucza uzywa
/// `ModelRuntimeExecutor::execute_tts` w `synthesize`. Idempotentne: gdy
/// silnik juz zarejestrowany, no-op (tani read-check przed pobraniem modelu).
///
/// Wolane przy deploy (`EmbeddedDeploy::prepare_embedded_tts`) oraz leniwie
/// przez executor, gdy po restarcie procesu `TtsManager` startuje pusty mimo
/// `status=running` uslugi. `model_repo` to HF repo (sherpa/kokoro); dla
/// apple-tts to logiczny voice id (glosy systemowe, bez pobierania).
// Gdy build nie ma zadnego embedded TTS engine (brak inference-sherpa /
// inference-mlx-kokoro i nie macOS/iOS), `match` ma tylko arm `other =>
// bail!`, ktory diverguje — caly load-body jest wtedy legalnie martwy, a
// `model_repo` nieuzywany. Allow zawezony dokladnie do tego configu.
#[cfg_attr(
    not(any(
        feature = "inference-sherpa",
        feature = "inference-mlx-kokoro",
        feature = "inference-supertonic",
        target_os = "macos",
        target_os = "ios"
    )),
    allow(unreachable_code, unused_variables)
)]
pub async fn ensure_embedded_engine_loaded(
    engine_id: &str,
    model_repo: &str,
    // Wybor voice z wielogłosowego repo: sherpa (`<voice>.onnx`) oraz
    // supertonic (preset `M1`/`F2`).
    #[cfg_attr(
        not(any(feature = "inference-sherpa", feature = "inference-supertonic")),
        allow(unused_variables)
    )]
    voice_hint: Option<&str>,
) -> anyhow::Result<()> {
    if shared_tts_manager().read().await.has(engine_id) {
        return Ok(());
    }

    let engine: Box<dyn TtsEngine> = match engine_id {
        #[cfg(feature = "inference-sherpa")]
        "sherpa-onnx" => {
            use anyhow::Context;
            let dir = sherpa::prepare_model(model_repo)
                .await
                .with_context(|| format!("prepare sherpa model '{model_repo}'"))?;
            let mut e = sherpa::SherpaTtsEngine::new();
            // Wielogłosowe repo (np. WitoldG/polish_piper_models) — wybierz voice
            // pasujacy do presetu, inaczej `load_model` bierze pierwszy z dysku.
            e.set_voice_hint(voice_hint);
            <sherpa::SherpaTtsEngine as TtsEngine>::load_model(&mut e, &dir)
                .context("load sherpa-onnx VITS model")?;
            Box::new(e)
        }
        #[cfg(feature = "inference-mlx-kokoro")]
        "kokoro" => {
            use anyhow::Context;
            let dir = mlx_kokoro::prepare_model(model_repo)
                .await
                .with_context(|| format!("prepare kokoro model '{model_repo}'"))?;
            let mut e = mlx_kokoro::MlxKokoroEngine::new();
            <mlx_kokoro::MlxKokoroEngine as TtsEngine>::load_model(&mut e, &dir)
                .context("load kokoro")?;
            Box::new(e)
        }
        #[cfg(feature = "inference-supertonic")]
        "supertonic" => {
            use anyhow::Context;
            let dir = supertonic::prepare_model(model_repo)
                .await
                .with_context(|| format!("prepare supertonic model '{model_repo}'"))?;
            let mut e = supertonic::SupertonicTtsEngine::new();
            // Voice preset (M1/F2/...) z presetu deployu — bez podpowiedzi
            // silnik bierze pierwszy dostepny voice_style z dysku.
            e.set_voice_hint(voice_hint);
            <supertonic::SupertonicTtsEngine as TtsEngine>::load_model(&mut e, &dir)
                .context("load supertonic ONNX model")?;
            Box::new(e)
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        "apple-tts" => {
            use anyhow::Context;
            let mut e = apple_tts::AppleTtsEngine::new();
            <apple_tts::AppleTtsEngine as TtsEngine>::load_model(
                &mut e,
                std::path::Path::new("apple-tts"),
            )
            .context("init apple-tts (brak libMLXBridge.dylib?)")?;
            Box::new(e)
        }
        other => anyhow::bail!(
            "embedded TTS engine '{other}' nie jest dostepny w tym buildzie \
             (brak loadera lub wymaganego feature flag — np. inference-sherpa)"
        ),
    };

    let mgr = shared_tts_manager();
    let mut guard = mgr.write().await;
    // Double-check pod write lockiem — inny task mogl zarejestrowac w trakcie
    // pobierania/ladowania modelu.
    if !guard.has(engine_id) {
        guard.register(engine_id.to_string(), engine);
    }
    Ok(())
}
