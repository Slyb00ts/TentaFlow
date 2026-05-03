// =============================================================================
// Plik: inference/mod.rs
// Opis: Lokalna inferencja modeli LLM — trait InferenceEngine i manager.
// =============================================================================

#[cfg(feature = "inference-llamacpp")]
pub mod llamacpp;

// MLX inference — implementacja przez Swift bridge (mlx-swift / MLXLLM).
// Stary modul `mlx` (mlx-rs / mlx-models w Rust) zostal usuniety bo Rust port
// mial bug w 4-bit forward pass: Bielik / Qwen generowaly losowe tokeny.
#[cfg(feature = "inference-mlx")]
pub mod mlx_swift_bridge;

pub mod local;
pub mod model_manager;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Globalny wspoldzielony InferenceManager — singleton per proces.
/// Uzywany przez handle_deploy_ws_native do ladowania modeli
/// i przez Router (LocalInferenceHandler) do obslugi requestow in-process.
static SHARED_INFERENCE: std::sync::OnceLock<Arc<RwLock<InferenceManager>>> =
    std::sync::OnceLock::new();

/// Zwraca globalna instancje InferenceManager (tworzy przy pierwszym uzyciu)
pub fn shared_inference_manager() -> Arc<RwLock<InferenceManager>> {
    SHARED_INFERENCE
        .get_or_init(|| Arc::new(RwLock::new(InferenceManager::new())))
        .clone()
}

/// Informacje o zaladowanym modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub parameters: String,
    pub quantization: Option<String>,
    pub context_length: u32,
    pub loaded: bool,
    pub vram_used_mb: u64,
    pub backend: String,
    /// Wykryty szablon chatu — np. "chatml", "llama3", "plain"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<String>,
}

/// Parametry generowania tekstu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateParams {
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub repeat_penalty: f32,
    pub stop_sequences: Vec<String>,
    pub system_prompt: Option<String>,
}

impl Default for GenerateParams {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            max_tokens: 2048,
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            // 1.0 = no-op (zgodnie z mlx-swift z iOS gdzie Bielik dziala czysto).
            // Dla 4-bit quantized modeli (Bielik 4.5B 4-bit, Qwen 0.8B 4-bit)
            // dodatkowy repeat_penalty na juz zdegradowanej kwantyzacja
            // dystrybucji logitow rozwala koherencje — model losuje tokeny z
            // calego corpusu zamiast trzymac sie watku.
            repeat_penalty: 1.0,
            stop_sequences: vec![],
            system_prompt: None,
        }
    }
}

/// Wynik generowania
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResult {
    pub text: String,
    pub tokens_generated: u32,
    /// Tokeny na sekunde — liczone od momentu wygenerowania 1-szego tokena (bez prefill)
    pub tokens_per_second: f64,
    pub prompt_tokens: u32,
    pub stop_reason: StopReason,
    /// Czas do pierwszego tokena w milisekundach (prefill + 1 forward pass + sampling)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    /// Calkowity czas generowania w milisekundach (prefill + decode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_time_ms: Option<u64>,
}

/// Powod zatrzymania generowania
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopReason {
    MaxTokens,
    StopSequence(String),
    EndOfText,
}

/// Pojedynczy token w streamie
#[derive(Debug, Clone)]
pub struct StreamToken {
    pub text: String,
    pub is_final: bool,
}

/// Parametry embeddingów
#[derive(Debug, Clone)]
pub struct EmbeddingParams {
    pub texts: Vec<String>,
    pub normalize: bool,
}

/// Wynik obliczania embeddingów
#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    pub embeddings: Vec<Vec<f32>>,
    pub dimensions: usize,
}

/// Interfejs silnika inferencji — implementowany przez backendy (llama.cpp, MLX)
#[async_trait]
pub trait InferenceEngine: Send + Sync {
    /// Nazwa backendu ("llamacpp", "mlx")
    fn backend_name(&self) -> &str;

    /// Lista obslugiwanych formatow modeli
    fn supported_formats(&self) -> Vec<String>;

    /// Zaladuj model z podanej sciezki
    async fn load_model(
        &self,
        model_path: &Path,
        gpu_layers: Option<u32>,
    ) -> anyhow::Result<ModelInfo>;

    /// Wyladuj model z pamieci
    async fn unload_model(&self) -> anyhow::Result<()>;

    /// Informacje o zaladowanym modelu (None jesli nie zaladowany)
    fn model_info(&self) -> Option<ModelInfo>;

    /// Generuj tekst (blokujace — czeka na caly wynik)
    async fn generate(&self, params: GenerateParams) -> anyhow::Result<GenerateResult>;

    /// Generuj tekst ze streamingiem (zwraca kanal z tokenami)
    async fn generate_stream(
        &self,
        params: GenerateParams,
    ) -> anyhow::Result<mpsc::Receiver<StreamToken>>;

    /// Oblicz embeddingi (opcjonalne — nie kazdy backend wspiera)
    async fn embeddings(&self, _params: EmbeddingParams) -> anyhow::Result<EmbeddingResult> {
        anyhow::bail!(
            "Embeddingi nie sa obslugiwane przez backend {}",
            self.backend_name()
        )
    }

    /// Czy model jest zaladowany?
    fn is_loaded(&self) -> bool {
        self.model_info().map(|m| m.loaded).unwrap_or(false)
    }
}

/// Manager silnikow inferencji — wybiera odpowiedni backend
pub struct InferenceManager {
    engines: Vec<Box<dyn InferenceEngine>>,
    active_engine: Option<usize>,
}

impl InferenceManager {
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut engines: Vec<Box<dyn InferenceEngine>> = Vec::new();

        // Rejestruj dostepne backendy
        #[cfg(feature = "inference-llamacpp")]
        {
            engines.push(Box::new(llamacpp::LlamaCppEngine::new()));
        }

        // MLX przez Swift bridge — wymaga zarejestrowanych callbackow z
        // libMLXBridge.dylib (bootstrap w tentaflow/src/mlx_swift_init.rs).
        // Bez bridge'a engine nie pojawia sie na liscie.
        #[cfg(feature = "inference-mlx")]
        {
            if mlx_swift_bridge::is_available() {
                engines.push(Box::new(mlx_swift_bridge::MlxSwiftEngine::new()));
            }
        }

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
    pub fn active_engine(&self) -> Option<&dyn InferenceEngine> {
        self.active_engine
            .and_then(|i| self.engines.get(i).map(|e| e.as_ref()))
    }

    /// Zaladuj model — automatycznie wybierze backend na podstawie formatu
    pub async fn load_model(
        &mut self,
        model_path: &Path,
        gpu_layers: Option<u32>,
        preferred_backend: Option<&str>,
    ) -> anyhow::Result<ModelInfo> {
        let ext = model_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let engine_idx = if let Some(backend) = preferred_backend {
            self.engines
                .iter()
                .position(|e| e.backend_name() == backend)
                .ok_or_else(|| anyhow::anyhow!("Backend '{}' nie jest dostepny", backend))?
        } else if model_path.is_dir() {
            // Katalog z plikami safetensors -> backend MLX
            self.engines
                .iter()
                .position(|e| e.backend_name() == "mlx")
                .or_else(|| self.engines.iter().position(|_| true))
                .ok_or_else(|| anyhow::anyhow!("Brak backendu MLX dla katalogu modelu"))?
        } else {
            match ext {
                "gguf" => self
                    .engines
                    .iter()
                    .position(|e| e.backend_name() == "llamacpp"),
                "safetensors" | "mlx" => {
                    self.engines.iter().position(|e| e.backend_name() == "mlx")
                }
                _ => self.engines.iter().position(|_| true),
            }
            .ok_or_else(|| anyhow::anyhow!("Brak backendu obslugujacego format '{}'", ext))?
        };

        let info = self.engines[engine_idx]
            .load_model(model_path, gpu_layers)
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
}
