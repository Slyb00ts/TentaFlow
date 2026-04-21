// =============================================================================
// Plik: inference/llamacpp.rs
// Opis: Adapter llama-cpp-rs (llama.cpp) dla lokalnej inferencji modeli GGUF.
//       Implementuje trait InferenceEngine z wykorzystaniem crate llama-cpp-2.
// =============================================================================

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::inference::{
    EmbeddingParams, EmbeddingResult, GenerateParams, GenerateResult, InferenceEngine, ModelInfo,
    StopReason, StreamToken,
};

/// Domyslny rozmiar kontekstu
const DEFAULT_CTX_SIZE: u32 = 4096;

/// Domyslna liczba warstw na GPU (99 = wszystkie)
const DEFAULT_GPU_LAYERS: u32 = 99;

/// Rozmiar batcha do przetwarzania prompt
const BATCH_SIZE: usize = 512;

/// Zaladowany model llama.cpp ze wszystkimi zasobami
struct LoadedModel {
    model: LlamaModel,
    backend: LlamaBackend,
    ctx_size: u32,
    info: ModelInfo,
}

// LlamaModel i LlamaBackend z llama-cpp-2 implementuja Send + Sync
unsafe impl Send for LoadedModel {}
unsafe impl Sync for LoadedModel {}

/// Adapter llama.cpp — lokalna inferencja modeli GGUF
pub struct LlamaCppEngine {
    state: Arc<Mutex<Option<LoadedModel>>>,
}

impl LlamaCppEngine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Buduje lancuch samplerow na podstawie parametrow generowania
    fn build_sampler(params: &GenerateParams) -> LlamaSampler {
        let mut samplers: Vec<LlamaSampler> = Vec::new();

        // Kara za powtorzenia
        if params.repeat_penalty > 1.0 {
            samplers.push(LlamaSampler::penalties(
                64, // okno ostatnich tokenow do sprawdzenia
                params.repeat_penalty,
                0.0, // frequency_penalty
                0.0, // presence_penalty
            ));
        }

        // Top-K filtrowanie
        if params.top_k > 0 {
            samplers.push(LlamaSampler::top_k(params.top_k as i32));
        }

        // Top-P (nucleus sampling)
        if params.top_p < 1.0 {
            samplers.push(LlamaSampler::top_p(params.top_p, 1));
        }

        // Temperatura
        samplers.push(LlamaSampler::temp(params.temperature));

        // Koncowy sampler: greedy jesli temp <= 0, losowy w przeciwnym razie
        if params.temperature <= 0.0 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::dist(0));
        }

        LlamaSampler::chain_simple(samplers)
    }

    /// Sprawdza czy wygenerowany tekst konczy sie na stop sequence
    fn check_stop_sequence<'a>(text: &str, stop_sequences: &'a [String]) -> Option<&'a str> {
        for stop in stop_sequences {
            if text.ends_with(stop.as_str()) {
                return Some(stop.as_str());
            }
        }
        None
    }

    /// Laduje model z pliku GGUF (operacja synchroniczna)
    fn load_model_sync(model_path: &Path, gpu_layers: u32, ctx_size: u32) -> Result<LoadedModel> {
        info!("Inicjalizacja backendu llama.cpp...");

        let backend = LlamaBackend::init().map_err(|e| {
            if matches!(e, llama_cpp_2::LlamaCppError::BackendAlreadyInitialized) {
                warn!("Backend llama.cpp juz zainicjalizowany — kontynuuje");
            }
            anyhow::anyhow!("Blad inicjalizacji backendu llama.cpp: {}", e)
        })?;

        // Przekierowanie logow llama.cpp do tracing
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());

        let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);

        info!(
            "Ladowanie modelu GGUF: {} (gpu_layers={})",
            model_path.display(),
            gpu_layers,
        );

        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie zaladowac modelu GGUF: {}", e))?;

        let n_params = model.n_params();
        let size_bytes = model.size() as u64;
        let n_ctx_train = model.n_ctx_train();

        info!(
            "Model zaladowany: vocab={}, n_ctx_train={}, params={}M, size={}MB",
            model.n_vocab(),
            n_ctx_train,
            n_params / 1_000_000,
            size_bytes / (1024 * 1024),
        );

        // Okreslenie kwantyzacji na podstawie rozszerzenia nazwy pliku
        let quantization = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| {
                // Typowe wzorce: model-Q4_K_M.gguf, model.Q5_K_S.gguf
                let upper = name.to_uppercase();
                [
                    "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_K_S", "Q4_K_M", "Q5_0",
                    "Q5_K_S", "Q5_K_M", "Q6_K", "Q8_0", "F16", "F32",
                ]
                .iter()
                .find(|q| upper.contains(*q))
                .map(|q| q.to_string())
            });

        let info = ModelInfo {
            name: model_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
            path: model_path.to_string_lossy().to_string(),
            size_bytes,
            parameters: format!("{}M", n_params / 1_000_000),
            quantization,
            context_length: ctx_size.min(n_ctx_train),
            loaded: true,
            vram_used_mb: 0, // llama.cpp nie udostepnia tego bezposrednio
            backend: "llamacpp".to_string(),
            chat_template: None,
        };

        Ok(LoadedModel {
            model,
            backend,
            ctx_size: ctx_size.min(n_ctx_train),
            info,
        })
    }

    /// Generuje tekst synchronicznie (wywoływane w spawn_blocking)
    fn generate_sync(state: &LoadedModel, params: &GenerateParams) -> Result<GenerateResult> {
        let start = Instant::now();

        // Polacz system prompt z promptem uzytkownika
        let full_prompt = match &params.system_prompt {
            Some(sys) => format!("{}\n\n{}", sys, params.prompt),
            None => params.prompt.clone(),
        };

        // Tokenizacja
        let tokens = state
            .model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| anyhow::anyhow!("Blad tokenizacji: {}", e))?;

        let prompt_tokens = tokens.len() as u32;
        debug!(
            "Prompt: {} znakow -> {} tokenow",
            full_prompt.len(),
            prompt_tokens
        );

        // Kontekst inferencji
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(state.ctx_size))
            .with_n_batch(BATCH_SIZE as u32);

        let mut ctx = state
            .model
            .new_context(&state.backend, ctx_params)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc kontekstu: {}", e))?;

        // Batch z prompt tokenami
        let mut batch = LlamaBatch::new(BATCH_SIZE, 1);
        let last_idx = tokens.len() - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i == last_idx)
                .map_err(|e| anyhow::anyhow!("Blad dodawania tokena do batch: {}", e))?;
        }

        // Dekodowanie prompt
        ctx.decode(&mut batch)
            .map_err(|e| anyhow::anyhow!("Blad dekodowania prompt: {}", e))?;

        // Sampler
        let mut sampler = Self::build_sampler(params);

        let mut generated_text = String::new();
        let mut generated_tokens: u32 = 0;
        let mut stop_reason = StopReason::MaxTokens;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut n_cur = tokens.len() as i32;

        for _ in 0..params.max_tokens {
            let new_token = sampler.sample(&ctx, -1);
            sampler.accept(new_token);

            // Sprawdz koniec generowania (EOS)
            if state.model.is_eog_token(new_token) {
                stop_reason = StopReason::EndOfText;
                break;
            }

            // Dekoduj token na tekst
            let piece = state
                .model
                .token_to_piece(new_token, &mut decoder, false, None)
                .unwrap_or_default();

            generated_text.push_str(&piece);
            generated_tokens += 1;

            // Sprawdz stop sequences
            if let Some(matched) =
                Self::check_stop_sequence(&generated_text, &params.stop_sequences)
            {
                let trim_len = matched.len();
                let new_len = generated_text.len() - trim_len;
                generated_text.truncate(new_len);
                stop_reason = StopReason::StopSequence(matched.to_string());
                break;
            }

            // Sprawdz limit kontekstu
            if n_cur + 1 >= state.ctx_size as i32 {
                stop_reason = StopReason::MaxTokens;
                break;
            }

            // Przygotuj nastepny batch
            batch.clear();
            batch
                .add(new_token, n_cur, &[0], true)
                .map_err(|e| anyhow::anyhow!("Blad dodawania tokena do batch: {}", e))?;
            n_cur += 1;

            ctx.decode(&mut batch)
                .map_err(|e| anyhow::anyhow!("Blad dekodowania: {}", e))?;
        }

        let elapsed = start.elapsed();
        let tokens_per_second = if elapsed.as_secs_f64() > 0.0 {
            generated_tokens as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        Ok(GenerateResult {
            text: generated_text,
            tokens_generated: generated_tokens,
            tokens_per_second,
            prompt_tokens,
            stop_reason,
            time_to_first_token_ms: None,
            total_time_ms: Some(elapsed.as_millis() as u64),
        })
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

    async fn load_model(&self, model_path: &Path, gpu_layers: Option<u32>) -> Result<ModelInfo> {
        let path = model_path.to_path_buf();
        let layers = gpu_layers.unwrap_or(DEFAULT_GPU_LAYERS);
        let ctx_size = DEFAULT_CTX_SIZE;

        info!(
            "Ladowanie modelu: {} (gpu_layers={}, ctx={})",
            path.display(),
            layers,
            ctx_size,
        );

        // Ladowanie w osobnym watku (operacja synchroniczna C FFI)
        let loaded =
            tokio::task::spawn_blocking(move || Self::load_model_sync(&path, layers, ctx_size))
                .await
                .context("Blad w spawn_blocking podczas ladowania modelu")?
                .context("Nie udalo sie zaladowac modelu")?;

        let info = loaded.info.clone();
        *self.state.lock().await = Some(loaded);

        info!("Model zaladowany pomyslnie: {}", info.name);
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        let mut guard = self.state.lock().await;
        if guard.is_some() {
            let name = guard
                .as_ref()
                .map(|m| m.info.name.clone())
                .unwrap_or_default();
            *guard = None;
            info!("Model '{}' wyladowany z pamieci", name);
        } else {
            warn!("Proba wyladowania modelu gdy zaden nie jest zaladowany");
        }
        Ok(())
    }

    fn model_info(&self) -> Option<ModelInfo> {
        // Probujem zdobyc lock bez blokowania — jesli nie uda sie, zwracamy None
        self.state
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.info.clone()))
    }

    async fn generate(&self, params: GenerateParams) -> Result<GenerateResult> {
        {
            let guard = self.state.lock().await;
            if guard.is_none() {
                anyhow::bail!("Model nie jest zaladowany — wywolaj load_model() najpierw");
            }
        }

        let state = self.state.clone();
        let params_clone = params;

        tokio::task::spawn_blocking(move || {
            // Blokujacy lock w watku spawn_blocking
            let rt = tokio::runtime::Handle::current();
            let guard = rt.block_on(state.lock());
            let loaded = guard
                .as_ref()
                .context("Model zostal wyladowany w trakcie generowania")?;

            Self::generate_sync(loaded, &params_clone)
        })
        .await
        .context("Blad w spawn_blocking podczas generowania")?
    }

    async fn generate_stream(&self, params: GenerateParams) -> Result<mpsc::Receiver<StreamToken>> {
        // Sprawdz czy model jest zaladowany
        {
            let guard = self.state.lock().await;
            if guard.is_none() {
                anyhow::bail!("Model nie jest zaladowany — wywolaj load_model() najpierw");
            }
        }

        let (tx, rx) = mpsc::channel::<StreamToken>(64);
        let state = self.state.clone();
        let params_clone = params;

        // Generacja w osobnym watku
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let guard = rt.block_on(state.lock());
            let loaded = match guard.as_ref() {
                Some(m) => m,
                None => {
                    warn!("Model wyladowany przed rozpoczeciem streamingu");
                    return;
                }
            };

            if let Err(e) = Self::stream_tokens(loaded, &params_clone, &tx) {
                warn!("Blad podczas streamingu tokenow: {}", e);
            }
        });

        Ok(rx)
    }

    async fn embeddings(&self, params: EmbeddingParams) -> Result<EmbeddingResult> {
        let guard = self.state.lock().await;
        let loaded = guard.as_ref().context("Model nie jest zaladowany")?;

        // Sprawdz czy model wspiera embeddingi
        // llama.cpp obsluguje embeddingi tylko dla modeli embedding
        let n_embd = loaded.model.n_embd() as usize;
        if n_embd == 0 {
            anyhow::bail!("Model nie wspiera embedddingow");
        }

        drop(guard);

        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let guard = rt.block_on(state.lock());
            let loaded = guard
                .as_ref()
                .context("Model zostal wyladowany w trakcie obliczania embedddingow")?;

            let n_embd = loaded.model.n_embd() as usize;
            let mut all_embeddings = Vec::with_capacity(params.texts.len());

            for text in &params.texts {
                let tokens = loaded
                    .model
                    .str_to_token(text, AddBos::Always)
                    .map_err(|e| anyhow::anyhow!("Blad tokenizacji: {}", e))?;

                let ctx_params = LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(loaded.ctx_size))
                    .with_n_batch(BATCH_SIZE as u32)
                    .with_embeddings(true);

                let mut ctx = loaded
                    .model
                    .new_context(&loaded.backend, ctx_params)
                    .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc kontekstu: {}", e))?;

                let mut batch = LlamaBatch::new(BATCH_SIZE, 1);
                let last_idx = tokens.len() - 1;
                for (i, token) in tokens.iter().enumerate() {
                    batch
                        .add(*token, i as i32, &[0], i == last_idx)
                        .map_err(|e| anyhow::anyhow!("Blad dodawania tokena do batch: {}", e))?;
                }

                ctx.decode(&mut batch)
                    .map_err(|e| anyhow::anyhow!("Blad dekodowania: {}", e))?;

                // Pobierz embedding z ostatniego tokena
                let embd = ctx
                    .embeddings_seq_ith(0)
                    .map_err(|e| anyhow::anyhow!("Nie udalo sie pobrac embeddingu: {}", e))?;

                let mut embedding: Vec<f32> = embd.to_vec();

                // Normalizacja L2 jesli wymagana
                if params.normalize {
                    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
                    if norm > 0.0 {
                        for val in &mut embedding {
                            *val /= norm;
                        }
                    }
                }

                all_embeddings.push(embedding);
            }

            Ok(EmbeddingResult {
                embeddings: all_embeddings,
                dimensions: n_embd,
            })
        })
        .await
        .context("Blad w spawn_blocking podczas obliczania embedddingow")?
    }
}

impl LlamaCppEngine {
    /// Generuje tokeny i wysyla je przez kanal (operacja synchroniczna)
    fn stream_tokens(
        loaded: &LoadedModel,
        params: &GenerateParams,
        tx: &mpsc::Sender<StreamToken>,
    ) -> Result<()> {
        // Polacz system prompt z promptem uzytkownika
        let full_prompt = match &params.system_prompt {
            Some(sys) => format!("{}\n\n{}", sys, params.prompt),
            None => params.prompt.clone(),
        };

        // Tokenizacja
        let tokens = loaded
            .model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| anyhow::anyhow!("Blad tokenizacji: {}", e))?;

        debug!(
            "Stream: prompt {} znakow -> {} tokenow",
            full_prompt.len(),
            tokens.len()
        );

        // Kontekst
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(loaded.ctx_size))
            .with_n_batch(BATCH_SIZE as u32);

        let mut ctx = loaded
            .model
            .new_context(&loaded.backend, ctx_params)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc kontekstu: {}", e))?;

        // Batch z prompt tokenami
        let mut batch = LlamaBatch::new(BATCH_SIZE, 1);
        let last_idx = tokens.len() - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i == last_idx)
                .map_err(|e| anyhow::anyhow!("Blad dodawania tokena do batch: {}", e))?;
        }

        // Dekodowanie prompt
        ctx.decode(&mut batch)
            .map_err(|e| anyhow::anyhow!("Blad dekodowania prompt: {}", e))?;

        // Sampler
        let mut sampler = Self::build_sampler(params);

        let mut generated_text = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut n_cur = tokens.len() as i32;

        for _ in 0..params.max_tokens {
            let new_token = sampler.sample(&ctx, -1);
            sampler.accept(new_token);

            // Sprawdz EOS
            if loaded.model.is_eog_token(new_token) {
                let _ = tx.blocking_send(StreamToken {
                    text: String::new(),
                    is_final: true,
                });
                return Ok(());
            }

            // Dekoduj token
            let piece = loaded
                .model
                .token_to_piece(new_token, &mut decoder, false, None)
                .unwrap_or_default();

            generated_text.push_str(&piece);

            // Sprawdz stop sequences
            if let Some(_matched) =
                Self::check_stop_sequence(&generated_text, &params.stop_sequences)
            {
                let _ = tx.blocking_send(StreamToken {
                    text: String::new(),
                    is_final: true,
                });
                return Ok(());
            }

            // Wyslij token — jesli odbiorca zamknal kanal, konczymy
            if tx
                .blocking_send(StreamToken {
                    text: piece,
                    is_final: false,
                })
                .is_err()
            {
                return Ok(());
            }

            // Sprawdz limit kontekstu
            if n_cur + 1 >= loaded.ctx_size as i32 {
                let _ = tx.blocking_send(StreamToken {
                    text: String::new(),
                    is_final: true,
                });
                return Ok(());
            }

            // Przygotuj nastepny batch
            batch.clear();
            batch
                .add(new_token, n_cur, &[0], true)
                .map_err(|e| anyhow::anyhow!("Blad dodawania tokena do batch: {}", e))?;
            n_cur += 1;

            ctx.decode(&mut batch)
                .map_err(|e| anyhow::anyhow!("Blad dekodowania: {}", e))?;
        }

        // Osiagnieto max_tokens
        let _ = tx.blocking_send(StreamToken {
            text: String::new(),
            is_final: true,
        });

        Ok(())
    }
}
