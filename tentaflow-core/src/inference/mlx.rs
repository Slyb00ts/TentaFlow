// =============================================================================
// Plik: inference/mlx.rs
// Opis: Adapter MLX (Apple Silicon) dla lokalnej inferencji modeli safetensors.
//       Implementuje trait InferenceEngine z wykorzystaniem crate mlx-rs,
//       mlx-models (architektury modeli) i tokenizers (BPE tokenizacja).
//       Wszystkie operacje MLX sa wykonywane na JEDNYM dedykowanym watku
//       (singleton mlx-metal thread) — eliminuje problemy z Metal GPU context
//       na iOS, gdzie context jest per-watek.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use mlx_rs::ops::indexing::IndexOp;
use mlx_rs::Array;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::inference::{
    EmbeddingParams, EmbeddingResult, GenerateParams, GenerateResult, InferenceEngine, ModelInfo,
    StopReason, StreamToken,
};
use crate::routing::chat_template::{detect_chat_template, ChatTemplate};

/// Domyslny rozmiar kontekstu dla modeli MLX
const DEFAULT_CTX_SIZE: u32 = 4096;

/// Rozmiar kanalu streamingu tokenow
const STREAM_CHANNEL_SIZE: usize = 64;

/// Konfiguracja modelu wczytana z config.json
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
struct ModelConfig {
    #[serde(default = "default_ctx")]
    max_position_embeddings: u32,
    #[serde(default)]
    vocab_size: u32,
    #[serde(default)]
    hidden_size: u32,
    #[serde(default)]
    num_hidden_layers: u32,
    #[serde(default)]
    num_attention_heads: u32,
    #[serde(default)]
    model_type: Option<String>,
    #[serde(default)]
    torch_dtype: Option<String>,
    #[serde(default)]
    eos_token_id: Option<serde_json::Value>,
    #[serde(default)]
    bos_token_id: Option<u32>,
}

fn default_ctx() -> u32 {
    DEFAULT_CTX_SIZE
}

/// Wrapper na model MLX — enkapsuluje rozne architektury (LLaMA, Mistral, Qwen itp.)
/// Uzywamy trait object z mlx-models, ktory implementuje Module<&Array>
struct MlxModel {
    /// Model zaladowany przez mlx-models — Box<dyn Module> z forward(&Array) -> Array
    inner: Box<dyn MlxForwardPass>,
}

/// Trait abstrakcji forward pass — pozwala na ujednolicenie roznych architektur
trait MlxForwardPass: Send + Sync {
    /// Forward pass: przyjmuje tensor token IDs [batch, seq_len], zwraca logits [batch, seq_len, vocab]
    fn forward(&mut self, input_ids: &Array) -> Result<Array>;

    /// Pobranie hidden states (dla embedddingow) — opcjonalne
    fn hidden_states(&mut self, input_ids: &Array) -> Result<Array>;

    /// Resetuje KV cache — musi byc wolane przed kazdym nowym generowaniem
    fn reset_cache(&mut self);
}

// Typy MLX operuja na Metal GPU — sa thread-safe przez design
unsafe impl Send for MlxModel {}
unsafe impl Sync for MlxModel {}

/// Implementacja forward pass z wykorzystaniem mlx-models
/// Obsluguje modele typu LLaMA, Mistral, Qwen, Qwen3-Next itp.
struct MlxModelsForwardPass {
    /// Zunifikowany wrapper modelu — dispatchuje do odpowiedniej architektury
    model: mlx_models::AnyModel,
    /// KV cache do autoregresyjnego generowania
    cache: mlx_models::AnyCache,
}

unsafe impl Send for MlxModelsForwardPass {}
unsafe impl Sync for MlxModelsForwardPass {}

impl MlxForwardPass for MlxModelsForwardPass {
    fn forward(&mut self, input_ids: &Array) -> Result<Array> {
        let output = self
            .model
            .forward(input_ids, None, &mut self.cache)
            .map_err(|e| anyhow::anyhow!("Blad forward pass MLX: {}", e))?;
        Ok(output)
    }

    fn hidden_states(&mut self, input_ids: &Array) -> Result<Array> {
        // Dla embedddingow uzywamy forward pass i bierzemy output przed lm_head
        let output = self
            .model
            .forward(input_ids, None, &mut self.cache)
            .map_err(|e| anyhow::anyhow!("Blad forward pass MLX (hidden states): {}", e))?;
        Ok(output)
    }

    fn reset_cache(&mut self) {
        self.cache = self.model.make_cache();
    }
}

/// Zaladowany model MLX ze wszystkimi zasobami
#[allow(dead_code)]
struct LoadedModel {
    model_path: PathBuf,
    config: ModelConfig,
    /// Model MLX z forward pass
    model: MlxModel,
    /// Tokenizer BPE z crate tokenizers
    tokenizer: tokenizers::Tokenizer,
    /// EOS token IDs do detekcji konca generowania
    eos_token_ids: Vec<u32>,
    /// Informacje o modelu
    info: ModelInfo,
    /// Wykryty szablon chatu dla tego modelu
    chat_template: ChatTemplate,
}

// Typy MLX operuja na Metal GPU — sa thread-safe przez design
unsafe impl Send for LoadedModel {}
unsafe impl Sync for LoadedModel {}

// =============================================================================
// Singleton watek MLX Metal — wszystkie operacje GPU na jednym watku.
// Na iOS Metal context jest per-watek, wiec load_model i generate musza
// byc na tym samym watku. Rozwiazanie: dedykowany watek z kanalem zadan.
// =============================================================================

/// Zadanie wysylane do dedykowanego watku MLX Metal
enum MlxTask {
    /// Zaladowanie modelu z podanej sciezki
    LoadModel {
        path: PathBuf,
        result_tx: tokio::sync::oneshot::Sender<Result<ModelInfo>>,
    },
    /// Generowanie tekstu (blokujace — caly wynik)
    Generate {
        params: GenerateParams,
        result_tx: tokio::sync::oneshot::Sender<Result<GenerateResult>>,
    },
    /// Generowanie tekstu ze streamingiem tokenow
    GenerateStream {
        params: GenerateParams,
        token_tx: mpsc::Sender<StreamToken>,
        result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Wyladowanie modelu z pamieci
    UnloadModel {
        result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Pobranie informacji o zaladowanym modelu
    GetModelInfo {
        result_tx: tokio::sync::oneshot::Sender<Option<ModelInfo>>,
    },
    /// Obliczenie embedddingow
    ComputeEmbeddings {
        params: EmbeddingParams,
        result_tx: tokio::sync::oneshot::Sender<Result<EmbeddingResult>>,
    },
}

/// Globalny singleton — kanal do wysylania zadan na dedykowany watek MLX Metal
static MLX_THREAD: OnceLock<std::sync::mpsc::Sender<MlxTask>> = OnceLock::new();

/// Zwraca nadawce kanalu do dedykowanego watku MLX Metal.
/// Przy pierwszym wywolaniu tworzy watek i kanal.
fn mlx_sender() -> &'static std::sync::mpsc::Sender<MlxTask> {
    MLX_THREAD.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<MlxTask>();

        std::thread::Builder::new()
            .name("mlx-metal".to_string())
            .spawn(move || {
                info!(
                    "Dedykowany watek MLX Metal uruchomiony (tid={:?})",
                    std::thread::current().id()
                );

                // Test: czy Metal GPU dziala na tym watku?
                let test = Array::from_slice(&[1.0f32, 2.0, 3.0], &[3]);
                let test2 = Array::from_slice(&[10.0f32, 20.0, 30.0], &[3]);
                match test.multiply(&test2) {
                    Ok(result) => {
                        match result.eval() {
                            Ok(_) => {
                                let data = result.as_slice::<f32>();
                                debug!("Test Metal OK: {:?}", data);

                                // Test 2: wieksza operacja — matmul 256x256
                                let big = Array::from_slice(&vec![1.0f32; 256 * 256], &[256, 256]);
                                match big.matmul(&big) {
                                    Ok(result) => match result.eval() {
                                        Ok(_) => {
                                            let s = result.as_slice::<f32>();
                                            debug!("Test Metal 2 OK: matmul wynik[0]={}", s[0]);
                                        }
                                        Err(e) => debug!("Test Metal 2 eval FAILED: {}", e),
                                    },
                                    Err(e) => debug!("Test Metal 2 matmul FAILED: {}", e),
                                }

                                // Test 3: duza operacja — matmul 2048x2048
                                let huge =
                                    Array::from_slice(&vec![0.01f32; 2048 * 2048], &[2048, 2048]);
                                match huge.matmul(&huge) {
                                    Ok(result) => match result.eval() {
                                        Ok(_) => {
                                            let s = result.as_slice::<f32>();
                                            debug!("Test Metal 3 OK: matmul wynik[0]={}", s[0]);
                                        }
                                        Err(e) => debug!("Test Metal 3 eval FAILED: {}", e),
                                    },
                                    Err(e) => debug!("Test Metal 3 matmul FAILED: {}", e),
                                }
                            }
                            Err(e) => debug!("Test Metal eval FAILED: {}", e),
                        }
                    }
                    Err(e) => debug!("Test Metal multiply FAILED: {}", e),
                }

                // Model zyje wylacznie w tym watku — brak wspoldzielenia miedzy watkami
                let mut loaded: Option<LoadedModel> = None;

                for task in rx {
                    match task {
                        MlxTask::LoadModel { path, result_tx } => {
                            info!("LoadModel: {}", path.display());
                            let result = MlxEngine::load_model_sync(&path);
                            match result {
                                Ok(model) => {
                                    let info = model.info.clone();
                                    info!("Model zaladowany: {}", info.name);
                                    loaded = Some(model);
                                    let _ = result_tx.send(Ok(info));
                                }
                                Err(e) => {
                                    info!("Blad ladowania modelu: {}", e);
                                    let _ = result_tx.send(Err(e));
                                }
                            }
                        }
                        MlxTask::Generate { params, result_tx } => {
                            debug!("Generate: max_tokens={}", params.max_tokens);
                            let result = match loaded.as_mut() {
                                Some(m) => MlxEngine::generate_sync(m, &params),
                                None => Err(anyhow::anyhow!("Model MLX nie zaladowany")),
                            };
                            let _ = result_tx.send(result);
                        }
                        MlxTask::GenerateStream {
                            params,
                            token_tx,
                            result_tx,
                        } => {
                            debug!("GenerateStream: max_tokens={}", params.max_tokens);
                            let result = match loaded.as_mut() {
                                Some(m) => MlxEngine::stream_tokens(m, &params, &token_tx),
                                None => Err(anyhow::anyhow!("Model MLX nie zaladowany")),
                            };
                            let _ = result_tx.send(result);
                        }
                        MlxTask::UnloadModel { result_tx } => {
                            if let Some(ref m) = loaded {
                                info!("UnloadModel: {}", m.info.name);
                            }
                            loaded = None;
                            let _ = result_tx.send(Ok(()));
                        }
                        MlxTask::GetModelInfo { result_tx } => {
                            let info = loaded.as_ref().map(|m| m.info.clone());
                            let _ = result_tx.send(info);
                        }
                        MlxTask::ComputeEmbeddings { params, result_tx } => {
                            debug!("ComputeEmbeddings: {} tekstow", params.texts.len());
                            let result = match loaded.as_mut() {
                                Some(m) => MlxEngine::compute_embeddings(m, &params),
                                None => Err(anyhow::anyhow!("Model MLX nie zaladowany")),
                            };
                            let _ = result_tx.send(result);
                        }
                    }
                }

                info!("Watek MLX Metal zakonczony (kanal zamkniety)");
            })
            .expect("Nie udalo sie utworzyc watku MLX Metal");

        tx
    })
}

/// Adapter MLX — lokalna inferencja na Apple Silicon (Metal GPU).
/// Wszystkie operacje sa delegowane na dedykowany singleton watek MLX Metal
/// przez kanal zadan — eliminuje problemy z cross-thread Metal context na iOS.
pub struct MlxEngine;

impl Default for MlxEngine {
    fn default() -> Self {
        Self
    }
}

impl MlxEngine {
    pub fn new() -> Self {
        Self
    }

    /// Parsuje EOS token IDs z roznych formatow w config.json
    fn parse_eos_token_ids(value: &Option<serde_json::Value>) -> Vec<u32> {
        match value {
            Some(serde_json::Value::Number(n)) => {
                n.as_u64().map(|v| vec![v as u32]).unwrap_or_default()
            }
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect(),
            _ => vec![2], // domyslny EOS token
        }
    }

    /// Wczytuje konfiguracje modelu z config.json
    fn load_config(model_dir: &Path) -> Result<ModelConfig> {
        let config_path = model_dir.join("config.json");
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Nie udalo sie wczytac {}", config_path.display()))?;
        let config: ModelConfig =
            serde_json::from_str(&content).with_context(|| "Blad parsowania config.json")?;
        Ok(config)
    }

    /// Wczytuje tokenizer BPE z tokenizer.json
    fn load_tokenizer(model_dir: &Path) -> Result<tokenizers::Tokenizer> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!(
                "Nie udalo sie wczytac tokenizera z {}: {}",
                tokenizer_path.display(),
                e
            )
        })?;

        info!(
            "Tokenizer zaladowany: vocab_size={}",
            tokenizer.get_vocab_size(true),
        );

        Ok(tokenizer)
    }

    /// Laduje model z katalogu uzywajac mlx-models
    fn load_mlx_model(model_dir: &Path, config: &ModelConfig) -> Result<MlxModel> {
        let model_type_str = config.model_type.as_deref().unwrap_or("llama");
        info!("Ladowanie architektury modelu MLX: {}", model_type_str);

        // Wykryj architekture z config.json
        let detected = mlx_models::registry::detect_model_type(model_dir)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie wykryc typu modelu: {}", e))?;

        if !mlx_models::registry::is_supported(&detected) {
            anyhow::bail!("Nieobslugiwana architektura modelu: {}", detected);
        }

        // Zbuduj model na podstawie wykrytej architektury
        let (any_model, cache) = match detected.as_str() {
            "qwen3_next" => {
                // Qwen3-Next — hybrydowy model SSM/attention
                let config_path = model_dir.join("config.json");
                let config_file = std::fs::File::open(&config_path)
                    .with_context(|| format!("Brak config.json w {}", model_dir.display()))?;
                let qwen3_args: mlx_models::qwen3_next::Qwen3NextModelArgs =
                    serde_json::from_reader(config_file)
                        .with_context(|| "Blad parsowania Qwen3-Next config.json")?;
                let mut model = mlx_models::Qwen3NextCausalLM::new(qwen3_args)
                    .map_err(|e| anyhow::anyhow!("Blad tworzenia modelu Qwen3-Next: {}", e))?;
                mlx_models::load_safetensors_weights(&mut model, model_dir)
                    .map_err(|e| anyhow::anyhow!("Blad ladowania wag Qwen3-Next: {}", e))?;
                let any = mlx_models::AnyModel::Qwen3Next(model);
                let cache = any.make_cache();
                (any, cache)
            }
            _ => {
                // Standardowy transformer: qwen2, qwen3, llama, mistral
                // Uzyj transformer::load_model() — zawiera obejscie na embed_tokens
                let model = mlx_models::transformer::load_model(model_dir)
                    .map_err(|e| anyhow::anyhow!("Blad ladowania modelu {}: {}", detected, e))?;
                let any = mlx_models::AnyModel::Transformer(model);
                let cache = any.make_cache();
                (any, cache)
            }
        };

        info!(
            "Model MLX zaladowany pomyslnie (architektura: {})",
            detected
        );

        Ok(MlxModel {
            inner: Box::new(MlxModelsForwardPass {
                model: any_model,
                cache,
            }),
        })
    }

    /// Tokenizuje tekst do tablicy token IDs
    fn tokenize(tokenizer: &tokenizers::Tokenizer, text: &str, add_bos: bool) -> Result<Vec<u32>> {
        let encoding = tokenizer
            .encode(text, add_bos)
            .map_err(|e| anyhow::anyhow!("Blad tokenizacji: {}", e))?;
        Ok(encoding.get_ids().to_vec())
    }

    /// OPT-3: Uproszczony decode_incremental z poprawna obsluga granic znakow UTF-8.
    /// Dekoduje calkowity wygenerowany tekst i zwraca roznice wzgledem prev_text.
    #[allow(dead_code)]
    fn decode_incremental(
        tokenizer: &tokenizers::Tokenizer,
        all_generated_ids: &[u32],
        prev_text: &str,
    ) -> String {
        let full = tokenizer
            .decode(all_generated_ids, true)
            .unwrap_or_default();
        if full.len() > prev_text.len() && full.is_char_boundary(prev_text.len()) {
            full[prev_text.len()..].to_string()
        } else if full.len() > prev_text.len() {
            // Fallback: znajdz najblizszy poprawny boundary UTF-8
            let start = (prev_text.len()..full.len())
                .find(|&i| full.is_char_boundary(i))
                .unwrap_or(full.len());
            full[start..].to_string()
        } else {
            String::new()
        }
    }

    /// CPU sampling — softmax + losowanie z dystrybucji (fallback gdy GPU categorical nie dziala)
    fn sample_token_cpu(logits: &[f32], temperature: f32, top_p: f32) -> u32 {
        if logits.is_empty() {
            return 0;
        }

        // Zastosuj temperature
        let inv_temp = 1.0 / temperature;
        let max_val = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        // Softmax z temperature
        let mut sum = 0.0f32;
        let mut probs: Vec<(usize, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, &l)| {
                let p = ((l - max_val) * inv_temp).exp();
                sum += p;
                (i, p)
            })
            .collect();

        // Normalizacja
        for p in &mut probs {
            p.1 /= sum;
        }

        // Top-P (nucleus) — filtrowanie
        if top_p < 1.0 {
            probs.sort_unstable_by(|a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut cumsum = 0.0f32;
            let cutoff = probs
                .iter()
                .position(|&(_, p)| {
                    cumsum += p;
                    cumsum >= top_p
                })
                .map(|i| i + 1)
                .unwrap_or(probs.len());
            probs.truncate(cutoff);
            // Renormalizacja
            let total: f32 = probs.iter().map(|&(_, p)| p).sum();
            for p in &mut probs {
                p.1 /= total;
            }
        }

        // Losowanie
        let r: f32 = rand::random::<f32>();
        let mut cumsum = 0.0f32;
        for &(idx, prob) in &probs {
            cumsum += prob;
            if cumsum >= r {
                return idx as u32;
            }
        }

        probs.last().map(|&(i, _)| i as u32).unwrap_or(0)
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

    /// Wyodrebnia logits z ostatniej pozycji jako MLX Array [1, vocab].
    /// Zostaje na GPU — nie kopiuje na CPU.
    fn extract_last_logits_gpu(logits_array: &Array) -> Result<Array> {
        let shape = logits_array.shape();

        let sliced = match shape.len() {
            3 => {
                let vocab = shape[2];
                let seq_len = shape[1];
                let last_pos = logits_array.index((0, seq_len - 1));
                last_pos
                    .reshape(&[1, vocab])
                    .context("Blad reshape logits do [1, vocab]")?
            }
            2 => {
                let vocab = shape[1];
                let seq_len = shape[0];
                let last_pos = logits_array.index(seq_len - 1);
                last_pos
                    .reshape(&[1, vocab])
                    .context("Blad reshape logits do [1, vocab]")?
            }
            _ => anyhow::bail!("Nieoczekiwany ksztalt tensora logits: {:?}", shape,),
        };

        // Konwertuj Bfloat16 -> Float32 (sampling wymaga f32)
        if sliced.dtype() != mlx_rs::Dtype::Float32 {
            sliced
                .as_dtype(mlx_rs::Dtype::Float32)
                .context("Blad konwersji logits na Float32")
        } else {
            Ok(sliced)
        }
    }

    /// Konwertuje liste token IDs na tensor MLX Array [1, seq_len]
    fn tokens_to_array(token_ids: &[u32]) -> Result<Array> {
        let ids_i32: Vec<i32> = token_ids.iter().map(|&id| id as i32).collect();
        let len = ids_i32.len();
        let array = Array::from_slice(&ids_i32, &[1, len as i32]);
        Ok(array)
    }

    /// Laduje model z katalogu (synchronicznie) — wolane TYLKO z dedykowanego watku mlx-metal
    fn load_model_sync(model_dir: &Path) -> Result<LoadedModel> {
        info!("Ladowanie modelu MLX z: {}", model_dir.display());

        // Wczytaj konfiguracje
        let config = Self::load_config(model_dir)?;
        info!(
            "Konfiguracja modelu: type={:?}, hidden_size={}, layers={}, heads={}, vocab={}",
            config.model_type,
            config.hidden_size,
            config.num_hidden_layers,
            config.num_attention_heads,
            config.vocab_size,
        );

        // Wczytaj tokenizer BPE
        let tokenizer = Self::load_tokenizer(model_dir)?;

        // Wczytaj model z mlx-models
        let model = Self::load_mlx_model(model_dir, &config)?;

        // Parsuj EOS token IDs
        let eos_token_ids = Self::parse_eos_token_ids(&config.eos_token_id);
        debug!("EOS token IDs: {:?}", eos_token_ids);

        // Oblicz rozmiar modelu na dysku (suma plikow safetensors)
        let size_bytes: u64 = std::fs::read_dir(model_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "safetensors")
                    .unwrap_or(false)
            })
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();

        // Oszacuj liczbe parametrow na podstawie rozmiaru (zakladamy fp16 = 2 bajty/param)
        let estimated_params = size_bytes / 2;

        let ctx_length = config.max_position_embeddings;

        // Wykryj szablon chatu na podstawie tokenizer_config.json
        let chat_template = detect_chat_template(model_dir);
        info!("Wykryty szablon chatu: {:?}", chat_template);

        // Okreslenie kwantyzacji z torch_dtype lub nazwy katalogu
        let quantization = config.torch_dtype.clone().or_else(|| {
            model_dir
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|name| {
                    let upper = name.to_uppercase();
                    ["4BIT", "8BIT", "FP16", "FP32", "BF16", "INT4", "INT8"]
                        .iter()
                        .find(|q| upper.contains(*q))
                        .map(|q| q.to_string())
                })
        });

        let info = ModelInfo {
            name: model_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
            path: model_dir.to_string_lossy().to_string(),
            size_bytes,
            parameters: format!("{}M", estimated_params / 1_000_000),
            quantization,
            context_length: ctx_length,
            loaded: true,
            vram_used_mb: size_bytes / (1024 * 1024),
            backend: "mlx".to_string(),
            chat_template: Some(chat_template.name().to_string()),
        };

        info!(
            "Model zaladowany: {} ({}MB, ctx={}, vocab={})",
            info.name,
            info.vram_used_mb,
            info.context_length,
            tokenizer.get_vocab_size(true),
        );

        Ok(LoadedModel {
            model_path: model_dir.to_path_buf(),
            config,
            model,
            tokenizer,
            eos_token_ids,
            info,
            chat_template,
        })
    }

    /// OPT-6: Wspolna logika generowania tokenow — eliminuje duplikacje miedzy generate_sync i stream_tokens.
    /// Callback on_token zwraca true jesli kontynuowac, false jesli przerwac (np. kanal zamkniety).
    /// Wolane TYLKO z dedykowanego watku mlx-metal.
    fn generate_loop<F>(
        loaded: &mut LoadedModel,
        params: &GenerateParams,
        mut on_token: F,
    ) -> Result<GenerateResult>
    where
        F: FnMut(u32, &str) -> bool,
    {
        // VULN-040: Ograniczenie max_tokens do 32768 — ochrona przed nadmiernym zuzbyciem zasobow
        let max_tokens = params.max_tokens.min(32768);

        // Reszta logiki generowania uzywa max_tokens zamiast params.max_tokens
        let start = Instant::now();

        // Reset KV cache — kazde generowanie zaczyna od zera
        loaded.model.inner.reset_cache();

        // OPT-4: Uzywaj &params.prompt bezposrednio — bez klonowania
        let input_tokens = Self::tokenize(&loaded.tokenizer, &params.prompt, true)?;
        let prompt_tokens = input_tokens.len() as u32;
        debug!(
            "generate_loop: prompt {} znakow -> {} tokenow, max_tokens={}",
            params.prompt.len(),
            prompt_tokens,
            max_tokens
        );

        // OPT-4: Zamiast current_tokens: Vec<u32> sledzmy tylko licznik i ostatni token
        let mut total_token_count: usize = input_tokens.len();
        let mut last_token_id: u32 = *input_tokens.last().unwrap_or(&0);
        let mut generated_text = String::new();
        let mut prev_decoded = String::new();
        let mut generated_ids: Vec<u32> = Vec::new();
        let mut generated_tokens: u32 = 0;
        let mut stop_reason = StopReason::MaxTokens;
        let mut ttft: Option<std::time::Duration> = None;
        let mut decode_start: Option<Instant> = None;

        // Autoregresyjna petla generowania
        for step in 0..max_tokens {
            // Przygotuj tensor wejsciowy
            let input_array = if step == 0 {
                // Pierwszy krok — caly prompt (OPT-4: bez klonowania input_tokens)
                Self::tokens_to_array(&input_tokens)?
            } else {
                // Kolejne kroki — tylko ostatni wygenerowany token
                Self::tokens_to_array(&[last_token_id])?
            };

            // Forward pass przez model
            if step == 0 {
                debug!("prefill start ({} tokenow)...", input_tokens.len());
            }
            let logits_array = match loaded.model.inner.forward(&input_array) {
                Ok(arr) => arr,
                Err(e) => {
                    return Err(anyhow::anyhow!("Blad forward pass w kroku {}: {}", step, e));
                }
            };

            // eval() — wymusza obliczenie na GPU Metal
            // Materializacja przez odczyt rozmiaru — wymusza eval bez kopiowania danych
            let _shape = logits_array.shape();

            if step == 0 {
                debug!(
                    "prefill DONE w {:.1}ms",
                    start.elapsed().as_secs_f64() * 1000.0
                );
            }

            // Wyciagnij logits z ostatniej pozycji
            let last_logits = Self::extract_last_logits_gpu(&logits_array)?;

            // OPT-7: Repeat penalty — aplikujemy na GPU array przed samplowaniem
            let penalized_logits = if params.repeat_penalty != 1.0 && !generated_ids.is_empty() {
                Self::apply_repeat_penalty_gpu(&last_logits, &generated_ids, params.repeat_penalty)?
            } else {
                last_logits
            };

            // GPU sampling — mlx_models::sample dziala calkowicie na GPU
            // Unikamy as_slice() bo na iOS moze wisiec przy transferze 32k floatow
            let token_array =
                mlx_models::sample(&penalized_logits, params.temperature, params.top_p)
                    .map_err(|e| anyhow::anyhow!("Blad samplowania GPU: {}", e))?;
            // item() kopiuje tylko 1 skalar GPU->CPU (4 bajty zamiast 128KB)
            let next_token: u32 = if params.temperature <= 0.0 {
                token_array.item::<i32>() as u32
            } else {
                token_array.item::<u32>()
            };
            // Sprawdz EOS
            if loaded.eos_token_ids.contains(&next_token) {
                stop_reason = StopReason::EndOfText;
                debug!("Osiagnieto EOS token ({}) w kroku {}", next_token, step);
                break;
            }

            // TTFT — rejestruj moment wygenerowania pierwszego tokena
            if ttft.is_none() {
                ttft = Some(start.elapsed());
                decode_start = Some(Instant::now());
                debug!("TTFT: {:.1}ms", ttft.unwrap().as_secs_f64() * 1000.0);
            }

            // Dekoduj pelny tekst i wyciagnij nowy fragment (roznica)
            // SentencePiece moze zmieniac wczesniejszy tekst przy dodaniu kontekstu,
            // wiec zawsze pelny dekod jest zrodlem prawdy
            generated_ids.push(next_token);
            let full_decoded = loaded
                .tokenizer
                .decode(&generated_ids, true)
                .unwrap_or_default();
            let piece = if full_decoded.len() >= prev_decoded.len()
                && full_decoded.is_char_boundary(prev_decoded.len())
                && full_decoded.starts_with(&prev_decoded)
            {
                full_decoded[prev_decoded.len()..].to_string()
            } else if full_decoded.len() > prev_decoded.len() {
                // SentencePiece zmienil wczesniejszy tekst lub granica UTF-8
                // Szukaj wspolnego prefiksu i emituj roznice
                let common = prev_decoded
                    .chars()
                    .zip(full_decoded.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                let common_bytes: usize = full_decoded
                    .chars()
                    .take(common)
                    .map(|c| c.len_utf8())
                    .sum();
                if common_bytes < full_decoded.len() && full_decoded.is_char_boundary(common_bytes)
                {
                    full_decoded[common_bytes..].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            // Usun replacement characters (U+FFFD) — model moze generowac
            // tokeny mapujace na niekompletne sekwencje bajtow
            let piece = piece.replace('\u{FFFD}', "");
            generated_text = full_decoded.replace('\u{FFFD}', "");
            prev_decoded = generated_text.clone();
            generated_tokens += 1;

            // OPT-4: Aktualizuj licznik i ostatni token zamiast Vec::push
            last_token_id = next_token;
            total_token_count += 1;

            // Callback — jesli zwroci false, konczymy (np. kanal zamkniety)
            if !on_token(next_token, &piece) {
                debug!("Callback on_token zwrocil false — konczenie generowania");
                stop_reason = StopReason::MaxTokens;
                break;
            }

            // Sprawdz stop sequences
            if let Some(matched) =
                Self::check_stop_sequence(&generated_text, &params.stop_sequences)
            {
                let trim_len = matched.len();
                let new_len = generated_text.len() - trim_len;
                generated_text.truncate(new_len);
                stop_reason = StopReason::StopSequence(matched.to_string());
                debug!("Zatrzymano na stop sequence: '{}'", matched);
                break;
            }

            // Sprawdz limit kontekstu
            if total_token_count as u32 >= loaded.info.context_length {
                debug!("Osiagnieto limit kontekstu: {}", loaded.info.context_length);
                stop_reason = StopReason::MaxTokens;
                break;
            }
        }

        let total_elapsed = start.elapsed();
        let total_time_ms = total_elapsed.as_millis() as u64;
        let ttft_ms = ttft.map(|d| d.as_millis() as u64);

        // tok/s liczone od momentu 1-szego tokena (bez prefill) — tak jak standard w AI
        let decode_elapsed = decode_start
            .map(|ds| ds.elapsed().as_secs_f64())
            .unwrap_or(total_elapsed.as_secs_f64());
        let tokens_per_second = if decode_elapsed > 0.0 && generated_tokens > 1 {
            // -1 bo pierwszy token jest czescia TTFT, liczymy od 2. tokena
            (generated_tokens - 1) as f64 / decode_elapsed
        } else if decode_elapsed > 0.0 && generated_tokens == 1 {
            1.0 / decode_elapsed
        } else {
            0.0
        };

        info!(
            "Generowanie: {} tok w {:.2}s ({:.1} tok/s), TTFT={:?}ms, prefill={}tok, powod: {:?}",
            generated_tokens,
            total_elapsed.as_secs_f64(),
            tokens_per_second,
            ttft_ms,
            prompt_tokens,
            stop_reason,
        );

        Ok(GenerateResult {
            text: generated_text,
            tokens_generated: generated_tokens,
            tokens_per_second,
            prompt_tokens,
            stop_reason,
            time_to_first_token_ms: ttft_ms,
            total_time_ms: Some(total_time_ms),
        })
    }

    /// OPT-7: Aplikuje repeat penalty na GPU array logits.
    /// Tworzy mask z karami dla tokenow ktore juz wystapily i mnozy/dzieli logits.
    fn apply_repeat_penalty_gpu(
        logits: &Array,
        generated_ids: &[u32],
        penalty: f32,
    ) -> Result<Array> {
        // Kopiuj logits na CPU, aplikuj penalty, wroc na GPU
        // (pelna implementacja GPU penalty wymagalaby scatter ops — zbyt skomplikowane)
        let mut logits_cpu = logits.as_slice::<f32>().to_vec();

        for &id in generated_ids {
            let idx = id as usize;
            if idx < logits_cpu.len() {
                // Dla dodatnich logits — dziel przez penalty (zmniejsz prawdop.)
                // Dla ujemnych logits — mnoz przez penalty (zmniejsz prawdop.)
                if logits_cpu[idx] > 0.0 {
                    logits_cpu[idx] /= penalty;
                } else {
                    logits_cpu[idx] *= penalty;
                }
            }
        }

        // Stworzenie nowego array na GPU z zmodyfikowanymi logits
        let shape = logits.shape();
        let result = Array::from_slice(&logits_cpu, &[shape[0], shape[1]]);

        Ok(result)
    }

    /// Generuje tekst synchronicznie — deleguje do generate_loop z pustym callbackiem.
    /// Wolane TYLKO z dedykowanego watku mlx-metal.
    fn generate_sync(loaded: &mut LoadedModel, params: &GenerateParams) -> Result<GenerateResult> {
        Self::generate_loop(loaded, params, |_token_id, _piece| true)
    }

    /// Generuje tokeny i wysyla je przez kanal — deleguje do generate_loop z callbackiem streamingu.
    /// Wolane TYLKO z dedykowanego watku mlx-metal.
    fn stream_tokens(
        loaded: &mut LoadedModel,
        params: &GenerateParams,
        tx: &mpsc::Sender<StreamToken>,
    ) -> Result<()> {
        let result = Self::generate_loop(loaded, params, |_token_id, piece| {
            // Wyslij token — jesli odbiorca zamknal kanal, konczymy
            tx.blocking_send(StreamToken {
                text: piece.to_string(),
                is_final: false,
            })
            .is_ok()
        })?;

        // Wyslij koncowy token
        let _ = tx.blocking_send(StreamToken {
            text: String::new(),
            is_final: true,
        });

        // Loguj wynik streamingu
        debug!(
            "Stream zakonczony: {} tokenow, powod: {:?}",
            result.tokens_generated, result.stop_reason,
        );

        Ok(())
    }

    /// Oblicza embeddingi dla listy tekstow.
    /// Wolane TYLKO z dedykowanego watku mlx-metal.
    fn compute_embeddings(
        loaded: &mut LoadedModel,
        params: &EmbeddingParams,
    ) -> Result<EmbeddingResult> {
        let hidden_size = loaded.config.hidden_size as usize;
        if hidden_size == 0 {
            anyhow::bail!("Model nie udostepnia hidden_size — embeddingi niedostepne");
        }

        let mut all_embeddings = Vec::with_capacity(params.texts.len());

        for text in &params.texts {
            let tokens = Self::tokenize(&loaded.tokenizer, text, true)?;
            let input_array = Self::tokens_to_array(&tokens)?;

            // Forward pass — pobranie hidden states
            let output = loaded
                .model
                .inner
                .hidden_states(&input_array)
                .with_context(|| "Blad forward pass dla embedddingow")?;

            // Mean pooling po wymiarze sekwencji
            let shape = output.shape();
            let embedding = if shape.len() == 3 {
                // [batch, seq_len, hidden_size] — usredniamy po seq_len
                let mean = output
                    .mean_axes(&[1], false)
                    .map_err(|e| anyhow::anyhow!("Blad mean pooling: {}", e))?;
                mean.as_slice::<f32>().to_vec()
            } else if shape.len() == 2 {
                // [seq_len, hidden_size]
                let mean = output
                    .mean_axes(&[0], false)
                    .map_err(|e| anyhow::anyhow!("Blad mean pooling: {}", e))?;
                mean.as_slice::<f32>().to_vec()
            } else {
                anyhow::bail!("Nieoczekiwany ksztalt hidden states: {:?}", shape,);
            };

            // Normalizacja L2 jesli wymagana
            let mut final_embedding = embedding;
            if params.normalize {
                let norm: f32 = final_embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for val in &mut final_embedding {
                        *val /= norm;
                    }
                }
            }

            all_embeddings.push(final_embedding);
        }

        Ok(EmbeddingResult {
            embeddings: all_embeddings,
            dimensions: hidden_size,
        })
    }
}

#[async_trait]
impl InferenceEngine for MlxEngine {
    fn backend_name(&self) -> &str {
        "mlx"
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["safetensors".to_string(), "mlx".to_string()]
    }

    async fn load_model(&self, model_path: &Path, _gpu_layers: Option<u32>) -> Result<ModelInfo> {
        let path = model_path.to_path_buf();

        // Walidacja: czy to katalog z wymaganymi plikami
        if !path.is_dir() {
            anyhow::bail!(
                "Sciezka modelu MLX musi wskazywac na katalog z config.json, tokenizer.json i plikami .safetensors: {}",
                path.display(),
            );
        }

        if !path.join("config.json").exists() {
            anyhow::bail!("Brak config.json w katalogu modelu: {}", path.display());
        }

        if !path.join("tokenizer.json").exists() {
            anyhow::bail!("Brak tokenizer.json w katalogu modelu: {}", path.display());
        }

        info!("Ladowanie modelu MLX: {}", path.display());

        // Wyslij zadanie na dedykowany watek MLX Metal i czekaj na wynik
        let (tx, rx) = tokio::sync::oneshot::channel();
        mlx_sender()
            .send(MlxTask::LoadModel {
                path,
                result_tx: tx,
            })
            .map_err(|_| anyhow::anyhow!("Watek MLX Metal nie odpowiada — kanal zamkniety"))?;

        let info = rx
            .await
            .context("Watek MLX Metal zakonczyl sie nieoczekiwanie podczas ladowania modelu")?
            .context("Nie udalo sie zaladowac modelu MLX")?;

        info!("Model MLX zaladowany pomyslnie: {}", info.name);
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        mlx_sender()
            .send(MlxTask::UnloadModel { result_tx: tx })
            .map_err(|_| anyhow::anyhow!("Watek MLX Metal nie odpowiada — kanal zamkniety"))?;

        rx.await
            .context("Watek MLX Metal zakonczyl sie nieoczekiwanie podczas wyladowywania modelu")?
    }

    fn model_info(&self) -> Option<ModelInfo> {
        // Synchroniczne zapytanie do watku MLX — uzywamy std::sync::mpsc
        // Nie mozemy uzyc tokio oneshot w synchronicznym kontekscie,
        // wiec uzywamy std::sync::mpsc::channel jako oneshot
        let (tx, rx) = std::sync::mpsc::channel();
        let oneshot_tx = {
            // Konwersja: std::sync::mpsc -> tokio::sync::oneshot
            // Uzywamy dedykowanego tokio oneshot i blokujacego odbioru
            let (otx, orx) = tokio::sync::oneshot::channel();
            // Spawn watek ktory odbierze z tokio oneshot i przesle do std mpsc
            let tx_clone = tx;
            std::thread::spawn(move || {
                if let Ok(result) = orx.blocking_recv() {
                    let _ = tx_clone.send(result);
                }
            });
            otx
        };

        if mlx_sender()
            .send(MlxTask::GetModelInfo {
                result_tx: oneshot_tx,
            })
            .is_err()
        {
            return None;
        }

        // Czekamy max 1 sekunde na odpowiedz
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .ok()
            .flatten()
    }

    async fn generate(&self, params: GenerateParams) -> Result<GenerateResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        mlx_sender()
            .send(MlxTask::Generate {
                params,
                result_tx: tx,
            })
            .map_err(|_| anyhow::anyhow!("Watek MLX Metal nie odpowiada — kanal zamkniety"))?;

        rx.await
            .context("Watek MLX Metal zakonczyl sie nieoczekiwanie podczas generowania")?
    }

    async fn generate_stream(&self, params: GenerateParams) -> Result<mpsc::Receiver<StreamToken>> {
        let (token_tx, token_rx) = mpsc::channel::<StreamToken>(STREAM_CHANNEL_SIZE);
        let (done_tx, _done_rx) = tokio::sync::oneshot::channel();

        mlx_sender()
            .send(MlxTask::GenerateStream {
                params,
                token_tx,
                result_tx: done_tx,
            })
            .map_err(|_| anyhow::anyhow!("Watek MLX Metal nie odpowiada — kanal zamkniety"))?;

        // Nie czekamy na done_rx — tokeny przyjda asynchronicznie przez token_rx.
        // done_rx zostanie rozwiazany po zakonczeniu generowania na watku mlx-metal.
        Ok(token_rx)
    }

    async fn embeddings(&self, params: EmbeddingParams) -> Result<EmbeddingResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        mlx_sender()
            .send(MlxTask::ComputeEmbeddings {
                params,
                result_tx: tx,
            })
            .map_err(|_| anyhow::anyhow!("Watek MLX Metal nie odpowiada — kanal zamkniety"))?;

        rx.await.context(
            "Watek MLX Metal zakonczyl sie nieoczekiwanie podczas obliczania embedddingow",
        )?
    }
}
