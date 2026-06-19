// =============================================================================
// Plik: inference/llamacpp.rs
// Opis: Adapter llama.cpp dla lokalnej inferencji GGUF oparty o silnik continuous
//       batching (LlamaEngine) z własnego wrappera TentaFlow — równoległe zapytania
//       na jednym modelu/kontekście, streaming bez wątku-per-request (anty-hang).
// Przykład: InferenceManager ładuje GGUF i wywołuje generate/generate_stream/
//           embeddings przez ten adapter.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tentaflow_wrappers::llama::{LlamaLoadConfig, LlamaRuntime, SpeculativeConfig};
use tentaflow_wrappers::llama_engine::{
    EngineConfig, EngineSink, FinishReason, GenRequest, LlamaEngine, SamplingParams, SinkStatus,
    SpeculativeMode, StreamToken as WrapperStreamToken,
};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::inference::{
    EmbeddingParams, EmbeddingResult, GenerateParams, GenerateResult, InferenceEngine, ModelInfo,
    StopReason, StreamToken,
};

// Domyślna liczba slotów sekwencji, gdy deploy nie poda jawnie równoległości.
// Daje realną współbieżność out-of-the-box bez zalewania VRAM.
const DEFAULT_N_SEQ_MAX: u32 = 8;

// Stan jednego załadowanego modelu. `engine` to silnik continuous batching
// (generation-only). `embed_runtime` to leniwie tworzona, osobna ścieżka
// embeddingów (patrz niżej — silnik nie liczy embeddingów). Oba żyją tu, żeby
// unload mógł je czysto zdropować.
struct LoadedModel {
    engine: Arc<LlamaEngine>,
    info: ModelInfo,
    // Materiał do leniwego utworzenia runtime embeddingów: ścieżka modelu i
    // konfiguracja load (ctx/gpu/threads/flash-attn). Tworzymy go dopiero przy
    // pierwszym żądaniu embeddingów, by deploy generacyjny nie ładował drugi raz
    // modelu, jeśli embeddingi nie są używane.
    embed_source: (PathBuf, LlamaLoadConfig),
    embed_runtime: RwLock<Option<Arc<LlamaRuntime>>>,
}

pub struct LlamaCppEngine {
    state: Arc<RwLock<Option<LoadedModel>>>,
}

// Ujście streamingu: konwertuje wrapperowy StreamToken na rdzeniowy i wkłada do
// kanału tokio bez blokowania scheduler-a silnika. `try_send` na tokio Senderze
// jest nieblokujący; pełny kanał → Full (silnik odłoży token do pending tego
// slotu), zamknięty → Closed (silnik zwolni slot).
struct StreamSink {
    tx: mpsc::Sender<StreamToken>,
}

impl EngineSink for StreamSink {
    fn try_send(&mut self, token: WrapperStreamToken) -> SinkStatus {
        // CR-003: na tokenie finalnym przenosimy realny finish_reason oraz twardy
        // błąd silnika do rdzeniowego StreamToken, zamiast maskować je jako "stop".
        let (finish_reason, error) = if token.is_final {
            match &token.finish_reason {
                Some(FinishReason::Error(msg)) => {
                    warn!("Strumień llama.cpp zakończony błędem silnika: {msg}");
                    (None, Some(msg.clone()))
                }
                other => (LlamaCppEngine::finish_reason_opt(other.clone()), None),
            }
        } else {
            (None, None)
        };
        let core = StreamToken {
            text: token.text,
            is_final: token.is_final,
            finish_reason,
            error,
        };
        match self.tx.try_send(core) {
            Ok(()) => SinkStatus::Delivered,
            Err(mpsc::error::TrySendError::Full(core)) => SinkStatus::Full(WrapperStreamToken {
                text: core.text,
                is_final: core.is_final,
                finish_reason: token.finish_reason,
                generated_tokens: token.generated_tokens,
                prompt_tokens: token.prompt_tokens,
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => SinkStatus::Closed,
        }
    }
}

// Ujście dla blokującego `generate`: przekazuje PEŁNY wrapperowy StreamToken
// (z finish_reason i liczbą tokenów na finale) do kanału tokio, skąd zbieramy
// cały wynik. Kanał ma pojemność równą limitowi strumienia silnika, więc Full
// realizuje backpressure per-slot tak samo jak w streamingu.
struct CollectSink {
    tx: mpsc::Sender<WrapperStreamToken>,
}

impl EngineSink for CollectSink {
    fn try_send(&mut self, token: WrapperStreamToken) -> SinkStatus {
        match self.tx.try_send(token) {
            Ok(()) => SinkStatus::Delivered,
            Err(mpsc::error::TrySendError::Full(t)) => SinkStatus::Full(t),
            Err(mpsc::error::TrySendError::Closed(_)) => SinkStatus::Closed,
        }
    }
}

impl LlamaCppEngine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(None)),
        }
    }

    fn load_config(deploy_params: &super::DeployParamsSnapshot) -> LlamaLoadConfig {
        LlamaLoadConfig::from_deploy_hash_map(&deploy_params.llamacpp)
    }

    // Liczba slotów sekwencji z deploy params (`n_parallel` lub `max_concurrency`),
    // z sensownym domyślnym fallbackiem. Wartość 0 traktujemy jak brak.
    fn n_seq_max(deploy_params: &super::DeployParamsSnapshot) -> u32 {
        let map = &deploy_params.llamacpp;
        let read = |key: &str| map.get(key).and_then(|v| v.as_u64()).map(|v| v as u32);
        read("n_parallel")
            .or_else(|| read("max_concurrency"))
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_N_SEQ_MAX)
    }

    // Mapuje deploy params na tryb speculative silnika. MTP/ngram konfiguruje się
    // przez te same klucze co dotychczas (`speculative_method`,
    // `num_speculative_tokens`, `size_ngram`, `size_mgram`). Silnik sam wymusza
    // n_rs_seq i zwróci błąd przy MTP bez głowy nextn — nie zgadujemy tu obecności
    // MTP, tylko przekładamy konfigurację.
    fn speculative_mode(deploy_params: &super::DeployParamsSnapshot) -> SpeculativeMode {
        match SpeculativeConfig::from_deploy_hash_map(&deploy_params.llamacpp) {
            SpeculativeConfig::Off => SpeculativeMode::Off,
            SpeculativeConfig::Mtp { num_tokens } => SpeculativeMode::Mtp {
                n_max: num_tokens.max(1),
            },
            SpeculativeConfig::NgramSimple {
                size_ngram,
                size_mgram,
            } => SpeculativeMode::NgramSimple {
                // size_mgram = maksymalna długość draftu (n_max), size_ngram =
                // minimalny wzorzec (n_min). Pilnujemy 1 <= n_min <= n_max.
                n_max: (size_mgram as u32).max(1),
                n_min: (size_ngram as u32).clamp(1, (size_mgram as u32).max(1)),
            },
        }
    }

    fn engine_config(
        deploy_params: &super::DeployParamsSnapshot,
        load: &LlamaLoadConfig,
    ) -> EngineConfig {
        let defaults = EngineConfig::default();
        let map = &deploy_params.llamacpp;
        let read_u32 = |key: &str| map.get(key).and_then(|v| v.as_u64()).map(|v| v as u32);

        EngineConfig {
            n_seq_max: Self::n_seq_max(deploy_params),
            ctx_per_seq: load.ctx_size,
            // n_batch dziedziczy z batch_size load-configu (ten sam klucz co dawniej);
            // n_ubatch z deploy lub domyślny. Silnik docina ubatch do batch.
            n_batch: load.batch_size.max(1),
            n_ubatch: read_u32("n_ubatch").unwrap_or(defaults.n_ubatch),
            n_gpu_layers: load.n_gpu_layers,
            // main_gpu/tensor_split sparsowane już w LlamaLoadConfig — to jedyna
            // droga wyboru kart dla embedded llama.cpp (CUDA init raz na proces).
            main_gpu: load.main_gpu,
            tensor_split: load.tensor_split.clone(),
            threads: load.threads,
            flash_attn: load.flash_attn,
            kv_unified: map
                .get("kv_unified")
                .and_then(|v| v.as_bool())
                .unwrap_or(defaults.kv_unified),
            // Silnik podbije n_rs_seq sam wg trybu speculative — zostawiamy 0.
            n_rs_seq: 0,
            speculative: Self::speculative_mode(deploy_params),
            queue_capacity: read_u32("queue_capacity")
                .map(|v| v as usize)
                .unwrap_or(defaults.queue_capacity),
            stream_capacity: read_u32("stream_capacity")
                .map(|v| v as usize)
                .unwrap_or(defaults.stream_capacity),
            // CR-001: deadline postępu dostarczania (w sekundach z deploy params).
            stream_stall_timeout: read_u32("stream_stall_timeout_secs")
                .map(|v| std::time::Duration::from_secs(v as u64))
                .unwrap_or(defaults.stream_stall_timeout),
        }
    }

    fn gen_request(params: &GenerateParams) -> GenRequest {
        GenRequest {
            prompt: params.prompt.clone(),
            system_prompt: params.system_prompt.clone(),
            sampling: SamplingParams {
                temperature: params.temperature,
                top_p: params.top_p,
                top_k: params.top_k,
                repeat_penalty: params.repeat_penalty,
                seed: 0,
            },
            max_tokens: params.max_tokens,
            stop_sequences: params.stop_sequences.clone(),
        }
    }

    fn stop_reason(reason: Option<FinishReason>) -> StopReason {
        match reason {
            Some(FinishReason::StopSequence(value)) => StopReason::StopSequence(value),
            Some(FinishReason::MaxTokens) => StopReason::MaxTokens,
            // ContextFull/PromptTooLong/Error/EndOfText/None → traktujemy jak
            // naturalne zakończenie; twarde błędy lecą osobną ścieżką (poniżej).
            _ => StopReason::EndOfText,
        }
    }

    // Mapuje powód zakończenia silnika na rdzeniowy StopReason dla strumienia.
    // Twardy błąd (FinishReason::Error) NIE trafia tu — jest obsługiwany osobno
    // jako `error` w StreamToken. Pozostałe powody mapujemy na realny StopReason.
    fn finish_reason_opt(reason: Option<FinishReason>) -> Option<StopReason> {
        match reason {
            Some(FinishReason::Error(_)) | None => Some(StopReason::EndOfText),
            other => Some(Self::stop_reason(other)),
        }
    }
}

#[async_trait]
impl InferenceEngine for LlamaCppEngine {
    fn backend_name(&self) -> &str {
        "llamacpp"
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["gguf".to_string()]
    }

    async fn load_model(
        &self,
        model_path: &Path,
        deploy_params: &super::DeployParamsSnapshot,
    ) -> Result<ModelInfo> {
        let path = model_path.to_path_buf();
        let load = Self::load_config(deploy_params);
        let engine_config = Self::engine_config(deploy_params, &load);

        info!(
            "Ladowanie modelu GGUF (continuous batching): {} (n_seq_max={}, ctx_per_seq={}, n_batch={}, n_ubatch={}, gpu_layers={}, threads={:?}, flash_attn={:?}, speculative={:?})",
            path.display(),
            engine_config.n_seq_max,
            engine_config.ctx_per_seq,
            engine_config.n_batch,
            engine_config.n_ubatch,
            engine_config.n_gpu_layers,
            engine_config.threads,
            engine_config.flash_attn,
            engine_config.speculative,
        );

        // Silnik ładuje model w swoim wątku-schedulerze; spawn_blocking trzyma
        // pulę tokio wolną na czas ładowania (sekundy dla dużych GGUF).
        let load_path = path.clone();
        let cfg = engine_config.clone();
        let engine = tokio::task::spawn_blocking(move || LlamaEngine::load(&load_path, cfg))
            .await
            .context("Blad w spawn_blocking podczas ladowania silnika")?
            .context("Nie udalo sie zaladowac modelu do silnika llama.cpp")?;

        // Metadane modelu czytamy osobno z GGUF (silnik nie wystawia ich, a info
        // dashboardu ich potrzebuje). Tani odczyt nagłówka, bez ładowania wag.
        let gguf = tentaflow_wrappers::llama::inspect_gguf(&path)
            .context("Nie udalo sie odczytac metadanych GGUF")?;
        let context_train = gguf.context_length.unwrap_or(0) as u32;
        let info = ModelInfo {
            name: gguf.name.clone(),
            path: path.to_string_lossy().to_string(),
            size_bytes: gguf.size_bytes,
            // Liczba parametrów nie jest dostępna z samego nagłówka GGUF bez
            // ładowania wag; silnik nie wystawia metadanych, więc zostawiamy puste.
            parameters: String::new(),
            quantization: gguf.quantization_version.map(|v| format!("v{v}")),
            context_length: if context_train > 0 {
                engine_config.ctx_per_seq.min(context_train)
            } else {
                engine_config.ctx_per_seq
            },
            loaded: true,
            vram_used_mb: 0,
            backend: "llamacpp".to_string(),
            chat_template: if gguf.mtp_layers > 0 {
                Some(format!("mtp:{}-layers", gguf.mtp_layers))
            } else {
                None
            },
        };

        let loaded = LoadedModel {
            engine: Arc::new(engine),
            info: info.clone(),
            embed_source: (path, load),
            embed_runtime: RwLock::new(None),
        };
        *self.state.write().await = Some(loaded);

        info!("Model zaladowany pomyslnie: {}", info.name);
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        // Drop silnika kończy wątek-scheduler czysto; drop runtime embeddingów
        // zwalnia drugi kontekst, jeśli był utworzony.
        let mut guard = self.state.write().await;
        if let Some(loaded) = guard.take() {
            info!("Model '{}' wyladowany z pamieci", loaded.info.name);
        } else {
            warn!("Proba wyladowania modelu gdy zaden nie jest zaladowany");
        }
        Ok(())
    }

    fn model_info(&self) -> Option<ModelInfo> {
        self.state
            .try_read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.info.clone()))
    }

    async fn generate(&self, params: GenerateParams) -> Result<GenerateResult> {
        // Krótki read-lock: klonujemy Arc<LlamaEngine> i NATYCHMIAST zwalniamy
        // lock — generacja biegnie bez trzymania jakiegokolwiek locka (anty-hang).
        let engine = {
            let guard = self.state.read().await;
            let loaded = guard.as_ref().context("Model nie jest zaladowany")?;
            Arc::clone(&loaded.engine)
        };

        let start = Instant::now();
        let request = Self::gen_request(&params);
        let (tx, mut rx) = mpsc::channel::<WrapperStreamToken>(256);
        engine
            .submit_with_sink(request, Box::new(CollectSink { tx }))
            .map_err(|e| anyhow::anyhow!("Silnik odrzucil request: {e}"))?;

        let mut text = String::new();
        let mut generated_tokens = 0_u32;
        let mut prompt_tokens = 0_u32;
        let mut stop = StopReason::EndOfText;
        let mut ttft: Option<Instant> = None;
        let mut hard_error: Option<String> = None;

        while let Some(token) = rx.recv().await {
            if !token.text.is_empty() {
                if ttft.is_none() {
                    ttft = Some(Instant::now());
                }
                text.push_str(&token.text);
            }
            if token.is_final {
                generated_tokens = token.generated_tokens;
                // CR-004: realna liczba tokenów promptu z silnika (slot zna prompt.len()).
                prompt_tokens = token.prompt_tokens;
                if let Some(FinishReason::Error(msg)) = &token.finish_reason {
                    hard_error = Some(msg.clone());
                }
                stop = Self::stop_reason(token.finish_reason);
                break;
            }
        }

        if let Some(msg) = hard_error {
            anyhow::bail!("Generacja llama.cpp nie powiodla sie: {msg}");
        }

        let elapsed = start.elapsed();
        let ttft_ms = ttft.map(|t| t.duration_since(start).as_millis() as u64);
        // CR-004: tok/s liczymy od PIERWSZEGO tokena (TTFT), nie od startu — czas
        // prefillu/TTFT nie zaniża tempa dekodowania. Bez pierwszego tokena (0 lub 1
        // wygenerowany) tempo nie ma sensu → 0.0.
        let decode_secs = match ttft {
            Some(t) => elapsed
                .saturating_sub(t.duration_since(start))
                .as_secs_f64(),
            None => 0.0,
        };
        let tokens_per_second = if decode_secs > 0.0 && generated_tokens > 1 {
            (generated_tokens.saturating_sub(1)) as f64 / decode_secs
        } else {
            0.0
        };

        Ok(GenerateResult {
            text,
            tokens_generated: generated_tokens,
            tokens_per_second,
            prompt_tokens,
            stop_reason: stop,
            time_to_first_token_ms: ttft_ms,
            total_time_ms: Some(elapsed.as_millis() as u64),
        })
    }

    async fn generate_stream(&self, params: GenerateParams) -> Result<mpsc::Receiver<StreamToken>> {
        // Krótki read-lock → klon Arc → zwolnienie locka → submit. Scheduler
        // silnika oddaje tokeny wprost do tego kanału tokio przez StreamSink,
        // bez wątku-per-request i bez trzymania locka podczas generacji.
        let engine = {
            let guard = self.state.read().await;
            let loaded = guard
                .as_ref()
                .context("Model nie jest zaladowany — wywolaj load_model() najpierw")?;
            Arc::clone(&loaded.engine)
        };

        let (tx, rx) = mpsc::channel::<StreamToken>(64);
        let request = Self::gen_request(&params);
        engine
            .submit_with_sink(request, Box::new(StreamSink { tx }))
            .map_err(|e| anyhow::anyhow!("Silnik odrzucil request: {e}"))?;

        Ok(rx)
    }

    async fn embeddings(&self, params: EmbeddingParams) -> Result<EmbeddingResult> {
        // Silnik continuous batching jest generation-only. Embeddingi liczymy
        // osobną, leniwie tworzoną ścieżką LlamaRuntime (kontekst z embeddings=true),
        // obok silnika generacji — bez regresji wobec poprzedniej implementacji.
        // Pełną unifikację (jeden model, kontekst embeddingów w silniku) zostawiamy
        // na później; tu liczy się zero regresji.
        let (runtime, source) = {
            let guard = self.state.read().await;
            let loaded = guard.as_ref().context("Model nie jest zaladowany")?;
            let existing = loaded.embed_runtime.read().await.clone();
            (existing, loaded.embed_source.clone())
        };

        let runtime = match runtime {
            Some(rt) => rt,
            None => {
                // Utwórz runtime embeddingów raz, pod write-lockiem pola (double-check).
                let (path, load) = source;
                let built = tokio::task::spawn_blocking(move || LlamaRuntime::load(&path, load))
                    .await
                    .context("Blad w spawn_blocking podczas ladowania runtime embeddingow")?
                    .context("Nie udalo sie zaladowac runtime embeddingow")?;
                let built = Arc::new(built);

                let guard = self.state.read().await;
                let loaded = guard.as_ref().context("Model nie jest zaladowany")?;
                let mut slot = loaded.embed_runtime.write().await;
                if let Some(existing) = slot.as_ref() {
                    Arc::clone(existing)
                } else {
                    *slot = Some(Arc::clone(&built));
                    built
                }
            }
        };

        let normalize = params.normalize;
        let texts = params.texts;
        let result = tokio::task::spawn_blocking(move || -> Result<EmbeddingResult> {
            let dimensions = runtime.metadata().embedding_size as usize;
            let mut embeddings = Vec::with_capacity(texts.len());
            for text in &texts {
                embeddings.push(runtime.embeddings(text, normalize)?);
            }
            Ok(EmbeddingResult {
                embeddings,
                dimensions,
            })
        })
        .await
        .context("Blad w spawn_blocking podczas obliczania embeddingow")??;

        Ok(result)
    }
}

impl Default for LlamaCppEngine {
    fn default() -> Self {
        Self::new()
    }
}
