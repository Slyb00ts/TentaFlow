// =============================================================================
// Plik: llama.rs
// Opis: Typy i punkty wejścia wrappera TentaFlow dla llama.cpp.
// Przykład: let config = LlamaLoadConfig::default();
// =============================================================================

use std::collections::HashMap;
#[cfg(feature = "llama")]
use std::ffi::CStr;
use std::io::{Read, Seek, SeekFrom};
#[cfg(feature = "llama")]
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
#[cfg(feature = "llama")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "llama")]
use std::sync::{Mutex, Once, OnceLock};

use serde::{Deserialize, Serialize};

use crate::native::{NativeError, NativeLayout};

#[cfg(feature = "llama")]
pub use llama_cpp_sys_2 as sys;

pub const ENGINE_ID: &str = "llama-cpp";
pub const DEFAULT_CTX_SIZE: u32 = 4096;
pub const DEFAULT_GPU_LAYERS: u32 = 99;
pub const DEFAULT_BATCH_SIZE: u32 = 512;
pub const DEFAULT_MTP_TOKENS: u32 = 4;
pub const DEFAULT_FLASH_ATTN: FlashAttentionMode = FlashAttentionMode::Auto;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaGgufInfo {
    pub path: PathBuf,
    pub name: String,
    pub architecture: Option<String>,
    pub size_bytes: u64,
    pub tensor_count: u64,
    pub metadata_count: u64,
    pub context_length: Option<u64>,
    pub embedding_length: Option<u64>,
    pub vocab_size: Option<u64>,
    pub quantization_version: Option<u64>,
    pub mtp_layers: u32,
}

impl LlamaGgufInfo {
    pub fn supports_mtp(&self) -> bool {
        self.mtp_layers > 0
    }
}

// Specjalna wartosc progu oznaczajaca "nie drukuj nic" (sentinel powyzej CONT=5).
// Pozwala odroznic tryb `none` od poziomu ERROR bez dodatkowego flagu.
#[cfg(feature = "llama")]
const LOG_THRESHOLD_SILENT: u32 = u32::MAX;

// Globalny prog poziomu logow llama.cpp/ggml. Inicjalizowany RAZ z env
// `TENTAFLOW_LLAMA_LOG_LEVEL`; `LOG_THRESHOLD_SILENT` = pelne wyciszenie.
#[cfg(feature = "llama")]
static LOG_THRESHOLD: AtomicU32 = AtomicU32::new(sys::GGML_LOG_LEVEL_WARN);

#[cfg(feature = "llama")]
static LOG_THRESHOLD_INIT: Once = Once::new();

// Czyta env raz i ustawia globalny prog. Domyslnie WARN, co wycina spam
// `GGML_LOG_LEVEL_DEBUG` (per-tokenowe "CUDA Graph id N reused", "warmup complete/reset")
// oraz INFO, zachowujac WARN i ERROR.
#[cfg(feature = "llama")]
fn ensure_log_threshold() {
    LOG_THRESHOLD_INIT.call_once(|| {
        let threshold = match std::env::var("TENTAFLOW_LLAMA_LOG_LEVEL") {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "none" => LOG_THRESHOLD_SILENT,
                "error" => sys::GGML_LOG_LEVEL_ERROR,
                "warn" => sys::GGML_LOG_LEVEL_WARN,
                "info" => sys::GGML_LOG_LEVEL_INFO,
                "debug" => sys::GGML_LOG_LEVEL_DEBUG,
                _ => sys::GGML_LOG_LEVEL_WARN,
            },
            Err(_) => sys::GGML_LOG_LEVEL_WARN,
        };
        LOG_THRESHOLD.store(threshold, Ordering::Relaxed);
    });
}

// Callback filtrujacy wspolny dla obu kanalow (llama_log_set + ggml_log_set).
// Drukuje na stderr tylko gdy poziom >= prog; CONT (kontynuacja poprzedniej linii)
// traktujemy jak zwykly poziom wobec progu. ggml dostarcza wlasny `\n`, wiec
// uzywamy `eprint!` bez dodatkowego znaku konca linii.
#[cfg(feature = "llama")]
unsafe extern "C" fn filtered_log(
    level: sys::ggml_log_level,
    text: *const std::os::raw::c_char,
    _user_data: *mut std::os::raw::c_void,
) {
    let threshold = LOG_THRESHOLD.load(Ordering::Relaxed);
    if threshold == LOG_THRESHOLD_SILENT || level < threshold {
        return;
    }
    if text.is_null() {
        return;
    }
    let message = CStr::from_ptr(text).to_string_lossy();
    if message.is_empty() {
        return;
    }
    eprint!("{message}");
}

// Instaluje filtr poziomu na OBU kanalach logow. Kanal ggml (`ggml_log_set`)
// jest odpowiedzialny za per-tokenowe komunikaty CUDA Graph z `ggml-cuda.cu`,
// ktorych `llama_log_set` nie obejmuje. Idempotentne.
#[cfg(feature = "llama")]
pub fn install_llama_log_filter() {
    ensure_log_threshold();
    unsafe {
        sys::llama_log_set(Some(filtered_log), std::ptr::null_mut());
        sys::ggml_log_set(Some(filtered_log), std::ptr::null_mut());
    }
}

// Pelne wyciszenie obu kanalow dla przykladow chcacych absolutnej ciszy.
#[cfg(feature = "llama")]
pub fn silence_llama_logs() {
    unsafe {
        sys::llama_log_set(Some(ignore_llama_log), std::ptr::null_mut());
        sys::ggml_log_set(Some(ignore_llama_log), std::ptr::null_mut());
    }
}

#[cfg(feature = "llama")]
unsafe extern "C" fn ignore_llama_log(
    _level: sys::ggml_log_level,
    _text: *const std::os::raw::c_char,
    _user_data: *mut std::os::raw::c_void,
) {
}

#[cfg(feature = "llama")]
static LLAMA_LOG_CAPTURE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(feature = "llama")]
#[derive(Default)]
struct LlamaLogCapture {
    lines: Vec<String>,
}

#[cfg(feature = "llama")]
impl LlamaLogCapture {
    fn push(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                self.lines.push(line.to_string());
            }
        }
        if self.lines.len() > 64 {
            let drain = self.lines.len() - 64;
            self.lines.drain(0..drain);
        }
    }

    fn summary(&self) -> String {
        if self.lines.is_empty() {
            return "brak szczegółów z llama.cpp".to_string();
        }

        let start = self.lines.len().saturating_sub(12);
        self.lines[start..].join("\n")
    }
}

#[cfg(feature = "llama")]
unsafe extern "C" fn capture_llama_log(
    _level: sys::ggml_log_level,
    text: *const c_char,
    user_data: *mut c_void,
) {
    if text.is_null() || user_data.is_null() {
        return;
    }

    let capture = &mut *(user_data as *mut LlamaLogCapture);
    capture.push(&CStr::from_ptr(text).to_string_lossy());
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaArtifacts {
    pub include_dir: PathBuf,
    pub static_dir: PathBuf,
    pub dynamic_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaModelMetadata {
    pub name: String,
    pub size_bytes: u64,
    pub parameters: u64,
    pub context_train: u32,
    pub vocab_size: u32,
    pub embedding_size: u32,
    pub quantization: Option<String>,
    pub mtp_layers: u32,
}

impl LlamaModelMetadata {
    pub fn supports_mtp(&self) -> bool {
        self.mtp_layers > 0
    }
}

impl LlamaArtifacts {
    pub fn discover() -> Result<Self, NativeError> {
        Self::from_layout(&NativeLayout::discover()?, LlamaVariant::Multi)
    }

    pub fn from_layout(layout: &NativeLayout, variant: LlamaVariant) -> Result<Self, NativeError> {
        let include_dir = layout.include_dir().join("llama");
        layout.require_file(include_dir.join("llama.h"))?;
        layout.require_file(include_dir.join("common").join("common.h"))?;
        layout.require_file(include_dir.join("common").join("chat.h"))?;

        let static_dir = layout
            .static_dir()
            .join("llama-cpp")
            .join(variant.as_dir_name());
        layout.require_file(static_dir.join(static_library_name("llama")))?;
        layout.require_file(static_dir.join(static_library_name("ggml")))?;

        Ok(Self {
            include_dir,
            static_dir,
            dynamic_dir: layout
                .dynamic_dir()
                .join("llama-cpp")
                .join(variant.as_dir_name()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlamaVariant {
    Multi,
    Cpu,
    Cuda,
    Vulkan,
    Rocm,
    Metal,
}

impl LlamaVariant {
    pub fn as_dir_name(self) -> &'static str {
        match self {
            Self::Multi => "multi",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Rocm => "rocm",
            Self::Metal => "metal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlashAttentionMode {
    Auto,
    Off,
    On,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlamaLoadConfig {
    pub ctx_size: u32,
    pub n_gpu_layers: u32,
    pub batch_size: u32,
    pub threads: Option<u32>,
    pub flash_attn: FlashAttentionMode,
    // Indeks karty głównej (skupia bufory pomocnicze). Wybór kart embedded idzie
    // przez te pola, bo jeden proces core inicjalizuje CUDA raz i nie reaguje na
    // CUDA_VISIBLE_DEVICES ustawiane po starcie.
    pub main_gpu: i32,
    // Wagi rozkładu warstw na karty (long. = liczba kart). Pusty = domyślny
    // rozkład na wszystkie karty. Waga 0.0 dla karty wyklucza ją z użycia, więc
    // to jedyny sposób zawężenia embedded llama.cpp do podzbioru kart.
    pub tensor_split: Vec<f32>,
}

impl Default for LlamaLoadConfig {
    fn default() -> Self {
        Self {
            ctx_size: DEFAULT_CTX_SIZE,
            n_gpu_layers: DEFAULT_GPU_LAYERS,
            batch_size: DEFAULT_BATCH_SIZE,
            threads: None,
            flash_attn: DEFAULT_FLASH_ATTN,
            main_gpu: 0,
            tensor_split: Vec::new(),
        }
    }
}

impl LlamaLoadConfig {
    pub fn from_deploy_map(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self::from_value_reader(|key| map.get(key))
    }

    pub fn from_deploy_hash_map(map: &HashMap<String, serde_json::Value>) -> Self {
        Self::from_value_reader(|key| map.get(key))
    }

    fn from_value_reader<'a>(read: impl Fn(&str) -> Option<&'a serde_json::Value>) -> Self {
        let mut config = Self::default();
        if let Some(value) = read("ctx_size").and_then(|v| v.as_u64()) {
            config.ctx_size = value as u32;
        }
        if let Some(value) = read("n_gpu_layers").and_then(|v| v.as_u64()) {
            config.n_gpu_layers = value as u32;
        }
        if let Some(value) = read("batch_size").and_then(|v| v.as_u64()) {
            config.batch_size = value as u32;
        }
        config.threads = read("threads").and_then(|v| v.as_u64()).map(|v| v as u32);
        if let Some(value) = read("flash_attn").and_then(parse_flash_attention_mode) {
            config.flash_attn = value;
        }
        if let Some(value) = read("main_gpu").and_then(|v| v.as_i64()) {
            config.main_gpu = value as i32;
        }
        if let Some(array) = read("tensor_split").and_then(|v| v.as_array()) {
            config.tensor_split = array
                .iter()
                .filter_map(|v| v.as_f64())
                .map(|v| v as f32)
                .collect();
        }
        config
    }
}

fn parse_flash_attention_mode(value: &serde_json::Value) -> Option<FlashAttentionMode> {
    if let Some(enabled) = value.as_bool() {
        return Some(if enabled {
            FlashAttentionMode::On
        } else {
            FlashAttentionMode::Off
        });
    }

    match value.as_str()? {
        "auto" => Some(FlashAttentionMode::Auto),
        "off" | "false" | "disabled" => Some(FlashAttentionMode::Off),
        "on" | "true" | "enabled" => Some(FlashAttentionMode::On),
        _ => None,
    }
}

impl SpeculativeConfig {
    pub fn from_deploy_hash_map(map: &HashMap<String, serde_json::Value>) -> Self {
        let method = map
            .get("speculative_method")
            .and_then(|v| v.as_str())
            .unwrap_or("off");
        match method {
            "mtp" => Self::Mtp {
                num_tokens: map
                    .get("num_speculative_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(DEFAULT_MTP_TOKENS as u64) as u32,
            },
            "ngram" | "ngram-simple" => Self::NgramSimple {
                size_ngram: map.get("size_ngram").and_then(|v| v.as_u64()).unwrap_or(3) as u16,
                size_mgram: map.get("size_mgram").and_then(|v| v.as_u64()).unwrap_or(4) as u16,
            },
            _ => Self::Off,
        }
    }
}

pub fn inspect_gguf(path: &Path) -> Result<LlamaGgufInfo, LlamaError> {
    let mut file = std::fs::File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Err(LlamaError::InvalidGguf("brak magic GGUF".to_string()));
    }

    let version = read_u32(&mut file)?;
    if version < 2 {
        return Err(LlamaError::InvalidGguf(format!(
            "nieobslugiwana wersja GGUF {version}"
        )));
    }

    let tensor_count = read_u64(&mut file)?;
    let metadata_count = read_u64(&mut file)?;
    let mut name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut architecture = None;
    let mut context_length = None;
    let mut embedding_length = None;
    let mut vocab_size = None;
    let mut quantization_version = None;
    let mut mtp_layers = 0_u32;

    for _ in 0..metadata_count {
        let key = read_gguf_string(&mut file)?;
        let value_type = read_u32(&mut file)?;
        match value_type {
            GGUF_TYPE_U32 => {
                let value = read_u32(&mut file)?;
                if key == "general.quantization_version" {
                    quantization_version = Some(value as u64);
                }
                if key.ends_with(".context_length") {
                    context_length = Some(value as u64);
                }
                if key.ends_with(".embedding_length") {
                    embedding_length = Some(value as u64);
                }
                if key.ends_with(".nextn_predict_layers") {
                    mtp_layers = value;
                }
            }
            GGUF_TYPE_U64 => {
                let value = read_u64(&mut file)?;
                if key.ends_with(".context_length") {
                    context_length = Some(value);
                }
                if key.ends_with(".embedding_length") {
                    embedding_length = Some(value);
                }
                if key.ends_with(".nextn_predict_layers") {
                    mtp_layers = value as u32;
                }
            }
            GGUF_TYPE_STRING => {
                let value = read_gguf_string(&mut file)?;
                if key == "general.name" {
                    name = value;
                } else if key == "general.architecture" {
                    architecture = Some(value);
                }
            }
            GGUF_TYPE_ARRAY => {
                let array_type = read_u32(&mut file)?;
                let len = read_u64(&mut file)?;
                if key == "tokenizer.ggml.tokens" {
                    vocab_size = Some(len);
                }
                skip_gguf_array(&mut file, array_type, len)?;
            }
            other => skip_gguf_scalar(&mut file, other)?,
        }
    }

    Ok(LlamaGgufInfo {
        path: path.to_path_buf(),
        name,
        architecture,
        size_bytes,
        tensor_count,
        metadata_count,
        context_length,
        embedding_length,
        vocab_size,
        quantization_version,
        mtp_layers,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum SpeculativeConfig {
    #[default]
    Off,
    Mtp {
        num_tokens: u32,
    },
    NgramSimple {
        size_ngram: u16,
        size_mgram: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredLlamaCapability {
    BackendInit,
    LoadGguf,
    ModelMetadata,
    Tokenize,
    DecodeBatch,
    LoadThreads,
    LoadBatchSize,
    Sampling,
    Streaming,
    StopSequences,
    Embeddings,
    SpeculativeMtp,
    SpeculativeNgramSimple,
}

pub const CURRENT_REQUIRED_CAPABILITIES: &[RequiredLlamaCapability] = &[
    RequiredLlamaCapability::BackendInit,
    RequiredLlamaCapability::LoadGguf,
    RequiredLlamaCapability::ModelMetadata,
    RequiredLlamaCapability::Tokenize,
    RequiredLlamaCapability::DecodeBatch,
    RequiredLlamaCapability::LoadThreads,
    RequiredLlamaCapability::LoadBatchSize,
    RequiredLlamaCapability::Sampling,
    RequiredLlamaCapability::Streaming,
    RequiredLlamaCapability::StopSequences,
    RequiredLlamaCapability::Embeddings,
    RequiredLlamaCapability::SpeculativeMtp,
    RequiredLlamaCapability::SpeculativeNgramSimple,
];

#[derive(Debug, thiserror::Error)]
pub enum LlamaError {
    #[error("llama.cpp feature is disabled")]
    FeatureDisabled,
    #[error("ścieżka modelu zawiera bajt NUL")]
    InvalidModelPath,
    #[error("błąd odczytu GGUF: {0}")]
    Io(#[from] std::io::Error),
    #[error("niepoprawny GGUF: {0}")]
    InvalidGguf(String),
    #[error("llama.cpp nie załadował modelu: {0}")]
    LoadFailed(String),
    #[error("llama.cpp nie utworzył kontekstu")]
    ContextFailed,
    #[error("llama.cpp nie utworzył samplera")]
    SamplerFailed,
    #[error("tokenizacja nie powiodła się")]
    TokenizeFailed,
    #[error("dekodowanie llama.cpp zwróciło kod błędu {0}")]
    DecodeFailed(i32),
    #[error("prompt jest pusty po tokenizacji")]
    EmptyPrompt,
    #[error("prompt ma {prompt_tokens} tokenow i przekracza kontekst {context_tokens}")]
    PromptTooLong {
        prompt_tokens: usize,
        context_tokens: usize,
    },
    #[error("batch llama.cpp przekroczyl pojemnosc {capacity}")]
    BatchCapacityExceeded { capacity: i32 },
    #[error("model nie wspiera embeddingów")]
    EmbeddingsUnsupported,
    #[error("nie udało się pobrać embeddingu")]
    EmbeddingsMissing,
}

#[cfg(feature = "llama")]
pub struct LlamaRuntime {
    _backend: LlamaBackendGuard,
    model: LlamaModelGuard,
    metadata: LlamaModelMetadata,
    load_config: LlamaLoadConfig,
}

#[cfg(feature = "llama")]
unsafe impl Send for LlamaRuntime {}

#[cfg(feature = "llama")]
unsafe impl Sync for LlamaRuntime {}

#[cfg(feature = "llama")]
impl LlamaRuntime {
    pub fn load(path: &Path, config: LlamaLoadConfig) -> Result<Self, LlamaError> {
        let backend = LlamaBackendGuard::init();
        let c_path = path_to_c_string(path)?;
        let mut params = unsafe { sys::llama_model_default_params() };
        params.n_gpu_layers = config.n_gpu_layers as i32;
        params.main_gpu = config.main_gpu;
        // tensor_split przekazujemy jako surowy wskaźnik do FFI, więc wektor MUSI
        // żyć aż do końca llama_model_load_from_file. `tensor_split` jest tu lokalną
        // zmienną i nie jest dropowany przed loadem. Waga 0.0 wyklucza kartę —
        // bez tego embedded llama.cpp rozkładałby model na WSZYSTKIE karty mimo
        // ustawionego main_gpu (CUDA_VISIBLE_DEVICES nie działa w jednym procesie).
        let tensor_split = config.tensor_split.clone();
        if !tensor_split.is_empty() {
            params.split_mode = sys::LLAMA_SPLIT_MODE_LAYER;
            params.tensor_split = tensor_split.as_ptr();
        }

        let log_lock = LLAMA_LOG_CAPTURE_LOCK.get_or_init(|| Mutex::new(()));
        let _log_guard = log_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut logs = LlamaLogCapture::default();
        unsafe {
            sys::llama_log_set(
                Some(capture_llama_log),
                (&mut logs as *mut LlamaLogCapture).cast(),
            );
        }
        let raw = unsafe { sys::llama_model_load_from_file(c_path.as_ptr(), params) };
        install_llama_log_filter();
        if raw.is_null() {
            return Err(LlamaError::LoadFailed(logs.summary()));
        }

        let model = LlamaModelGuard { raw };
        let metadata = read_metadata(path, model.raw);

        Ok(Self {
            _backend: backend,
            model,
            metadata,
            load_config: config,
        })
    }

    pub fn metadata(&self) -> &LlamaModelMetadata {
        &self.metadata
    }

    pub(crate) fn model_ptr(&self) -> *mut sys::llama_model {
        self.model.raw
    }

    pub fn embeddings(&self, text: &str, normalize: bool) -> Result<Vec<f32>, LlamaError> {
        let n_embd = self.metadata.embedding_size as usize;
        if n_embd == 0 {
            return Err(LlamaError::EmbeddingsUnsupported);
        }

        let tokens = self.tokenize(text, true)?;
        if tokens.is_empty() {
            return Err(LlamaError::EmptyPrompt);
        }
        self.ensure_prompt_fits(tokens.len())?;

        let mut context = self.context(true)?;
        let mut batch = LlamaBatchGuard::new(self.context_limit() as i32, 1);
        eval_tokens(&mut context, &mut batch, &tokens, 0, true)?;
        unsafe { sys::llama_synchronize(context.raw) };

        let ptr = unsafe { sys::llama_get_embeddings_seq(context.raw, 0) };
        if ptr.is_null() {
            return Err(LlamaError::EmbeddingsMissing);
        }

        let mut values = unsafe { std::slice::from_raw_parts(ptr, n_embd) }.to_vec();
        if normalize {
            let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for value in &mut values {
                    *value /= norm;
                }
            }
        }
        Ok(values)
    }

    fn context(&self, embeddings: bool) -> Result<LlamaContextGuard, LlamaError> {
        let mut params = unsafe { sys::llama_context_default_params() };
        params.n_ctx = self.context_limit();
        params.n_batch = self.load_config.batch_size.max(1);
        params.n_ubatch = self.load_config.batch_size.max(1);
        params.embeddings = embeddings;
        params.flash_attn_type = match self.load_config.flash_attn {
            FlashAttentionMode::Auto => sys::LLAMA_FLASH_ATTN_TYPE_AUTO,
            FlashAttentionMode::Off => sys::LLAMA_FLASH_ATTN_TYPE_DISABLED,
            FlashAttentionMode::On => sys::LLAMA_FLASH_ATTN_TYPE_ENABLED,
        };
        if let Some(threads) = self.load_config.threads {
            params.n_threads = threads as i32;
            params.n_threads_batch = threads as i32;
        }

        let raw = unsafe { sys::llama_init_from_model(self.model.raw, params) };
        if raw.is_null() {
            return Err(LlamaError::ContextFailed);
        }
        Ok(LlamaContextGuard { raw })
    }

    fn context_limit(&self) -> u32 {
        self.load_config
            .ctx_size
            .min(self.metadata.context_train)
            .max(1)
    }

    fn ensure_prompt_fits(&self, prompt_tokens: usize) -> Result<(), LlamaError> {
        let context_tokens = self.context_limit() as usize;
        if prompt_tokens >= context_tokens {
            Err(LlamaError::PromptTooLong {
                prompt_tokens,
                context_tokens,
            })
        } else {
            Ok(())
        }
    }

    fn tokenize(&self, text: &str, add_special: bool) -> Result<Vec<sys::llama_token>, LlamaError> {
        tokenize_with_model(self.model.raw, text, add_special)
    }
}

#[cfg(not(feature = "llama"))]
pub struct LlamaRuntime;

#[cfg(not(feature = "llama"))]
impl LlamaRuntime {
    pub fn load(_path: &Path, _config: LlamaLoadConfig) -> Result<Self, LlamaError> {
        Err(LlamaError::FeatureDisabled)
    }
}

#[cfg(feature = "llama")]
pub(crate) struct LlamaBackendGuard;

#[cfg(feature = "llama")]
impl LlamaBackendGuard {
    pub(crate) fn init() -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            unsafe { sys::llama_backend_init() };
            install_llama_log_filter();
        });
        Self
    }
}

#[cfg(feature = "llama")]
pub(crate) struct LlamaModelGuard {
    pub(crate) raw: *mut sys::llama_model,
}

#[cfg(feature = "llama")]
impl Drop for LlamaModelGuard {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sys::llama_model_free(self.raw) };
        }
    }
}

#[cfg(feature = "llama")]
pub(crate) struct LlamaContextGuard {
    pub(crate) raw: *mut sys::llama_context,
}

#[cfg(feature = "llama")]
impl Drop for LlamaContextGuard {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sys::llama_free(self.raw) };
        }
    }
}

#[cfg(feature = "llama")]
struct LlamaBatchGuard {
    raw: sys::llama_batch,
    capacity: i32,
}

#[cfg(feature = "llama")]
impl LlamaBatchGuard {
    fn new(n_tokens: i32, n_seq_max: i32) -> Self {
        Self {
            raw: unsafe { sys::llama_batch_init(n_tokens, 0, n_seq_max) },
            capacity: n_tokens,
        }
    }

    fn clear(&mut self) {
        self.raw.n_tokens = 0;
    }

    fn add(&mut self, token: sys::llama_token, pos: i32, logits: bool) -> Result<(), LlamaError> {
        if self.raw.n_tokens >= self.capacity {
            return Err(LlamaError::BatchCapacityExceeded {
                capacity: self.capacity,
            });
        }
        let idx = self.raw.n_tokens as isize;
        unsafe {
            *self.raw.token.offset(idx) = token;
            *self.raw.pos.offset(idx) = pos;
            *self.raw.n_seq_id.offset(idx) = 1;
            **self.raw.seq_id.offset(idx) = 0;
            *self.raw.logits.offset(idx) = if logits { 1 } else { 0 };
        }
        self.raw.n_tokens += 1;
        Ok(())
    }
}

#[cfg(feature = "llama")]
impl Drop for LlamaBatchGuard {
    fn drop(&mut self) {
        unsafe { sys::llama_batch_free(self.raw) };
    }
}

#[cfg(feature = "llama")]
pub(crate) struct LlamaSamplerGuard {
    pub(crate) raw: *mut sys::llama_sampler,
}

#[cfg(feature = "llama")]
impl LlamaSamplerGuard {
    pub(crate) fn add(&mut self, sampler: *mut sys::llama_sampler) -> Result<(), LlamaError> {
        if sampler.is_null() {
            return Err(LlamaError::SamplerFailed);
        }
        unsafe { sys::llama_sampler_chain_add(self.raw, sampler) };
        Ok(())
    }

    pub(crate) fn sample(&mut self, context: *mut sys::llama_context, idx: i32) -> sys::llama_token {
        unsafe { sys::llama_sampler_sample(self.raw, context, idx) }
    }

    pub(crate) fn accept(&mut self, token: sys::llama_token) -> Result<(), LlamaError> {
        unsafe { sys::llama_sampler_accept(self.raw, token) };
        Ok(())
    }
}

#[cfg(feature = "llama")]
impl Drop for LlamaSamplerGuard {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sys::llama_sampler_free(self.raw) };
        }
    }
}

#[cfg(feature = "llama")]
fn eval_tokens(
    context: &mut LlamaContextGuard,
    batch: &mut LlamaBatchGuard,
    tokens: &[sys::llama_token],
    start_pos: i32,
    last_logits: bool,
) -> Result<(), LlamaError> {
    batch.clear();
    for (idx, token) in tokens.iter().enumerate() {
        batch.add(
            *token,
            start_pos + idx as i32,
            last_logits && idx + 1 == tokens.len(),
        )?;
    }
    decode(context.raw, batch)
}

#[cfg(feature = "llama")]
fn decode(context: *mut sys::llama_context, batch: &LlamaBatchGuard) -> Result<(), LlamaError> {
    let rc = unsafe { sys::llama_decode(context, batch.raw) };
    if rc != 0 {
        return Err(LlamaError::DecodeFailed(rc));
    }
    Ok(())
}

#[cfg(feature = "llama")]
fn path_to_c_string(path: &Path) -> Result<std::ffi::CString, LlamaError> {
    std::ffi::CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| LlamaError::InvalidModelPath)
}

#[cfg(feature = "llama")]
fn read_metadata(path: &Path, model: *const sys::llama_model) -> LlamaModelMetadata {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let context_train = unsafe { sys::llama_model_n_ctx_train(model) }.max(1) as u32;
    let vocab = unsafe { sys::llama_model_get_vocab(model) };
    let vocab_size = unsafe { sys::llama_vocab_n_tokens(vocab) }.max(0) as u32;
    let embedding_size = unsafe { sys::llama_model_n_embd(model) }.max(0) as u32;
    let mtp_layers = detect_mtp_layers(model);

    LlamaModelMetadata {
        name,
        size_bytes: unsafe { sys::llama_model_size(model) },
        parameters: unsafe { sys::llama_model_n_params(model) },
        context_train,
        vocab_size,
        embedding_size,
        quantization: detect_quantization(path),
        mtp_layers,
    }
}

#[cfg(feature = "llama")]
fn detect_mtp_layers(model: *const sys::llama_model) -> u32 {
    let count = unsafe { sys::llama_model_meta_count(model) };
    for idx in 0..count {
        let Some(key) = model_meta_key(model, idx) else {
            continue;
        };
        if key.ends_with(".nextn_predict_layers") {
            if let Some(value) = model_meta_value(model, idx).and_then(|v| v.parse::<u32>().ok()) {
                return value;
            }
        }
    }
    if model_has_meta_key_fragment(model, "nextn") {
        1
    } else {
        0
    }
}

#[cfg(feature = "llama")]
fn model_has_meta_key_fragment(model: *const sys::llama_model, fragment: &str) -> bool {
    let count = unsafe { sys::llama_model_meta_count(model) };
    (0..count).any(|idx| {
        model_meta_key(model, idx)
            .map(|key| key.contains(fragment))
            .unwrap_or(false)
    })
}

#[cfg(feature = "llama")]
fn model_meta_key(model: *const sys::llama_model, idx: i32) -> Option<String> {
    model_meta_string(|buf, len| unsafe {
        sys::llama_model_meta_key_by_index(model, idx, buf, len)
    })
}

#[cfg(feature = "llama")]
fn model_meta_value(model: *const sys::llama_model, idx: i32) -> Option<String> {
    model_meta_string(|buf, len| unsafe {
        sys::llama_model_meta_val_str_by_index(model, idx, buf, len)
    })
}

#[cfg(feature = "llama")]
fn model_meta_string(read: impl Fn(*mut std::os::raw::c_char, usize) -> i32) -> Option<String> {
    let needed = read(std::ptr::null_mut(), 0);
    if needed < 0 {
        return None;
    }
    // c_char jest i8 na x86_64, ale u8 na aarch64 (char domyslnie unsigned na
    // ARM). Buforujemy jako c_char, zeby wskaznik pasowal do FFI na obu ABI.
    let mut buf = vec![0 as std::os::raw::c_char; needed as usize + 1];
    let written = read(buf.as_mut_ptr(), buf.len());
    if written < 0 {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .to_string(),
    )
}

#[cfg(feature = "llama")]
fn detect_quantization(path: &Path) -> Option<String> {
    path.file_stem().and_then(|s| s.to_str()).and_then(|name| {
        let upper = name.to_uppercase();
        [
            "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_K_S", "Q4_K_M", "Q5_0", "Q5_K_S",
            "Q5_K_M", "Q6_K", "Q8_0", "F16", "F32",
        ]
        .iter()
        .find(|q| upper.contains(*q))
        .map(|q| q.to_string())
    })
}

#[cfg(feature = "llama")]
pub(crate) fn build_sampler_chain(
    repeat_penalty: f32,
    top_k: u32,
    top_p: f32,
    temperature: f32,
    seed: u32,
) -> Result<LlamaSamplerGuard, LlamaError> {
    let params = unsafe { sys::llama_sampler_chain_default_params() };
    let chain = unsafe { sys::llama_sampler_chain_init(params) };
    if chain.is_null() {
        return Err(LlamaError::SamplerFailed);
    }
    let mut sampler = LlamaSamplerGuard { raw: chain };

    if repeat_penalty > 1.0 {
        sampler.add(unsafe { sys::llama_sampler_init_penalties(64, repeat_penalty, 0.0, 0.0) })?;
    }
    if top_k > 0 {
        sampler.add(unsafe { sys::llama_sampler_init_top_k(top_k as i32) })?;
    }
    if top_p < 1.0 {
        sampler.add(unsafe { sys::llama_sampler_init_top_p(top_p, 1) })?;
    }
    sampler.add(unsafe { sys::llama_sampler_init_temp(temperature) })?;
    if temperature <= 0.0 {
        sampler.add(unsafe { sys::llama_sampler_init_greedy() })?;
    } else {
        sampler.add(unsafe { sys::llama_sampler_init_dist(seed) })?;
    }

    Ok(sampler)
}

#[cfg(feature = "llama")]
pub(crate) fn tokenize_with_model(
    model: *const sys::llama_model,
    text: &str,
    add_special: bool,
) -> Result<Vec<sys::llama_token>, LlamaError> {
    let vocab = unsafe { sys::llama_model_get_vocab(model) };
    let text_bytes = text.as_bytes();
    let needed = unsafe {
        sys::llama_tokenize(
            vocab,
            text_bytes.as_ptr().cast(),
            text_bytes.len() as i32,
            std::ptr::null_mut(),
            0,
            add_special,
            false,
        )
    };

    let cap = if needed < 0 { -needed } else { needed };
    if cap <= 0 {
        return Err(LlamaError::TokenizeFailed);
    }

    let mut tokens = vec![0; cap as usize];
    let written = unsafe {
        sys::llama_tokenize(
            vocab,
            text_bytes.as_ptr().cast(),
            text_bytes.len() as i32,
            tokens.as_mut_ptr(),
            tokens.len() as i32,
            add_special,
            false,
        )
    };
    if written < 0 {
        return Err(LlamaError::TokenizeFailed);
    }
    tokens.truncate(written as usize);
    Ok(tokens)
}

#[cfg(feature = "llama")]
pub(crate) fn is_eog_with_model(model: *const sys::llama_model, token: sys::llama_token) -> bool {
    let vocab = unsafe { sys::llama_model_get_vocab(model) };
    unsafe { sys::llama_vocab_is_eog(vocab, token) }
}

#[cfg(feature = "llama")]
pub(crate) fn token_to_piece_with_model(
    model: *const sys::llama_model,
    token: sys::llama_token,
    decoder: &mut encoding_rs::Decoder,
) -> String {
    let vocab = unsafe { sys::llama_model_get_vocab(model) };
    // c_char = i8 (x86_64) / u8 (aarch64) — patrz model_meta_string.
    let mut buf = vec![0 as std::os::raw::c_char; 256];
    let mut n = unsafe {
        sys::llama_token_to_piece(vocab, token, buf.as_mut_ptr(), buf.len() as i32, 0, false)
    };
    if n < 0 {
        buf.resize((-n) as usize, 0);
        n = unsafe {
            sys::llama_token_to_piece(vocab, token, buf.as_mut_ptr(), buf.len() as i32, 0, false)
        };
    }
    if n <= 0 {
        return String::new();
    }
    decode_piece(decoder, unsafe {
        std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), n as usize)
    })
}

#[cfg(feature = "llama")]
pub(crate) fn decode_piece(decoder: &mut encoding_rs::Decoder, bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2).max(8));
    let _ = decoder.decode_to_string(bytes, &mut output, false);
    output
}

#[cfg(feature = "llama")]
pub(crate) fn check_stop_sequence<'a>(text: &str, stop_sequences: &'a [String]) -> Option<&'a str> {
    stop_sequences
        .iter()
        .find(|stop| text.ends_with(stop.as_str()))
        .map(|s| s.as_str())
}

fn static_library_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.lib")
    } else {
        format!("lib{name}.a")
    }
}

const GGUF_TYPE_U8: u32 = 0;
const GGUF_TYPE_I8: u32 = 1;
const GGUF_TYPE_U16: u32 = 2;
const GGUF_TYPE_I16: u32 = 3;
const GGUF_TYPE_U32: u32 = 4;
const GGUF_TYPE_I32: u32 = 5;
const GGUF_TYPE_F32: u32 = 6;
const GGUF_TYPE_BOOL: u32 = 7;
const GGUF_TYPE_STRING: u32 = 8;
const GGUF_TYPE_ARRAY: u32 = 9;
const GGUF_TYPE_U64: u32 = 10;
const GGUF_TYPE_I64: u32 = 11;
const GGUF_TYPE_F64: u32 = 12;

fn read_u32(read: &mut impl Read) -> Result<u32, LlamaError> {
    let mut buf = [0_u8; 4];
    read.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(read: &mut impl Read) -> Result<u64, LlamaError> {
    let mut buf = [0_u8; 8];
    read.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_gguf_string(read: &mut impl Read) -> Result<String, LlamaError> {
    let len = read_u64(read)?;
    if len > 16 * 1024 * 1024 {
        return Err(LlamaError::InvalidGguf(format!(
            "za dlugi string metadata: {len} bajtow"
        )));
    }
    let mut buf = vec![0_u8; len as usize];
    read.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| LlamaError::InvalidGguf(e.to_string()))
}

fn skip_gguf_scalar(reader: &mut (impl Read + Seek), value_type: u32) -> Result<(), LlamaError> {
    let bytes = match value_type {
        GGUF_TYPE_U8 | GGUF_TYPE_I8 | GGUF_TYPE_BOOL => 1,
        GGUF_TYPE_U16 | GGUF_TYPE_I16 => 2,
        GGUF_TYPE_U32 | GGUF_TYPE_I32 | GGUF_TYPE_F32 => 4,
        GGUF_TYPE_U64 | GGUF_TYPE_I64 | GGUF_TYPE_F64 => 8,
        GGUF_TYPE_STRING => {
            let len = read_u64(reader)?;
            seek_forward(reader, len)?;
            return Ok(());
        }
        other => {
            return Err(LlamaError::InvalidGguf(format!(
                "nieznany typ metadata {other}"
            )));
        }
    };
    seek_forward(reader, bytes)
}

fn skip_gguf_array(
    reader: &mut (impl Read + Seek),
    array_type: u32,
    len: u64,
) -> Result<(), LlamaError> {
    match array_type {
        GGUF_TYPE_STRING => {
            for _ in 0..len {
                let bytes = read_u64(reader)?;
                seek_forward(reader, bytes)?;
            }
            Ok(())
        }
        other => {
            let item_size = match other {
                GGUF_TYPE_U8 | GGUF_TYPE_I8 | GGUF_TYPE_BOOL => 1,
                GGUF_TYPE_U16 | GGUF_TYPE_I16 => 2,
                GGUF_TYPE_U32 | GGUF_TYPE_I32 | GGUF_TYPE_F32 => 4,
                GGUF_TYPE_U64 | GGUF_TYPE_I64 | GGUF_TYPE_F64 => 8,
                _ => {
                    return Err(LlamaError::InvalidGguf(format!(
                        "nieobslugiwany typ tablicy metadata {other}"
                    )));
                }
            };
            seek_forward(
                reader,
                len.checked_mul(item_size).ok_or_else(|| {
                    LlamaError::InvalidGguf("przepelnienie rozmiaru tablicy".to_string())
                })?,
            )
        }
    }
}

fn seek_forward(reader: &mut impl Seek, bytes: u64) -> Result<(), LlamaError> {
    let offset = i64::try_from(bytes)
        .map_err(|_| LlamaError::InvalidGguf("za duzy offset metadata".to_string()))?;
    reader.seek(SeekFrom::Current(offset))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{inspect_gguf, LlamaLoadConfig, SpeculativeConfig};
    use std::collections::HashMap;
    use std::io::Write;

    #[test]
    fn parses_load_config_from_deploy_map() {
        let map = serde_json::json!({
            "ctx_size": 8192,
            "n_gpu_layers": 80,
            "batch_size": 1024,
            "threads": 12
        });
        let map = map.as_object().unwrap();

        let config = LlamaLoadConfig::from_deploy_map(map);

        assert_eq!(config.ctx_size, 8192);
        assert_eq!(config.n_gpu_layers, 80);
        assert_eq!(config.batch_size, 1024);
        assert_eq!(config.threads, Some(12));
    }

    #[test]
    fn parses_load_config_from_deploy_hash_map() {
        let mut map = HashMap::new();
        map.insert("ctx_size".to_string(), serde_json::json!(16384));
        map.insert("n_gpu_layers".to_string(), serde_json::json!(999));
        map.insert("batch_size".to_string(), serde_json::json!(256));
        map.insert("threads".to_string(), serde_json::json!(8));

        let config = LlamaLoadConfig::from_deploy_hash_map(&map);

        assert_eq!(config.ctx_size, 16384);
        assert_eq!(config.n_gpu_layers, 999);
        assert_eq!(config.batch_size, 256);
        assert_eq!(config.threads, Some(8));
    }

    #[test]
    fn serializes_ngram_simple_config() {
        let config = SpeculativeConfig::NgramSimple {
            size_ngram: 3,
            size_mgram: 4,
        };

        let json = serde_json::to_value(config).unwrap();

        assert_eq!(json["method"], "ngram-simple");
        assert_eq!(json["size_ngram"], 3);
        assert_eq!(json["size_mgram"], 4);
    }

    #[test]
    fn parses_speculative_config_from_deploy_hash_map() {
        let mut map = HashMap::new();
        map.insert(
            "speculative_method".to_string(),
            serde_json::json!("ngram-simple"),
        );
        map.insert("size_ngram".to_string(), serde_json::json!(5));
        map.insert("size_mgram".to_string(), serde_json::json!(12));

        let config = SpeculativeConfig::from_deploy_hash_map(&map);

        assert_eq!(
            config,
            SpeculativeConfig::NgramSimple {
                size_ngram: 5,
                size_mgram: 12,
            }
        );
    }

    #[test]
    fn inspects_minimal_gguf_metadata() {
        let path =
            std::env::temp_dir().join(format!("tentaflow-gguf-test-{}.gguf", std::process::id()));
        {
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(b"GGUF").unwrap();
            write_u32(&mut file, 3);
            write_u64(&mut file, 0);
            write_u64(&mut file, 5);
            write_kv_string(&mut file, "general.name", "Test Model");
            write_kv_string(&mut file, "general.architecture", "qwen3");
            write_kv_u32(&mut file, "qwen3.context_length", 32768);
            write_kv_u32(&mut file, "qwen3.nextn_predict_layers", 4);
            write_kv_string_array(&mut file, "tokenizer.ggml.tokens", &["a", "b", "c"]);
        }

        let info = inspect_gguf(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(info.name, "Test Model");
        assert_eq!(info.architecture.as_deref(), Some("qwen3"));
        assert_eq!(info.context_length, Some(32768));
        assert_eq!(info.mtp_layers, 4);
        assert_eq!(info.vocab_size, Some(3));
        assert!(info.supports_mtp());
    }

    fn write_kv_string(file: &mut std::fs::File, key: &str, value: &str) {
        write_string(file, key);
        write_u32(file, super::GGUF_TYPE_STRING);
        write_string(file, value);
    }

    fn write_kv_u32(file: &mut std::fs::File, key: &str, value: u32) {
        write_string(file, key);
        write_u32(file, super::GGUF_TYPE_U32);
        write_u32(file, value);
    }

    fn write_kv_string_array(file: &mut std::fs::File, key: &str, values: &[&str]) {
        write_string(file, key);
        write_u32(file, super::GGUF_TYPE_ARRAY);
        write_u32(file, super::GGUF_TYPE_STRING);
        write_u64(file, values.len() as u64);
        for value in values {
            write_string(file, value);
        }
    }

    fn write_string(file: &mut std::fs::File, value: &str) {
        write_u64(file, value.len() as u64);
        file.write_all(value.as_bytes()).unwrap();
    }

    fn write_u32(file: &mut std::fs::File, value: u32) {
        file.write_all(&value.to_le_bytes()).unwrap();
    }

    fn write_u64(file: &mut std::fs::File, value: u64) {
        file.write_all(&value.to_le_bytes()).unwrap();
    }
}
