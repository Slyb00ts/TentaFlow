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
    /// Hard cap on total context (prompt + generated) for the MLX runtime guard.
    /// 0 = use the model's native max. The Swift runner aborts with a clean
    /// "insufficient memory" error instead of OOMing when this is exceeded.
    pub max_context_tokens: u32,
    /// Upper bound (MB) on how much memory the MLX model may use. 0 = no cap.
    /// Enforced via `GPU.set(memoryLimit:)` + a per-token memory snapshot check.
    pub memory_budget_mb: u32,
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
            // 0 = native max / no cap; the deploy wizard pins real values that
            // persist via the mlx `[[parameter]]` bindings.
            max_context_tokens: 0,
            memory_budget_mb: 0,
        }
    }
}

impl GenerateParams {
    /// Inicjalizuj `GenerateParams` z deploy-time defaults dla MLX engine.
    /// `default_max_tokens` / `default_temperature` / `default_top_p` /
    /// `default_top_k` / `default_repeat_penalty` z `mlx` mapy nadpisuja
    /// hardcoded `Default` jako baseline. Request-time wartosci z OpenAI
    /// API maja priorytet wyzszy (dolaczane przez
    /// `merge_request_override`).
    pub fn from_mlx_deploy_defaults(
        defaults: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Self {
        let mut p = Self::default();
        if let Some(v) = defaults.get("default_max_tokens").and_then(|v| v.as_u64()) {
            p.max_tokens = v as u32;
        }
        if let Some(v) = defaults.get("default_temperature").and_then(|v| v.as_f64()) {
            p.temperature = v as f32;
        }
        if let Some(v) = defaults.get("default_top_p").and_then(|v| v.as_f64()) {
            p.top_p = v as f32;
        }
        if let Some(v) = defaults.get("default_top_k").and_then(|v| v.as_u64()) {
            p.top_k = v as u32;
        }
        if let Some(v) = defaults
            .get("default_repeat_penalty")
            .and_then(|v| v.as_f64())
        {
            p.repeat_penalty = v as f32;
        }
        // Keys match the `mlx_field` bindings in mlx.toml; they are re-derived
        // from the persisted config_json on every (re)deploy and restart.
        if let Some(v) = defaults.get("max_context_tokens").and_then(|v| v.as_u64()) {
            p.max_context_tokens = v as u32;
        }
        if let Some(v) = defaults.get("memory_budget_mb").and_then(|v| v.as_u64()) {
            p.memory_budget_mb = v as u32;
        }
        p
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
    /// Powód zakończenia — ustawiany WYŁĄCZNIE na tokenie finalnym (is_final=true).
    /// `None` dla fragmentów. Pozwala konsumentowi zmapować realny finish_reason
    /// zamiast twardego "stop". Backendy, które nie znają powodu, ustawiają
    /// `Some(StopReason::EndOfText)` na finale.
    pub finish_reason: Option<StopReason>,
    /// Twardy błąd silnika (np. llama_decode rc!=0, błąd MTP process). Ustawiany
    /// tylko na tokenie finalnym, gdy generacja przerwana błędem. Konsument musi
    /// propagować ten błąd zamiast cicho kończyć strumień jako "stop".
    pub error: Option<String>,
    /// Liczniki tokenów wypełniane WYŁĄCZNIE na tokenie finalnym (is_final=true),
    /// 0 na fragmentach. Per-request mieszczą się w u32; kumulacja globalna w górę
    /// (token_usage_daily) jest i64 i nigdy nie przenosi sumy tym kanałem.
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Przepustowość faz raportowana przez silnik na tokenie finalnym: prefill
    /// (tokeny promptu / czas prefillu) i dekodowanie (tokeny generacji / czas
    /// generacji). 0.0 = silnik nie podał pomiaru (konsument wraca do wall-clock).
    pub prefill_tps: f32,
    pub completion_tps: f32,
    /// Czas do pierwszego tokena (ms) zmierzony WEWNĄTRZ silnika po granicach faz
    /// slotu, nie zegarem ściennym konsumenta. Ustawiany wyłącznie na tokenie
    /// finalnym; 0 = silnik nie podał pomiaru (konsument wraca do wall-clock).
    pub ttft_ms: u32,
}

/// Przepustowość prefillu w t/s. Pomiar silnika wygrywa: silnik zna realne
/// granice faz swojego slotu, a zegar konsumenta łapie dodatkowo kolejkę kanału
/// i harmonogram tokio. `engine_tps == 0.0` znaczy „silnik nie zmierzył" — tylko
/// wtedy liczymy z okna prefillu. Jeden kontrakt dla KAŻDEGO silnika, żeby
/// kolumny t/s z różnych backendów dało się porównywać.
pub fn prefill_tps(engine_tps: f32, prompt_tokens: u32, prefill_secs: f32) -> f32 {
    if engine_tps > 0.0 {
        engine_tps
    } else if prefill_secs > 0.0 && prompt_tokens > 0 {
        prompt_tokens as f32 / prefill_secs
    } else {
        0.0
    }
}

/// Przepustowość dekodowania w t/s, ta sama zasada co `prefill_tps`. Okno liczy
/// N-1 interwałów między pierwszym a ostatnim tokenem — pierwszy token nie ma
/// poprzednika, więc wliczenie go zawyżałoby tempo.
pub fn decode_tps(engine_tps: f32, completion_tokens: u32, decode_secs: f32) -> f32 {
    if engine_tps > 0.0 {
        engine_tps
    } else if decode_secs > 0.0 && completion_tokens > 1 {
        (completion_tokens - 1) as f32 / decode_secs
    } else {
        0.0
    }
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

/// Snapshot deploy-time parametrow zwiazanych z aktywnym modelem.
/// Wszystkie mapy `key → JSON value` — backend interpretuje per silnik
/// (np. llama-cpp czyta `ctx_size`/`n_gpu_layers`/`threads`/`batch_size`,
/// MLX trzyma `default_max_tokens`/`default_temperature` jako request-
/// time defaults). Empty snapshot = sensowne defaulty per backend.
#[derive(Debug, Default, Clone)]
pub struct DeployParamsSnapshot {
    pub llamacpp: std::collections::HashMap<String, serde_json::Value>,
    pub mlx: std::collections::HashMap<String, serde_json::Value>,
}

impl DeployParamsSnapshot {
    /// Convenience: legacy `gpu_layers` jako pojedyncze pole — trzymane
    /// dla zachowania prostej sciezki migracji starych callerow ktorzy
    /// znaja tylko `gpu_layers`.
    pub fn with_gpu_layers(layers: Option<u32>) -> Self {
        let mut s = Self::default();
        if let Some(l) = layers {
            s.llamacpp
                .insert("n_gpu_layers".into(), serde_json::json!(l));
        }
        s
    }
}

/// Interfejs silnika inferencji — implementowany przez backendy (llama.cpp, MLX)
#[async_trait]
pub trait InferenceEngine: Send + Sync {
    /// Nazwa backendu ("llamacpp", "mlx")
    fn backend_name(&self) -> &str;

    /// Lista obslugiwanych formatow modeli
    fn supported_formats(&self) -> Vec<String>;

    /// Zaladuj model z podanej sciezki. `deploy_params` niesie typed
    /// load-time tunables — llama-cpp czyta `ctx_size`/`n_gpu_layers`/
    /// `threads`/`batch_size`, MLX nic z load-time nie konsumuje
    /// (deploy defaults zywa w `InferenceManager.active_deploy_params`
    /// i sa materializowane przy kazdym `generate`).
    async fn load_model(
        &self,
        model_path: &Path,
        deploy_params: &DeployParamsSnapshot,
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

/// Manager silnikow inferencji — wybiera odpowiedni backend.
/// **Singleton invariant:** jeden active embedded LLM per host process
/// (architektura `OnceLock<Arc<RwLock<InferenceManager>>>` w
/// `shared_inference_manager`). Deploy drugiego embedded LLM podmienia
/// active_engine + active_deploy_params.
pub struct InferenceManager {
    engines: Vec<Box<dyn InferenceEngine>>,
    active_engine: Option<usize>,
    /// Typed deploy params aktualnego modelu. Ustawiane przy `load_model`,
    /// czyszczone w `unload_model`. `LocalInferenceHandler` czyta przez
    /// `get_deploy_params()` i materializuje do `GenerateParams` jako
    /// baseline (request override z OpenAI API ma priorytet wyzszy).
    active_deploy_params: DeployParamsSnapshot,
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
            active_deploy_params: DeployParamsSnapshot::default(),
        }
    }

    /// Snapshot aktualnych deploy params. `LocalInferenceHandler` woła to
    /// per chat completion request zeby zbudowac `GenerateParams` z
    /// `default_temperature`/`default_max_tokens`/`default_top_p` z
    /// `mlx` mapy jako baseline.
    pub fn get_deploy_params(&self) -> DeployParamsSnapshot {
        self.active_deploy_params.clone()
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

    /// Zaladuj model — automatycznie wybierze backend na podstawie formatu.
    /// `deploy_params` niesie typed load-time tunables (czytane przez
    /// llama-cpp) i request-time defaults (czytane przez MLX z
    /// `active_deploy_params`).
    pub async fn load_model(
        &mut self,
        model_path: &Path,
        deploy_params: DeployParamsSnapshot,
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
            .load_model(model_path, &deploy_params)
            .await?;
        self.active_engine = Some(engine_idx);
        self.active_deploy_params = deploy_params;
        Ok(info)
    }

    /// Wyladuj model. Czysci tez `active_deploy_params` zeby kolejne
    /// request po unload nie czytaly stale wartosci poprzedniego modelu.
    pub async fn unload_model(&mut self) -> anyhow::Result<()> {
        if let Some(idx) = self.active_engine {
            self.engines[idx].unload_model().await?;
            self.active_engine = None;
            self.active_deploy_params = DeployParamsSnapshot::default();
        }
        Ok(())
    }
}

#[cfg(test)]
mod throughput_tests {
    use super::{decode_tps, prefill_tps};

    #[test]
    fn engine_measurement_wins_over_wall_clock() {
        // Zegar konsumenta dałby tu 4x mniej — pomiar silnika nie może przegrać.
        assert_eq!(prefill_tps(800.0, 512, 2.0), 800.0);
        assert_eq!(decode_tps(120.0, 101, 10.0), 120.0);
    }

    #[test]
    fn wall_clock_fills_in_when_engine_is_silent() {
        assert_eq!(prefill_tps(0.0, 512, 2.0), 256.0);
        // N-1 interwałów: 101 tokenów w 10 s to 10 t/s, nie 10.1.
        assert_eq!(decode_tps(0.0, 101, 10.0), 10.0);
    }

    #[test]
    fn unmeasurable_stays_zero_instead_of_fabricated() {
        assert_eq!(prefill_tps(0.0, 0, 2.0), 0.0);
        assert_eq!(prefill_tps(0.0, 512, 0.0), 0.0);
        // Jeden token nie ma okna dekodowania.
        assert_eq!(decode_tps(0.0, 1, 10.0), 0.0);
        assert_eq!(decode_tps(0.0, 100, 0.0), 0.0);
    }
}
