// =============================================================================
// Plik: inference/mlx_swift_bridge.rs
// Opis: Bridge do Swift MLX na iOS — deleguje inferencje do natywnego mlx-swift
//       przez FFI callback registration pattern.
// =============================================================================

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;

use crate::inference::{
    EmbeddingParams, EmbeddingResult, GenerateParams, GenerateResult, InferenceEngine, ModelInfo,
    StopReason, StreamToken,
};

// =============================================================================
// Typy callbackow FFI — musza pasowac do Bridging Header
// =============================================================================

/// Callback: zaladuj model z podanej sciezki. Zwraca 0=OK, <0=blad
type LoadModelFn = extern "C" fn(model_path: *const c_char, context: *mut c_void) -> i32;

/// Callback: wyladuj model
type UnloadModelFn = extern "C" fn(context: *mut c_void);

/// Callback: generuj tekst. prompt=C string, max_tokens, temperature, top_p.
/// Dla kazdego wygenerowanego tokena Swift wywoluje token_callback.
/// Zwraca 0=OK, <0=blad
type GenerateFn = extern "C" fn(
    prompt: *const c_char,
    max_tokens: i32,
    temperature: f32,
    top_p: f32,
    token_callback: TokenCallbackFn,
    callback_context: *mut c_void,
    context: *mut c_void,
) -> i32;

/// Callback wolany przez Swift dla kazdego wygenerowanego tokena
type TokenCallbackFn =
    extern "C" fn(token_text: *const c_char, is_final: bool, callback_context: *mut c_void);

/// Callback: pobierz info o modelu (nazwa, backend, rozmiar). Zwraca JSON C string (caller musi zwolnic)
type ModelInfoFn = extern "C" fn(context: *mut c_void) -> *mut c_char;

// =============================================================================
// Wrapper na raw pointer — bezpieczne przesylanie miedzy watkami
// =============================================================================

/// Opakowanie na `*mut c_void` jako usize — umozliwia przesylanie miedzy watkami.
/// SAFETY: Swift side gwarantuje thread-safety przez DispatchQueue.
/// Uzywamy usize zamiast *mut c_void bo raw pointery nie implementuja Send.
#[derive(Clone, Copy)]
struct SendPtr(usize);

impl SendPtr {
    /// Tworzy SendPtr z raw pointera
    fn from_raw(ptr: *mut c_void) -> Self {
        Self(ptr as usize)
    }

    /// Zwraca raw pointer
    fn as_ptr(self) -> *mut c_void {
        self.0 as *mut c_void
    }
}

// =============================================================================
// Globalny stan callbackow
// =============================================================================

/// Przechowuje zarejestrowane callbacki z Swift
struct SwiftCallbacks {
    load_fn: LoadModelFn,
    unload_fn: UnloadModelFn,
    generate_fn: GenerateFn,
    model_info_fn: ModelInfoFn,
    /// Opaque pointer na Swift object — zarzadzany przez strone Swift
    context: *mut c_void,
}

// Swift callbacks sa thread-safe bo Swift side uzywa DispatchQueue
unsafe impl Send for SwiftCallbacks {}
unsafe impl Sync for SwiftCallbacks {}

/// Globalny singleton — ustawiany raz przy starcie przez Swift
static SWIFT_CALLBACKS: OnceLock<SwiftCallbacks> = OnceLock::new();

// =============================================================================
// Rejestracja FFI — wywolywane z Swift przy starcie aplikacji
// =============================================================================

/// Rejestruje callbacki MLX z natywnej strony Swift.
/// Wywolywane z AppDelegate po `tentaflow_mobile_start()`.
#[no_mangle]
pub extern "C" fn tentaflow_register_mlx_swift(
    load_fn: LoadModelFn,
    unload_fn: UnloadModelFn,
    generate_fn: GenerateFn,
    model_info_fn: ModelInfoFn,
    context: *mut c_void,
) {
    let _ = SWIFT_CALLBACKS.set(SwiftCallbacks {
        load_fn,
        unload_fn,
        generate_fn,
        model_info_fn,
        context,
    });
    tracing::info!("Swift MLX callbacks zarejestrowane");
}

/// Sprawdza czy Swift MLX jest dostepny (callbacki zarejestrowane)
pub fn is_available() -> bool {
    SWIFT_CALLBACKS.get().is_some()
}

// =============================================================================
// Pomocnicze funkcje
// =============================================================================

/// Pobiera callbacki lub zwraca blad
fn get_callbacks() -> Result<&'static SwiftCallbacks> {
    SWIFT_CALLBACKS
        .get()
        .context("Swift MLX callbacks nie zostaly zarejestrowane")
}

/// Konwertuje Rust &str na CString (zastepuje wewnetrzne NUL bajty podkresleniem)
fn to_cstring(s: &str) -> CString {
    let sanitized = s.replace('\0', "_");
    CString::new(sanitized).unwrap_or_else(|_| CString::new("").unwrap())
}

// =============================================================================
// Token callback — wolany przez Swift dla kazdego wygenerowanego tokena
// =============================================================================

/// Callback extern "C" przekazywany do Swift. Swift wywoluje go dla kazdego tokena.
/// `callback_context` to wskaznik na `mpsc::Sender<StreamToken>`.
extern "C" fn rust_token_callback(
    token_text: *const c_char,
    is_final: bool,
    callback_context: *mut c_void,
) {
    // SAFETY: callback_context to &mpsc::Sender<StreamToken> rzutowany na *mut c_void
    // w generate/generate_stream. Zycie nadawcy jest gwarantowane przez blok wywolujacy.
    let tx = unsafe { &*(callback_context as *const mpsc::Sender<StreamToken>) };

    let text = if token_text.is_null() {
        String::new()
    } else {
        // SAFETY: Swift przekazuje poprawny C string zakonczony NUL
        unsafe { CStr::from_ptr(token_text) }
            .to_string_lossy()
            .to_string()
    };

    // Ignorujemy blad wyslania — moze sie zdarzyc jesli odbiorca zostal porzucony
    let _ = tx.blocking_send(StreamToken { text, is_final });
}

// =============================================================================
// Silnik inferencji — MlxSwiftEngine
// =============================================================================

/// Silnik inferencji delegujacy do Swift MLX przez zarejestrowane callbacki.
/// Kazde wywolanie FFI odbywa sie na dedykowanym watku (spawn_blocking)
/// poniewaz Swift side moze blokowac.
pub struct MlxSwiftEngine {
    /// Cache model info z load_model — zeby model_info() nie traciło chat_template
    cached_info: std::sync::Mutex<Option<ModelInfo>>,
}

impl MlxSwiftEngine {
    pub fn new() -> Self {
        Self {
            cached_info: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl InferenceEngine for MlxSwiftEngine {
    fn backend_name(&self) -> &str {
        "mlx"
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["safetensors".to_string(), "mlx".to_string()]
    }

    async fn load_model(
        &self,
        model_path: &Path,
        _deploy_params: &super::DeployParamsSnapshot,
    ) -> Result<ModelInfo> {
        let callbacks = get_callbacks()?;
        let path_str = model_path
            .to_str()
            .context("Sciezka modelu zawiera nieprawidlowe znaki UTF-8")?
            .to_string();

        let load_fn = callbacks.load_fn;
        let ctx = SendPtr::from_raw(callbacks.context);

        // Wywolaj load_fn na dedykowanym watku — Swift moze blokowac
        let result = tokio::task::spawn_blocking(move || {
            let c_path = to_cstring(&path_str);
            load_fn(c_path.as_ptr(), ctx.as_ptr())
        })
        .await
        .context("Blad watku ladowania modelu")?;

        if result < 0 {
            anyhow::bail!("Swift MLX: blad ladowania modelu (kod: {})", result);
        }

        // Wykryj chat template z tokenizer_config.json (tak samo jak mlx.rs na macOS)
        let chat_template = crate::routing::chat_template::detect_chat_template(model_path);
        debug!(
            "[mlx-bridge] Wykryty chat template: {:?}",
            chat_template.name()
        );

        // Pobierz info o zaladowanym modelu — nadpisz chat_template wykrytym z pliku
        let mut info = self.model_info().unwrap_or_else(|| ModelInfo {
            name: model_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            path: model_path.to_string_lossy().to_string(),
            size_bytes: 0,
            parameters: String::new(),
            quantization: None,
            context_length: 32768,
            loaded: true,
            vram_used_mb: 0,
            backend: "mlx".to_string(),
            chat_template: Some(chat_template.name().to_string()),
        });

        // Zawsze nadpisz chat_template wykrytym z pliku (Swift nie przekazuje tego)
        info.chat_template = Some(chat_template.name().to_string());
        info.context_length = 32768;

        // Cache info — zeby model_info() per-request zwracalo to samo (z chat_template)
        *self.cached_info.lock().unwrap() = Some(info.clone());

        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        let callbacks = get_callbacks()?;
        let unload_fn = callbacks.unload_fn;
        let ctx = SendPtr::from_raw(callbacks.context);

        tokio::task::spawn_blocking(move || {
            unload_fn(ctx.as_ptr());
        })
        .await
        .context("Blad watku wyladowania modelu")?;

        Ok(())
    }

    fn model_info(&self) -> Option<ModelInfo> {
        // Zwroc z cache (ustawione w load_model z poprawnym chat_template)
        let cached = self.cached_info.lock().unwrap();
        if cached.is_some() {
            return cached.clone();
        }
        drop(cached);

        // Fallback — zapytaj Swift
        let callbacks = SWIFT_CALLBACKS.get()?;
        let json_ptr = (callbacks.model_info_fn)(callbacks.context);

        if json_ptr.is_null() {
            return None;
        }

        let json_cstr = unsafe { CStr::from_ptr(json_ptr) };
        let json_str = json_cstr.to_string_lossy().to_string();

        // Zwolnij pamiec zaalokowana po stronie C/Swift
        unsafe {
            libc_free(json_ptr as *mut c_void);
        }

        serde_json::from_str(&json_str).ok()
    }

    async fn generate(&self, params: GenerateParams) -> Result<GenerateResult> {
        let callbacks = get_callbacks()?;
        let generate_fn = callbacks.generate_fn;
        let ctx = SendPtr::from_raw(callbacks.context);

        let prompt = params.prompt.clone();
        let max_tokens = params.max_tokens as i32;
        let temperature = params.temperature;
        let top_p = params.top_p;

        // Kanal do zbierania tokenow — bufor wystarczajacy na caly wynik
        let (tx, mut rx) = mpsc::channel::<StreamToken>(4096);

        let start = Instant::now();

        // Wywolaj generate_fn na dedykowanym watku
        let gen_result = tokio::task::spawn_blocking(move || {
            let c_prompt = to_cstring(&prompt);
            let tx_ptr = &tx as *const mpsc::Sender<StreamToken> as *mut c_void;

            let result = generate_fn(
                c_prompt.as_ptr(),
                max_tokens,
                temperature,
                top_p,
                rust_token_callback,
                tx_ptr,
                ctx.as_ptr(),
            );

            // tx jest dropowany tutaj — zamyka kanal
            drop(tx);
            result
        })
        .await
        .context("Blad watku generowania")?;

        if gen_result < 0 {
            anyhow::bail!("Swift MLX: blad generowania (kod: {})", gen_result);
        }

        // Zbierz wszystkie tokeny w jeden string
        let mut full_text = String::new();
        let mut tokens_count: u32 = 0;
        let mut first_token_time: Option<Instant> = None;

        while let Some(token) = rx.recv().await {
            if first_token_time.is_none() && !token.text.is_empty() {
                first_token_time = Some(Instant::now());
            }
            full_text.push_str(&token.text);
            tokens_count += 1;
        }

        let total_elapsed = start.elapsed();
        let time_to_first_token_ms =
            first_token_time.map(|t| t.duration_since(start).as_millis() as u64);

        // Oblicz tokeny na sekunde (bez prefill — od pierwszego tokena)
        let decode_duration = first_token_time
            .map(|t| total_elapsed - t.duration_since(start))
            .unwrap_or(total_elapsed);

        let tokens_per_second = if decode_duration.as_secs_f64() > 0.0 && tokens_count > 1 {
            (tokens_count - 1) as f64 / decode_duration.as_secs_f64()
        } else {
            0.0
        };

        Ok(GenerateResult {
            text: full_text,
            tokens_generated: tokens_count,
            tokens_per_second,
            prompt_tokens: 0, // Swift side nie raportuje tego
            stop_reason: StopReason::EndOfText,
            time_to_first_token_ms,
            total_time_ms: Some(total_elapsed.as_millis() as u64),
        })
    }

    async fn generate_stream(&self, params: GenerateParams) -> Result<mpsc::Receiver<StreamToken>> {
        let callbacks = get_callbacks()?;
        let generate_fn = callbacks.generate_fn;
        let ctx = SendPtr::from_raw(callbacks.context);

        let prompt = params.prompt.clone();
        let max_tokens = params.max_tokens as i32;
        let temperature = params.temperature;
        let top_p = params.top_p;

        // Kanal do streamowania tokenow do callera
        let (tx, rx) = mpsc::channel::<StreamToken>(256);

        // Uruchom generowanie na dedykowanym watku — tokeny beda streamowane przez kanal
        tokio::task::spawn_blocking(move || {
            let c_prompt = to_cstring(&prompt);
            let tx_ptr = &tx as *const mpsc::Sender<StreamToken> as *mut c_void;

            let result = generate_fn(
                c_prompt.as_ptr(),
                max_tokens,
                temperature,
                top_p,
                rust_token_callback,
                tx_ptr,
                ctx.as_ptr(),
            );

            if result < 0 {
                // Wyslij token bledu jesli generowanie sie nie powiodlo
                let _ = tx.blocking_send(StreamToken {
                    text: format!("[BLAD: Swift MLX zwrocil kod {}]", result),
                    is_final: true,
                });
            }

            // tx jest dropowany tutaj — zamyka kanal
            drop(tx);
        });

        Ok(rx)
    }

    async fn embeddings(&self, _params: EmbeddingParams) -> Result<EmbeddingResult> {
        anyhow::bail!("Embeddingi nie sa obslugiwane przez backend mlx-swift")
    }
}

// =============================================================================
// Pomocnicza funkcja do zwalniania pamieci C
// =============================================================================

extern "C" {
    /// Standardowa funkcja free z libc — uzywana do zwalniania pamieci
    /// zaalokowanej po stronie Swift/C (np. JSON string z model_info_fn)
    #[link_name = "free"]
    fn libc_free(ptr: *mut c_void);
}
