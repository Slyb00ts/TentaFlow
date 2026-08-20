// =============================================================================
// Plik: stt/mod.rs
// Opis: Rozpoznawanie mowy (Speech-to-Text) — trait SttEngine i manager.
// =============================================================================

pub mod audio;
#[cfg(feature = "inference-mlx-whisper")]
pub mod mlx_whisper;
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
    /// Surowe dane audio (WAV/PCM/itp.). Arc<[u8]> dzielony z routerem
    /// zeby uniknac kopiowania bufora przy kazdym przekazaniu do silnika.
    pub audio_data: Arc<[u8]>,
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
            audio_data: Arc::from(Vec::<u8>::new().into_boxed_slice()),
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
    /// Language the engine actually decoded with: explicit request language or
    /// the auto-detected one. `None` when the engine cannot tell — consumers
    /// must NOT substitute a placeholder ("auto" would leak into TTS).
    pub language: Option<String>,
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

/// Snapshot deploy-time parametrow dla STT enginow. Ustawiane raz przy
/// `load_model`, czytane potem przez `transcribe` jako baseline gdy
/// `TranscribeParams` per-request nie ma wartosci.
#[derive(Debug, Default, Clone)]
pub struct WhisperDeployParams {
    pub n_threads: Option<i32>,
    pub default_beam_size: Option<i32>,
    pub default_language: Option<String>,
    pub default_translate: Option<bool>,
    // Indeks karty GPU dla whisper.cpp (single-device). Wybor kart z kreatora
    // bierze pierwsza wybrana karte — embedded nie reaguje na CUDA_VISIBLE_DEVICES.
    pub gpu_device: i32,
}

impl WhisperDeployParams {
    /// Zbuduj z `app.whisper` mapy z `apply_parameters_deploy`. Klucze:
    /// `n_threads`, `default_beam_size`, `default_language`, `default_translate`.
    pub fn from_json_map(map: &std::collections::HashMap<String, serde_json::Value>) -> Self {
        let mut p = Self::default();
        if let Some(v) = map.get("n_threads").and_then(|v| v.as_i64()) {
            p.n_threads = Some(v as i32);
        }
        if let Some(v) = map.get("default_beam_size").and_then(|v| v.as_i64()) {
            p.default_beam_size = Some(v as i32);
        }
        if let Some(v) = map.get("default_language").and_then(|v| v.as_str()) {
            p.default_language = Some(v.to_string());
        }
        if let Some(v) = map.get("default_translate").and_then(|v| v.as_bool()) {
            p.default_translate = Some(v);
        }
        p
    }
}

/// Interfejs silnika STT — implementowany przez backendy (Whisper, itp.)
#[async_trait]
pub trait SttEngine: Send + Sync {
    /// Nazwa backendu ("whisper", itp.)
    fn backend_name(&self) -> &str;

    /// Lista obslugiwanych formatow audio
    fn supported_formats(&self) -> Vec<String>;

    /// Zaladuj model z podanej sciezki. `deploy_params` niesie typed
    /// load-time defaults (n_threads dla whisper.cpp) plus request-time
    /// fallback (default_beam_size/language/translate uzywane gdy
    /// `TranscribeParams` per-call nie podala wartosci).
    async fn load_model(
        &self,
        model_path: &Path,
        device: Option<&str>,
        deploy_params: &WhisperDeployParams,
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

/// HF repo hosting whisper.cpp GGML conversions of every `openai/whisper-*`
/// checkpoint under the `ggml-<variant>.bin` naming.
const WHISPER_CPP_GGML_REPO: &str = "ggerganov/whisper.cpp";

/// Source of the GGML file for the whisper.cpp engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgmlSource {
    pub repo: String,
    pub filename: String,
}

/// Maps the deploy's `(model_repo, model_file)` onto the GGML file whisper.cpp
/// loads. An explicit `model_file` is taken verbatim from `model_repo`; an
/// `openai/whisper-<variant>` preset resolves to the whisper.cpp conversion
/// of that variant. Anything else has no GGML artifact we know how to fetch.
pub fn resolve_ggml_source(
    model_repo: &str,
    model_file: Option<&str>,
) -> anyhow::Result<GgmlSource> {
    let repo = model_repo.trim();
    if repo.is_empty() {
        anyhow::bail!("whisper.cpp deploy has no model_repo");
    }
    if let Some(file) = model_file.map(str::trim).filter(|f| !f.is_empty()) {
        // The name is appended to a cache directory, so it has to be one plain
        // component: `components()` collapses separators, `..`, `.` and drive
        // prefixes that a substring check would miss on Windows paths.
        let path = std::path::Path::new(file);
        if path.is_absolute() || path.components().count() != 1 {
            anyhow::bail!("model_file '{file}' must be a bare file name");
        }
        return Ok(GgmlSource {
            repo: repo.to_string(),
            filename: file.to_string(),
        });
    }
    match repo.strip_prefix("openai/whisper-") {
        Some(variant) if !variant.is_empty() => Ok(GgmlSource {
            repo: WHISPER_CPP_GGML_REPO.to_string(),
            filename: format!("ggml-{variant}.bin"),
        }),
        _ => anyhow::bail!(
            "whisper.cpp cannot resolve a GGML file for '{repo}': pick an openai/whisper-* \
             preset or set model_file to the .bin name inside that repo"
        ),
    }
}

/// Manager silnikow STT — wybiera odpowiedni backend.
/// **Singleton invariant:** jeden active embedded STT engine per host
/// (jak `InferenceManager`). Deploy drugiego embedded STT podmienia
/// active_engine + active_deploy_params.
pub struct SttManager {
    engines: Vec<Box<dyn SttEngine>>,
    active_engine: Option<usize>,
    active_deploy_params: WhisperDeployParams,
}

impl SttManager {
    /// Manager over an explicit engine list (tests plug in fake engines).
    pub fn with_engines(engines: Vec<Box<dyn SttEngine>>) -> Self {
        Self {
            engines,
            active_engine: None,
            active_deploy_params: WhisperDeployParams::default(),
        }
    }

    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut engines: Vec<Box<dyn SttEngine>> = Vec::new();

        #[cfg(feature = "inference-whisper")]
        engines.push(Box::new(whisper::WhisperEngine::new()));

        // mlx-whisper PRZED whisper.cpp — gdy uzytkownik ma macOS i wybral
        // engine "mlx-whisper" przez preferred_backend, zostanie znaleziony.
        // Bez preferred_backend nadal idziemy na pierwszy (whisper.cpp), zeby
        // istniejaca konfiguracja nie zmienila zachowania niezauwazenie.
        #[cfg(feature = "inference-mlx-whisper")]
        engines.push(Box::new(mlx_whisper::MlxWhisperEngine::new()));

        Self::with_engines(engines)
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

    /// Zaladuj model — automatycznie wybierze backend.
    /// `deploy_params` niesie typed defaults (n_threads dla whisper.cpp,
    /// per-request fallbacki dla beam_size/language/translate).
    pub async fn load_model(
        &mut self,
        model_path: &Path,
        device: Option<&str>,
        preferred_backend: Option<&str>,
        deploy_params: WhisperDeployParams,
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
            .load_model(model_path, device, &deploy_params)
            .await?;
        self.active_engine = Some(engine_idx);
        self.active_deploy_params = deploy_params;
        Ok(info)
    }

    /// Snapshot aktualnych deploy params. `transcribe` callerzy moga je
    /// przeczytac jezeli chca dodac defaulty na poziomie wyzszym (np.
    /// HTTP handler lub flow adapter). WhisperEngine sam takze ich uzywa
    /// jako fallback dla per-call `TranscribeParams`.
    pub fn deploy_params(&self) -> &WhisperDeployParams {
        &self.active_deploy_params
    }

    /// Wyladuj model. Czysci tez `active_deploy_params`.
    pub async fn unload_model(&mut self) -> anyhow::Result<()> {
        if let Some(idx) = self.active_engine {
            self.engines[idx].unload_model().await?;
            self.active_engine = None;
            self.active_deploy_params = WhisperDeployParams::default();
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

    /// Downloads (when not cached) and loads the model of an embedded STT
    /// service row. `engine_id` and `model_repo` come from the deploy
    /// (manifest engine id + selected preset / user repo); nothing here has a
    /// built-in default model. `mlx-whisper` fetches the MLX directory
    /// (`config.json` + safetensors + tokenizer) via `prepare_model`;
    /// `whisper` fetches the GGML file resolved by `resolve_ggml_source`.
    /// `log_sink` reports progress to the deploy window.
    pub async fn ensure_and_load(
        &mut self,
        engine_id: &str,
        model_repo: &str,
        model_file: Option<&str>,
        device: Option<&str>,
        log_sink: Option<&crate::services::deploy::LogSink>,
        deploy_params: WhisperDeployParams,
    ) -> anyhow::Result<SttModelInfo> {
        match engine_id {
            #[cfg(feature = "inference-mlx-whisper")]
            "mlx-whisper" => {
                let repo = model_repo.trim();
                if repo.is_empty() {
                    anyhow::bail!("mlx-whisper deploy has no model_repo");
                }
                let dir = mlx_whisper::prepare_model(repo, log_sink).await?;
                self.load_model(&dir, device, Some("mlx-whisper"), deploy_params)
                    .await
            }
            "whisper" => {
                let source = resolve_ggml_source(model_repo, model_file)?;
                let model_path = Self::whisper_models_dir().join(&source.filename);
                if !model_path.exists() {
                    info!(
                        "Downloading whisper.cpp model {}/{} from HuggingFace...",
                        source.repo, source.filename
                    );
                    if let Some(s) = log_sink {
                        s.info(&format!(
                            "[stt] downloading {}/{}",
                            source.repo, source.filename
                        ));
                    }
                    let api = hf_hub::api::tokio::Api::new()?;
                    let repo = api.model(source.repo.clone());
                    let hf_path = repo.get(&source.filename).await?;
                    std::fs::copy(&hf_path, &model_path)?;
                    info!("whisper.cpp model downloaded: {:?}", model_path);
                }
                self.load_model(&model_path, device, Some("whisper"), deploy_params)
                    .await
            }
            other => anyhow::bail!(
                "engine '{other}' is not an embedded STT engine available in this build \
                 (available: {})",
                self.available_backends().join(", ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ggml_source_from_openai_preset() {
        let src = resolve_ggml_source("openai/whisper-large-v3-turbo", None).unwrap();
        assert_eq!(src.repo, WHISPER_CPP_GGML_REPO);
        assert_eq!(src.filename, "ggml-large-v3-turbo.bin");
        let src = resolve_ggml_source(" openai/whisper-base ", None).unwrap();
        assert_eq!(src.filename, "ggml-base.bin");
    }

    #[test]
    fn ggml_source_explicit_file_wins() {
        let src =
            resolve_ggml_source("ggerganov/whisper.cpp", Some("ggml-medium-q5_0.bin")).unwrap();
        assert_eq!(src.repo, "ggerganov/whisper.cpp");
        assert_eq!(src.filename, "ggml-medium-q5_0.bin");
        assert!(resolve_ggml_source("ggerganov/whisper.cpp", Some("../x.bin")).is_err());
    }

    #[test]
    fn ggml_source_rejects_unknown_repo_and_empty() {
        assert!(resolve_ggml_source("", None).is_err());
        assert!(resolve_ggml_source("openai/whisper-", None).is_err());
        assert!(resolve_ggml_source("someone/other-model", None).is_err());
    }
}
