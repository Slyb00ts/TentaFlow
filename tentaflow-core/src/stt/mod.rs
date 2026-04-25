// =============================================================================
// Plik: stt/mod.rs
// Opis: Rozpoznawanie mowy (Speech-to-Text) — trait SttEngine i manager.
// =============================================================================

pub mod audio;
#[cfg(feature = "inference-whisper")]
pub mod whisper;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

/// Globalny wspoldzielony SttManager — singleton per proces.
static SHARED_STT: std::sync::OnceLock<Arc<RwLock<SttManager>>> = std::sync::OnceLock::new();

/// Zwraca globalna instancje SttManager (tworzy przy pierwszym uzyciu)
pub fn shared_stt_manager() -> Arc<RwLock<SttManager>> {
    SHARED_STT
        .get_or_init(|| Arc::new(RwLock::new(SttManager::new())))
        .clone()
}

/// Informacje o zaladowanym modelu STT
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttModelInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub model_type: String,
    pub backend: String,
    pub loaded: bool,
    pub device: String,
}

/// Parametry transkrypcji audio
#[derive(Debug, Clone)]
pub struct TranscribeParams {
    /// Surowe dane audio (WAV/PCM/itp.)
    pub audio_data: Vec<u8>,
    /// Jezyk zrodlowy (None = auto-detekcja)
    pub language: Option<String>,
    /// Tlumacz na angielski
    pub translate: bool,
    /// Generuj znaczniki czasowe per slowo
    pub word_timestamps: bool,
    /// Temperatura samplowania (None = domyslna)
    pub temperature: Option<f32>,
    /// Prog braku mowy — segmenty powyzej sa pomijane
    pub no_speech_threshold: Option<f32>,
    /// Poczatkowy prompt (kontekst dla dekodera)
    pub initial_prompt: Option<String>,
}

impl Default for TranscribeParams {
    fn default() -> Self {
        Self {
            audio_data: Vec::new(),
            language: None,
            translate: false,
            word_timestamps: false,
            temperature: None,
            no_speech_threshold: None,
            initial_prompt: None,
        }
    }
}

/// Wynik transkrypcji
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResult {
    pub text: String,
    pub language: String,
    pub duration_seconds: f64,
    pub segments: Vec<TranscribeSegment>,
}

/// Pojedynczy segment transkrypcji
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeSegment {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub no_speech_prob: f32,
    pub avg_logprob: f32,
    pub compression_ratio: f32,
    pub tokens: Vec<i32>,
}

/// Fragment transkrypcji w trybie streamingu
#[derive(Debug, Clone)]
pub struct TranscribeChunk {
    pub text: String,
    pub is_final: bool,
    pub segment: Option<TranscribeSegment>,
}

/// Interfejs silnika STT — implementowany przez backendy (Whisper, itp.)
#[async_trait]
pub trait SttEngine: Send + Sync {
    /// Nazwa backendu ("whisper", itp.)
    fn backend_name(&self) -> &str;

    /// Lista obslugiwanych formatow audio
    fn supported_formats(&self) -> Vec<String>;

    /// Zaladuj model z podanej sciezki
    async fn load_model(
        &self,
        model_path: &Path,
        device: Option<&str>,
    ) -> anyhow::Result<SttModelInfo>;

    /// Wyladuj model z pamieci
    async fn unload_model(&self) -> anyhow::Result<()>;

    /// Informacje o zaladowanym modelu (None jesli nie zaladowany)
    fn model_info(&self) -> Option<SttModelInfo>;

    /// Czy model jest zaladowany?
    fn is_loaded(&self) -> bool {
        self.model_info().map(|m| m.loaded).unwrap_or(false)
    }

    /// Transkrybuj audio (blokujace — czeka na caly wynik)
    async fn transcribe(&self, params: TranscribeParams) -> anyhow::Result<TranscribeResult>;

    /// Transkrybuj audio ze streamingiem (zwraca kanal z fragmentami)
    async fn transcribe_stream(
        &self,
        params: TranscribeParams,
    ) -> anyhow::Result<mpsc::Receiver<TranscribeChunk>>;
}

/// Model Whisper do pobrania z HuggingFace (tylko large-v3-turbo — reszta za slaba)
pub const WHISPER_MODEL_NAME: &str = "large-v3-turbo";
pub const WHISPER_MODEL_FILENAME: &str = "ggml-large-v3-turbo.bin";
pub const WHISPER_MODEL_SIZE: u64 = 1_600_000_000;

const WHISPER_HF_REPO: &str = "ggerganov/whisper.cpp";

/// Status modelu Whisper (pobrany / zaladowany)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperModelStatus {
    pub name: String,
    pub filename: String,
    pub size_bytes: u64,
    pub downloaded: bool,
    pub loaded: bool,
}

/// Manager silnikow STT — wybiera odpowiedni backend
pub struct SttManager {
    engines: Vec<Box<dyn SttEngine>>,
    active_engine: Option<usize>,
}

impl SttManager {
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut engines: Vec<Box<dyn SttEngine>> = Vec::new();

        #[cfg(feature = "inference-whisper")]
        engines.push(Box::new(whisper::WhisperEngine::new()));

        Self {
            engines,
            active_engine: None,
        }
    }

    /// Lista dostepnych backendow
    pub fn available_backends(&self) -> Vec<String> {
        self.engines
            .iter()
            .map(|e| e.backend_name().to_string())
            .collect()
    }

    /// Aktywny silnik (jesli model zaladowany)
    pub fn active_engine(&self) -> Option<&dyn SttEngine> {
        self.active_engine
            .and_then(|i| self.engines.get(i).map(|e| e.as_ref()))
    }

    /// Zaladuj model — automatycznie wybierze backend
    pub async fn load_model(
        &mut self,
        model_path: &Path,
        device: Option<&str>,
        preferred_backend: Option<&str>,
    ) -> anyhow::Result<SttModelInfo> {
        let engine_idx = if let Some(backend) = preferred_backend {
            self.engines
                .iter()
                .position(|e| e.backend_name() == backend)
                .ok_or_else(|| anyhow::anyhow!("Backend '{}' nie jest dostepny", backend))?
        } else {
            self.engines
                .iter()
                .position(|_| true)
                .ok_or_else(|| anyhow::anyhow!("Brak dostepnego backendu STT"))?
        };

        let info = self.engines[engine_idx]
            .load_model(model_path, device)
            .await?;
        self.active_engine = Some(engine_idx);
        Ok(info)
    }

    /// Wyladuj model
    pub async fn unload_model(&mut self) -> anyhow::Result<()> {
        if let Some(idx) = self.active_engine {
            self.engines[idx].unload_model().await?;
            self.active_engine = None;
        }
        Ok(())
    }

    /// Katalog cache dla modeli Whisper
    pub fn whisper_models_dir() -> PathBuf {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tentaflow")
            .join("models")
            .join("whisper");
        std::fs::create_dir_all(&base).ok();
        base
    }

    /// Status modelu Whisper (pobrany / zaladowany)
    pub fn whisper_model_status(&self) -> WhisperModelStatus {
        let models_dir = Self::whisper_models_dir();
        let path = models_dir.join(WHISPER_MODEL_FILENAME);
        let downloaded = path.exists();
        let loaded = self
            .active_engine
            .and_then(|idx| self.engines.get(idx))
            .and_then(|e| e.model_info())
            .map(|info| info.path == path.to_string_lossy().as_ref())
            .unwrap_or(false);

        WhisperModelStatus {
            name: WHISPER_MODEL_NAME.to_string(),
            filename: WHISPER_MODEL_FILENAME.to_string(),
            size_bytes: WHISPER_MODEL_SIZE,
            downloaded,
            loaded,
        }
    }

    /// Pobierz model Whisper z HF (jesli brak w cache) i zaladuj go
    pub async fn ensure_and_load(&mut self, device: Option<&str>) -> anyhow::Result<SttModelInfo> {
        let filename = WHISPER_MODEL_FILENAME;
        let models_dir = Self::whisper_models_dir();
        let model_path = models_dir.join(filename);

        if !model_path.exists() {
            info!(
                "Pobieranie modelu Whisper '{}' z HuggingFace...",
                WHISPER_MODEL_NAME
            );
            let repo_id = WHISPER_HF_REPO.to_string();
            let fname = filename.to_string();
            let hf_path = tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
                let api = hf_hub::api::sync::Api::new()?;
                let repo = api.model(repo_id);
                let path = repo.get(&fname)?;
                Ok(path)
            })
            .await??;
            std::fs::copy(&hf_path, &model_path)?;
            info!(
                "Model Whisper '{}' pobrany: {:?}",
                WHISPER_MODEL_NAME, model_path
            );
        }

        self.load_model(&model_path, device, None).await
    }
}
