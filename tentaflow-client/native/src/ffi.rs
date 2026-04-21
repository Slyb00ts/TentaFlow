// ============================================================================
// FFI EXPORTS - Funkcje extern "C" dla P/Invoke z .NET
// ============================================================================
//
// CEL:
// Eksportuje funkcje C-compatible dla wywołań P/Invoke z aplikacji .NET.
// Każda funkcja jest punktem wejścia do natywnej biblioteki QUIC.
//
// JAK DZIAŁA:
// 1. .NET wywołuje funkcję przez P/Invoke (DllImport)
// 2. Funkcja konwertuje argumenty C na typy Rust
// 3. Wywołuje async metodę klienta przez Tokio runtime
// 4. Konwertuje wynik na strukturę C-compatible
// 5. .NET odbiera wynik i musi zwolnić pamięć przez tentaflow_free_*
//
// PRZYKŁAD UŻYCIA (C#):
// ```csharp
// [DllImport("tentaflow_client_native")]
// static extern IntPtr tentaflow_client_new(ref ClientConfig config);
//
// [DllImport("tentaflow_client_native")]
// static extern EmbeddingsResult tentaflow_embeddings(
//     IntPtr client, string model, string[] texts, UIntPtr count);
//
// [DllImport("tentaflow_client_native")]
// static extern void tentaflow_free_embeddings(EmbeddingsResult result);
// ```
//
// KLUCZOWE KONCEPCJE:
// - extern "C": Używa C ABI dla kompatybilności z .NET P/Invoke
// - #[unsafe(no_mangle)]: Zachowuje oryginalne nazwy funkcji
// - OnceCell<Runtime>: Globalny Tokio runtime (singleton)
// - block_on: Blokuje wątek .NET do zakończenia async operacji
//
// KONWENCJE NAZEWNICTWA:
// - tentaflow_<operacja>: Główne operacje API
// - tentaflow_free_<typ>: Zwalnianie pamięci dla danego typu wyniku
// - tentaflow_client_*: Zarządzanie życiem klienta
//
// BEZPIECZEŃSTWO:
// - Wszystkie funkcje sprawdzają null pointery
// - Błędy są zwracane w polu 'error' struktury wyniku
// - Brak panik - wszystkie błędy są obsługiwane gracefully
//
// ============================================================================

use crate::client::{ClientConfigInternal, TentaFlowClient};
use crate::types::*;
use once_cell::sync::OnceCell;
use tentaflow_protocol::SearchMode;
use std::ffi::{c_char, CStr};
use std::ptr;
use tokio::runtime::Runtime;
use tracing::error;

// Global Tokio runtime
static RUNTIME: OnceCell<Runtime> = OnceCell::new();

fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

// ============================================================================
// INITIALIZATION
// ============================================================================

/// Inicjalizuje bibliotekę (opcjonalnie - wywoływane automatycznie).
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_init() {
    // Initialize tracing
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // Initialize runtime
    let _ = get_runtime();
}

// ============================================================================
// CLIENT MANAGEMENT
// ============================================================================

/// Tworzy nowego klienta i łączy się z Router.
///
/// # Parametry
/// - config: Konfiguracja klienta (URL, opcjonalny CA, timeout)
///
/// # Zwraca
/// - Wskaźnik do klienta (lub null jeśli błąd)
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_client_new(config: *const ClientConfig) -> *mut TentaFlowClient {
    if config.is_null() {
        error!("tentaflow_client_new: config is null");
        return ptr::null_mut();
    }

    let config = unsafe { &*config };

    // Convert C strings to Rust strings
    let router_url = unsafe {
        if config.router_url.is_null() {
            error!("router_url is null");
            return ptr::null_mut();
        }
        match CStr::from_ptr(config.router_url).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ptr::null_mut(),
        }
    };

    // Pole `ca_path` z C ABI jest ignorowane — iroh nie uzywa CA bundle.
    // EndpointId jest wbudowany w URL `iroh://<hex>` albo w `router_url`.
    let _ = unsafe {
        if !config.ca_path.is_null() {
            CStr::from_ptr(config.ca_path).to_str().ok();
        }
    };

    let timeout_ms = if config.timeout_ms == 0 { 30000 } else { config.timeout_ms as u64 };

    let internal_config = ClientConfigInternal {
        router_url,
        timeout_ms,
    };

    // Connect
    let result = get_runtime().block_on(async {
        TentaFlowClient::connect(internal_config).await
    });

    match result {
        Ok(client) => Box::into_raw(Box::new(client)),
        Err(e) => {
            error!("Failed to connect: {}", e);
            ptr::null_mut()
        }
    }
}

/// Zamyka klienta i zwalnia pamięć.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_client_free(client: *mut TentaFlowClient) {
    if !client.is_null() {
        let client = unsafe { Box::from_raw(client) };
        get_runtime().block_on(async {
            client.close().await;
        });
        // Box is dropped here, freeing memory
    }
}

/// Sprawdza czy klient jest połączony.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_client_is_connected(client: *const TentaFlowClient) -> bool {
    if client.is_null() {
        return false;
    }
    let client = unsafe { &*client };
    get_runtime().block_on(async {
        client.is_connected().await
    })
}

// ============================================================================
// EMBEDDINGS
// ============================================================================

/// Generuje embeddings dla podanych tekstów.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - model: Nazwa modelu (np. "embeddings-gemma")
/// - texts: Tablica wskaźników do tekstów
/// - texts_count: Liczba tekstów
///
/// # Zwraca
/// - EmbeddingsResult z wektorami lub błędem
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_embeddings(
    client: *const TentaFlowClient,
    model: *const c_char,
    texts: *const *const c_char,
    texts_count: usize,
) -> EmbeddingsResult {
    if client.is_null() || model.is_null() || texts.is_null() {
        return EmbeddingsResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    // Convert model name
    let model = unsafe {
        match CStr::from_ptr(model).to_str() {
            Ok(s) => s,
            Err(_) => return EmbeddingsResult::error("Invalid model name"),
        }
    };

    // Convert texts
    let texts: Vec<String> = unsafe {
        (0..texts_count)
            .filter_map(|i| {
                let ptr = *texts.add(i);
                if ptr.is_null() {
                    None
                } else {
                    CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
                }
            })
            .collect()
    };

    if texts.is_empty() {
        return EmbeddingsResult::error("No texts provided");
    }

    // Call embeddings
    let result = get_runtime().block_on(async {
        client.embeddings(model, texts).await
    });

    match result {
        Ok(metrics) => EmbeddingsResult::success(metrics.embeddings, metrics.latency_ms),
        Err(e) => EmbeddingsResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć EmbeddingsResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_embeddings(result: EmbeddingsResult) {
    if !result.embeddings.is_null() && result.embeddings_count > 0 {
        let total_len = result.embeddings_count * result.dimensions;
        unsafe {
            let _ = Vec::from_raw_parts(result.embeddings, total_len, total_len);
        }
    }
    if !result.error.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// CHAT COMPLETION
// ============================================================================

/// Typ chat template (C-compatible enum).
/// 0 = Auto (serwer formatuje), 1 = Llama3, 2 = ChatML, 3 = Alpaca, 4 = Vicuna, 5 = Mistral
#[repr(C)]
#[derive(Clone, Copy)]
pub enum ChatTemplateType {
    Auto = 0,
    Llama3 = 1,
    ChatML = 2,
    Alpaca = 3,
    Vicuna = 4,
    Mistral = 5,
}

impl From<ChatTemplateType> for crate::chat_template::ChatTemplate {
    fn from(t: ChatTemplateType) -> Self {
        match t {
            ChatTemplateType::Auto => crate::chat_template::ChatTemplate::Auto,
            ChatTemplateType::Llama3 => crate::chat_template::ChatTemplate::Llama3,
            ChatTemplateType::ChatML => crate::chat_template::ChatTemplate::ChatML,
            ChatTemplateType::Alpaca => crate::chat_template::ChatTemplate::Alpaca,
            ChatTemplateType::Vicuna => crate::chat_template::ChatTemplate::Vicuna,
            ChatTemplateType::Mistral => crate::chat_template::ChatTemplate::Mistral,
        }
    }
}

/// Opcje TTS (przekazywane z .NET).
#[repr(C)]
pub struct TtsStreamingOptions {
    /// Model TTS (np. "jarvis")
    pub model: *const c_char,
    /// Głos (np. "jarvis")
    pub voice: *const c_char,
    /// Format audio (np. "wav") - może być null
    pub format: *const c_char,
    /// Prędkość (1.0 = normalna, 0 = default)
    pub speed: f32,
}

/// Opcje Memory (przekazywane z .NET).
#[repr(C)]
pub struct MemoryStreamingOptions {
    /// Czy pamięć jest włączona (1 = true, 0 = false, -1 = default)
    pub enabled: i8,
    /// Identyfikator sesji rozmowy (może być null)
    pub session_id: *const c_char,
    /// ID rozpoznanej osoby (może być null)
    pub person_id: *const c_char,
    /// Poziom pewności rozpoznania głosu (0.0-1.0, <0 = default)
    pub speaker_confidence: f32,
    /// Czy zapisywać do Memory (1 = true, 0 = false, -1 = default)
    pub store_enabled: i8,
    /// Czy odpytywać Memory (1 = true, 0 = false, -1 = default)
    pub query_enabled: i8,
}

/// Opcje chat completion (C-compatible).
#[repr(C)]
pub struct ChatCompletionOptionsNative {
    /// Temperatura (0.0-2.0, <0 dla default)
    pub temperature: f32,
    /// Max tokenów (<0 dla default)
    pub max_tokens: i32,
    /// Typ template (0=Auto)
    pub template_type: ChatTemplateType,
    /// Czy streamować (1=true, 0=false)
    pub stream: u8,
    /// Opcje TTS (może być null)
    pub tts_options: *const TtsStreamingOptions,
    /// Opcje Memory (może być null)
    pub memory_options: *const MemoryStreamingOptions,
    /// ID sesji (może być null)
    pub session_id: *const c_char,
    /// Wskaźnik do danych audio wejściowych (może być null)
    pub audio_input: *const u8,
    /// Długość danych audio wejściowych w bajtach
    pub audio_input_len: usize,
}

/// Zwalnia pamięć ChatCompletionResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_chat_completion(result: ChatCompletionResult) {
    unsafe {
        if !result.content.is_null() {
            let _ = std::ffi::CString::from_raw(result.content);
        }
        if !result.reasoning_content.is_null() {
            let _ = std::ffi::CString::from_raw(result.reasoning_content);
        }
        if !result.model.is_null() {
            let _ = std::ffi::CString::from_raw(result.model);
        }
        if !result.finish_reason.is_null() {
            let _ = std::ffi::CString::from_raw(result.finish_reason);
        }
        if !result.transcribed_text.is_null() {
            let _ = std::ffi::CString::from_raw(result.transcribed_text);
        }
        if !result.speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_id);
        }
        if !result.speaker_name.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_name);
        }
        if !result.detected_intent.is_null() {
            let _ = std::ffi::CString::from_raw(result.detected_intent);
        }
        // Zwolnij detected_tools
        if !result.detected_tools.is_null() && result.detected_tools_count > 0 {
            let tools = std::slice::from_raw_parts_mut(
                result.detected_tools,
                result.detected_tools_count as usize,
            );
            for tool in tools.iter_mut() {
                if !tool.call_id.is_null() {
                    let _ = std::ffi::CString::from_raw(tool.call_id);
                }
                if !tool.tool_name.is_null() {
                    let _ = std::ffi::CString::from_raw(tool.tool_name);
                }
                if !tool.parameters.is_null() {
                    let _ = std::ffi::CString::from_raw(tool.parameters);
                }
                if !tool.follow_up_question.is_null() {
                    let _ = std::ffi::CString::from_raw(tool.follow_up_question);
                }
                // Zwolnij missing_params
                if !tool.missing_params.is_null() && tool.missing_params_count > 0 {
                    let params = std::slice::from_raw_parts_mut(
                        tool.missing_params,
                        tool.missing_params_count as usize,
                    );
                    for param in params.iter_mut() {
                        if !param.is_null() {
                            let _ = std::ffi::CString::from_raw(*param);
                        }
                    }
                    let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        tool.missing_params,
                        tool.missing_params_count as usize,
                    ));
                }
                // Zwolnij execution_result
                if !tool.execution_result.is_null() {
                    let exec = Box::from_raw(tool.execution_result);
                    if !exec.message.is_null() {
                        let _ = std::ffi::CString::from_raw(exec.message);
                    }
                    if !exec.data.is_null() {
                        let _ = std::ffi::CString::from_raw(exec.data);
                    }
                    if !exec.error.is_null() {
                        let _ = std::ffi::CString::from_raw(exec.error);
                    }
                }
            }
            // Zwolnij tablicę tools
            let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                result.detected_tools,
                result.detected_tools_count as usize,
            ));
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Callback dla tokenów (reasoning lub content).
pub type StreamingTokenCallback = extern "C" fn(*const c_char);

/// Callback dla zdarzeń start/end (bez parametrów).
pub type StreamingEventCallback = extern "C" fn();

/// Callback dla audio chunks (data, len).
pub type StreamingAudioCallback = extern "C" fn(*const u8, usize);

/// Wynik anulowania requestu.
#[repr(C)]
pub struct CancelResult {
    /// Czy anulowanie się powiodło
    pub success: bool,
    /// Komunikat błędu (null jeśli sukces)
    pub error: *mut c_char,
}

impl CancelResult {
    pub fn success() -> Self {
        Self {
            success: true,
            error: ptr::null_mut(),
        }
    }

    pub fn error(msg: &str) -> Self {
        let c_str = std::ffi::CString::new(msg).unwrap_or_else(|_| std::ffi::CString::new("Unknown error").unwrap());
        Self {
            success: false,
            error: c_str.into_raw(),
        }
    }
}

/// Uniwersalna funkcja chat completion - obsługuje streaming, TTS i Memory.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - model: Nazwa modelu
/// - messages: Tablica wiadomości
/// - messages_count: Liczba wiadomości
/// - options: Wszystkie opcje (temperatura, max_tokens, template, stream, tts, memory, session_id)
/// - on_reasoning_start/end, on_reasoning: Callbacki dla reasoning (mogą być null)
/// - on_content_start/end, on_content: Callbacki dla content (mogą być null)
/// - on_audio: Callback dla audio chunks jeśli TTS włączony (może być null)
/// - request_id_out: Wskaźnik na string dla request_id (może być null)
///
/// # Zwraca
/// - ChatCompletionResult z odpowiedzią lub błędem
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_chat_completion(
    client: *const TentaFlowClient,
    model: *const c_char,
    messages: *const ChatMessage,
    messages_count: usize,
    options: *const ChatCompletionOptionsNative,
    on_reasoning_start: Option<StreamingEventCallback>,
    on_reasoning: Option<StreamingTokenCallback>,
    on_reasoning_end: Option<StreamingEventCallback>,
    on_content_start: Option<StreamingEventCallback>,
    on_content: Option<StreamingTokenCallback>,
    on_content_end: Option<StreamingEventCallback>,
    on_audio: Option<StreamingAudioCallback>,
    request_id_out: *mut *mut c_char,
) -> ChatCompletionResult {
    use tentaflow_protocol::{TTSStreamingOptions, MemoryOptions};
    use crate::client::ChatCompletionOptions;

    if client.is_null() || model.is_null() || messages.is_null() || options.is_null() {
        return ChatCompletionResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let opts = unsafe { &*options };

    // Convert model name
    let model_str = unsafe {
        match CStr::from_ptr(model).to_str() {
            Ok(s) => s,
            Err(_) => return ChatCompletionResult::error("Invalid model name"),
        }
    };

    // Convert messages
    let msgs: Vec<(String, String)> = unsafe {
        (0..messages_count)
            .filter_map(|i| {
                let msg = &*messages.add(i);
                let role = CStr::from_ptr(msg.role).to_str().ok()?.to_string();
                let content = CStr::from_ptr(msg.content).to_str().ok()?.to_string();
                Some((role, content))
            })
            .collect()
    };

    if msgs.is_empty() {
        return ChatCompletionResult::error("No messages provided");
    }

    // Convert options
    let temperature = if opts.temperature < 0.0 { None } else { Some(opts.temperature) };
    let max_tokens = if opts.max_tokens < 0 { None } else { Some(opts.max_tokens as u32) };
    let template: crate::chat_template::ChatTemplate = opts.template_type.into();
    let stream = opts.stream != 0;

    // Convert session_id
    let session_id = if opts.session_id.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(opts.session_id).to_str().ok().map(|s| s.to_string()) }
    };

    // Convert TTS options
    let tts = if opts.tts_options.is_null() {
        None
    } else {
        let tts_opts = unsafe { &*opts.tts_options };
        if tts_opts.model.is_null() || tts_opts.voice.is_null() {
            None
        } else {
            Some(TTSStreamingOptions {
                model: unsafe { CStr::from_ptr(tts_opts.model).to_str().unwrap_or("").to_string() },
                voice: unsafe { CStr::from_ptr(tts_opts.voice).to_str().unwrap_or("").to_string() },
                format: if tts_opts.format.is_null() { None } else {
                    unsafe { CStr::from_ptr(tts_opts.format).to_str().ok().map(|s| s.to_string()) }
                },
                speed: if tts_opts.speed <= 0.0 { None } else { Some(tts_opts.speed) },
            })
        }
    };

    // Convert Memory options
    let memory = if opts.memory_options.is_null() {
        None
    } else {
        let mem_opts = unsafe { &*opts.memory_options };
        Some(MemoryOptions {
            enabled: if mem_opts.enabled < 0 { None } else { Some(mem_opts.enabled != 0) },
            session_id: if mem_opts.session_id.is_null() { None } else {
                unsafe { CStr::from_ptr(mem_opts.session_id).to_str().ok().map(|s| s.to_string()) }
            },
            person_id: if mem_opts.person_id.is_null() { None } else {
                unsafe { CStr::from_ptr(mem_opts.person_id).to_str().ok().map(|s| s.to_string()) }
            },
            speaker_confidence: if mem_opts.speaker_confidence < 0.0 { None } else { Some(mem_opts.speaker_confidence) },
            store_enabled: if mem_opts.store_enabled < 0 { None } else { Some(mem_opts.store_enabled != 0) },
            query_enabled: if mem_opts.query_enabled < 0 { None } else { Some(mem_opts.query_enabled != 0) },
        })
    };

    // Convert audio_input
    let audio_input = if opts.audio_input.is_null() || opts.audio_input_len == 0 {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(opts.audio_input, opts.audio_input_len).to_vec() })
    };

    if audio_input.is_some() {
        tracing::info!("chat_completion FFI: audio_input present, {} bytes", opts.audio_input_len);
    }

    // Build ChatCompletionOptions
    let client_options = ChatCompletionOptions {
        temperature,
        max_tokens,
        template: Some(template),
        tts,
        memory,
        session_id,
        stream,
        audio_input,
    };

    // Call chat completion
    let result = get_runtime().block_on(async {
        client.chat_completion(
            model_str,
            msgs,
            client_options,
            || { if let Some(cb) = on_reasoning_start { cb(); } },
            |token| { if let Some(cb) = on_reasoning { if let Ok(c) = std::ffi::CString::new(token) { cb(c.as_ptr()); } } },
            || { if let Some(cb) = on_reasoning_end { cb(); } },
            || { if let Some(cb) = on_content_start { cb(); } },
            |token| { if let Some(cb) = on_content { if let Ok(c) = std::ffi::CString::new(token) { cb(c.as_ptr()); } } },
            || { if let Some(cb) = on_content_end { cb(); } },
            |audio| { if let Some(cb) = on_audio { cb(audio.as_ptr(), audio.len()); } },
        ).await
    });

    match result {
        Ok((metrics, request_id)) => {
            if !request_id_out.is_null() {
                if let Ok(c_request_id) = std::ffi::CString::new(request_id) {
                    unsafe { *request_id_out = c_request_id.into_raw(); }
                }
            }
            ChatCompletionResult::success_with_metrics(
                metrics.text,
                metrics.reasoning_content,
                metrics.model,
                Some("stop".to_string()),
                0,
                metrics.completion_tokens,
                metrics.time_to_first_token_ms,
                metrics.latency_ms,
                metrics.tokens_per_sec,
            )
        }
        Err(e) => ChatCompletionResult::error(&e.to_string()),
    }
}

// ============================================================================
// REQUEST CANCELLATION
// ============================================================================

/// Anuluje trwający request.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - request_id: ID requestu do anulowania
/// - reason: Opcjonalny powód anulowania (może być null)
///
/// # Zwraca
/// - CancelResult z wynikiem operacji
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_cancel_request(
    client: *const TentaFlowClient,
    request_id: *const c_char,
    reason: *const c_char,
) -> CancelResult {
    if client.is_null() || request_id.is_null() {
        return CancelResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let request_id = unsafe {
        match CStr::from_ptr(request_id).to_str() {
            Ok(s) => s,
            Err(_) => return CancelResult::error("Invalid request_id"),
        }
    };

    let reason = if reason.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(reason).to_str().ok() }
    };

    let result = get_runtime().block_on(async {
        client.cancel_request(request_id, reason).await
    });

    match result {
        Ok(success) => {
            if success {
                CancelResult::success()
            } else {
                CancelResult::error("Request not found or already completed")
            }
        }
        Err(e) => CancelResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć CancelResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_cancel(result: CancelResult) {
    if !result.error.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// TTS (Text-to-Speech)
// ============================================================================

/// Generuje audio z tekstu.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_tts(
    client: *const TentaFlowClient,
    model: *const c_char,
    text: *const c_char,
    voice: *const c_char,
    format: *const c_char, // nullable
) -> TtsResult {
    if client.is_null() || model.is_null() || text.is_null() || voice.is_null() {
        return TtsResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let model = unsafe { CStr::from_ptr(model).to_str().unwrap_or("") };
    let text = unsafe { CStr::from_ptr(text).to_str().unwrap_or("") };
    let voice = unsafe { CStr::from_ptr(voice).to_str().unwrap_or("") };
    let format = if format.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(format).to_str().ok() }
    };

    let result = get_runtime().block_on(async {
        client.tts(model, text, voice, format).await
    });

    match result {
        Ok((audio, fmt, latency_ms, audio_duration_sec)) => {
            TtsResult::success(audio, &fmt, latency_ms, audio_duration_sec)
        }
        Err(e) => TtsResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć TtsResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_tts(result: TtsResult) {
    if !result.audio_data.is_null() && result.audio_len > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(result.audio_data, result.audio_len, result.audio_len);
        }
    }
    unsafe {
        if !result.format.is_null() {
            let _ = std::ffi::CString::from_raw(result.format);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// STT (Speech-to-Text)
// ============================================================================

/// Transkrybuje audio na tekst.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_stt(
    client: *const TentaFlowClient,
    model: *const c_char,
    audio_data: *const u8,
    audio_len: usize,
    language: *const c_char, // nullable
) -> SttResult {
    if client.is_null() || model.is_null() || audio_data.is_null() || audio_len == 0 {
        return SttResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let model = unsafe { CStr::from_ptr(model).to_str().unwrap_or("") };
    let audio: Vec<u8> = unsafe {
        std::slice::from_raw_parts(audio_data, audio_len).to_vec()
    };
    let language = if language.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(language).to_str().ok() }
    };

    let result = get_runtime().block_on(async {
        client.stt(model, audio, language).await
    });

    match result {
        Ok((text, lang, duration)) => SttResult::success(text, lang, duration),
        Err(e) => SttResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć SttResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_stt(result: SttResult) {
    unsafe {
        if !result.text.is_null() {
            let _ = std::ffi::CString::from_raw(result.text);
        }
        if !result.language.is_null() {
            let _ = std::ffi::CString::from_raw(result.language);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Speech-to-Text z pełnymi opcjami.
///
/// Parametry:
/// - `client`: Wskaźnik na klienta
/// - `model`: Nazwa modelu Whisper (null-terminated)
/// - `audio_data`: Wskaźnik na dane audio
/// - `audio_len`: Długość danych audio
/// - `options`: Opcje STT (SttOptions struct)
///
/// Zwraca: SttDetailedResult z tekstem, segmentami i metrykami
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_stt_with_options(
    client: *const TentaFlowClient,
    model: *const c_char,
    audio_data: *const u8,
    audio_len: usize,
    options: SttOptions,
) -> SttDetailedResult {
    if client.is_null() || model.is_null() || audio_data.is_null() || audio_len == 0 {
        return SttDetailedResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let model = unsafe { CStr::from_ptr(model).to_str().unwrap_or("") };
    let audio: Vec<u8> = unsafe {
        std::slice::from_raw_parts(audio_data, audio_len).to_vec()
    };

    // Konwertuj SttOptions na SttOptionsInternal
    let language = if options.language.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(options.language).to_str().ok().map(|s| s.to_string()) }
    };

    let prompt = if options.prompt.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(options.prompt).to_str().ok().map(|s| s.to_string()) }
    };

    let response_format = if options.response_format.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(options.response_format).to_str().ok().map(|s| s.to_string()) }
    };

    let timestamp_granularities = if options.timestamp_granularities.is_null() {
        None
    } else {
        // timestamp_granularities to pojedynczy string - konwertuj na Vec
        unsafe {
            CStr::from_ptr(options.timestamp_granularities)
                .to_str()
                .ok()
                .map(|s| vec![s.to_string()])
        }
    };

    // Progi filtrowania - wartości ujemne oznaczają "wyłączone"
    let no_speech_threshold = if options.no_speech_threshold < 0.0 {
        None
    } else {
        Some(options.no_speech_threshold)
    };

    let avg_logprob_threshold = if options.avg_logprob_threshold < -100.0 {
        None
    } else {
        Some(options.avg_logprob_threshold)
    };

    let compression_ratio_threshold = if options.compression_ratio_threshold < 0.0 {
        None
    } else {
        Some(options.compression_ratio_threshold)
    };

    let temperature = if options.temperature < 0.0 {
        None
    } else {
        Some(options.temperature)
    };

    let internal_options = crate::client::SttOptionsInternal {
        language,
        prompt,
        response_format,
        temperature,
        timestamp_granularities,
        no_speech_threshold,
        avg_logprob_threshold,
        compression_ratio_threshold,
    };

    let result = get_runtime().block_on(async {
        client.stt_with_options(model, audio, internal_options).await
    });

    match result {
        Ok(data) => {
            // Konwertuj segmenty na FFI-safe format
            let segments: Vec<SttSegment> = data.segments
                .into_iter()
                .map(|s| SttSegment::new(
                    s.id,
                    s.start,
                    s.end,
                    s.text,
                    s.avg_logprob,
                    s.no_speech_prob,
                    s.compression_ratio,
                    s.temperature,
                    s.speaker_label,
                    s.speaker_similarity,
                    s.is_known_speaker,
                ))
                .collect();

            SttDetailedResult::success(
                data.text,
                data.language,
                data.duration_seconds,
                segments,
                data.filtered_segments_count,
                data.latency_ms,
            )
        }
        Err(e) => SttDetailedResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć SttDetailedResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_stt_detailed(result: SttDetailedResult) {
    unsafe {
        if !result.text.is_null() {
            let _ = std::ffi::CString::from_raw(result.text);
        }
        if !result.language.is_null() {
            let _ = std::ffi::CString::from_raw(result.language);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
        // Zwolnij segmenty
        if !result.segments.is_null() && result.segments_count > 0 {
            let segments = std::slice::from_raw_parts_mut(result.segments, result.segments_count as usize);
            for segment in segments.iter_mut() {
                if !segment.text.is_null() {
                    let _ = std::ffi::CString::from_raw(segment.text);
                }
                if !segment.speaker_label.is_null() {
                    let _ = std::ffi::CString::from_raw(segment.speaker_label);
                }
            }
            // Zwolnij tablicę segmentów
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(result.segments, result.segments_count as usize));
        }
    }
}

// ============================================================================
// SPEAKER IDENTIFICATION
// ============================================================================

/// Rejestruje nowego mówcę lub dodaje próbki do istniejącego.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - speaker_id: Unikalny ID mówcy
/// - speaker_name: Nazwa mówcy
/// - audio_samples: Tablica wskaźników do próbek audio
/// - sample_lengths: Tablica długości próbek
/// - samples_count: Liczba próbek
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_enroll(
    client: *const TentaFlowClient,
    speaker_id: *const c_char,
    speaker_name: *const c_char,
    audio_samples: *const *const u8,
    sample_lengths: *const usize,
    samples_count: usize,
) -> SpeakerEnrollResult {
    if client.is_null() || speaker_id.is_null() || speaker_name.is_null() {
        return SpeakerEnrollResult::error("Invalid arguments");
    }
    if samples_count > 0 && (audio_samples.is_null() || sample_lengths.is_null()) {
        return SpeakerEnrollResult::error("Invalid audio samples");
    }

    let client = unsafe { &*client };
    let speaker_id = unsafe { CStr::from_ptr(speaker_id).to_str().unwrap_or("") };
    let speaker_name = unsafe { CStr::from_ptr(speaker_name).to_str().unwrap_or("") };

    // Konwertuj tablice C do Vec<Vec<u8>>
    let samples: Vec<Vec<u8>> = if samples_count > 0 {
        unsafe {
            let ptrs = std::slice::from_raw_parts(audio_samples, samples_count);
            let lens = std::slice::from_raw_parts(sample_lengths, samples_count);
            ptrs.iter()
                .zip(lens.iter())
                .map(|(&ptr, &len)| std::slice::from_raw_parts(ptr, len).to_vec())
                .collect()
        }
    } else {
        Vec::new()
    };

    let result = get_runtime().block_on(async {
        client.speaker_enroll(speaker_id, speaker_name, samples, None).await
    });

    match result {
        Ok((id, name, samples_processed, embeddings_added, is_new, latency_ms)) => {
            SpeakerEnrollResult::success(id, name, samples_processed, embeddings_added, is_new, latency_ms)
        }
        Err(e) => SpeakerEnrollResult::error(&e.to_string()),
    }
}

/// Dodaje próbki audio do istniejącego mówcy.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_add_samples(
    client: *const TentaFlowClient,
    speaker_id: *const c_char,
    audio_samples: *const *const u8,
    sample_lengths: *const usize,
    samples_count: usize,
) -> SpeakerEnrollResult {
    if client.is_null() || speaker_id.is_null() {
        return SpeakerEnrollResult::error("Invalid arguments");
    }
    if samples_count > 0 && (audio_samples.is_null() || sample_lengths.is_null()) {
        return SpeakerEnrollResult::error("Invalid audio samples");
    }

    let client = unsafe { &*client };
    let speaker_id = unsafe { CStr::from_ptr(speaker_id).to_str().unwrap_or("") };

    // Konwertuj tablice C do Vec<Vec<u8>>
    let samples: Vec<Vec<u8>> = if samples_count > 0 {
        unsafe {
            let ptrs = std::slice::from_raw_parts(audio_samples, samples_count);
            let lens = std::slice::from_raw_parts(sample_lengths, samples_count);
            ptrs.iter()
                .zip(lens.iter())
                .map(|(&ptr, &len)| std::slice::from_raw_parts(ptr, len).to_vec())
                .collect()
        }
    } else {
        Vec::new()
    };

    let result = get_runtime().block_on(async {
        client.speaker_add_samples(speaker_id, samples).await
    });

    match result {
        Ok((id, name, samples_processed, embeddings_added, latency_ms)) => {
            // is_new = false dla add_samples
            SpeakerEnrollResult::success(id, name, samples_processed, embeddings_added, false, latency_ms)
        }
        Err(e) => SpeakerEnrollResult::error(&e.to_string()),
    }
}

/// Usuwa mówcę z bazy głosów.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_remove(
    client: *const TentaFlowClient,
    speaker_id: *const c_char,
) -> SpeakerRemoveResult {
    if client.is_null() || speaker_id.is_null() {
        return SpeakerRemoveResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let speaker_id = unsafe { CStr::from_ptr(speaker_id).to_str().unwrap_or("") };

    let result = get_runtime().block_on(async {
        client.speaker_remove(speaker_id).await
    });

    match result {
        Ok((id, success, latency_ms)) => SpeakerRemoveResult::success(id, success, latency_ms),
        Err(e) => SpeakerRemoveResult::error(&e.to_string()),
    }
}

/// Pobiera listę wszystkich mówców.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_list(
    client: *const TentaFlowClient,
) -> SpeakerListResult {
    if client.is_null() {
        return SpeakerListResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let result = get_runtime().block_on(async {
        client.speaker_list().await
    });

    match result {
        Ok((speakers, total_count, latency_ms)) => SpeakerListResult::success(speakers, total_count, latency_ms),
        Err(e) => SpeakerListResult::error(&e.to_string()),
    }
}

/// Pobiera informacje o bazie głosów.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_info(
    client: *const TentaFlowClient,
) -> SpeakerInfoResult {
    if client.is_null() {
        return SpeakerInfoResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let result = get_runtime().block_on(async {
        client.speaker_info().await
    });

    match result {
        Ok((speaker_count, embedding_dim, similarity_threshold, latency_ms)) => {
            SpeakerInfoResult::success(speaker_count, embedding_dim, similarity_threshold, latency_ms)
        }
        Err(e) => SpeakerInfoResult::error(&e.to_string()),
    }
}

/// Identyfikuje mówcę na podstawie próbki audio.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - audio_data: Dane audio
/// - audio_len: Długość danych audio
/// - threshold: Próg similarity (-1.0 = domyślny)
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_identify(
    client: *const TentaFlowClient,
    audio_data: *const u8,
    audio_len: usize,
    threshold: f32,
) -> SpeakerIdentifyResult {
    if client.is_null() || audio_data.is_null() || audio_len == 0 {
        return SpeakerIdentifyResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let audio = unsafe { std::slice::from_raw_parts(audio_data, audio_len).to_vec() };
    let threshold_opt = if threshold < 0.0 { None } else { Some(threshold) };

    let result = get_runtime().block_on(async {
        client.speaker_identify(audio, threshold_opt).await
    });

    match result {
        Ok((is_match, speaker_id, speaker_name, similarity, threshold, latency_ms)) => {
            SpeakerIdentifyResult::success(is_match, speaker_id, speaker_name, similarity, threshold, latency_ms)
        }
        Err(e) => SpeakerIdentifyResult::error(&e.to_string()),
    }
}

/// Weryfikuje czy próbka audio należy do konkretnego mówcy.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - speaker_id: ID mówcy do weryfikacji
/// - audio_data: Dane audio
/// - audio_len: Długość danych audio
/// - threshold: Próg similarity (-1.0 = domyślny)
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_speaker_verify(
    client: *const TentaFlowClient,
    speaker_id: *const c_char,
    audio_data: *const u8,
    audio_len: usize,
    threshold: f32,
) -> SpeakerVerifyResult {
    if client.is_null() || speaker_id.is_null() || audio_data.is_null() || audio_len == 0 {
        return SpeakerVerifyResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let speaker_id = unsafe { CStr::from_ptr(speaker_id).to_str().unwrap_or("") };
    let audio = unsafe { std::slice::from_raw_parts(audio_data, audio_len).to_vec() };
    let threshold_opt = if threshold < 0.0 { None } else { Some(threshold) };

    let result = get_runtime().block_on(async {
        client.speaker_verify(speaker_id, audio, threshold_opt).await
    });

    match result {
        Ok((id, is_verified, similarity, threshold, detected_speaker_id, latency_ms)) => {
            SpeakerVerifyResult::success(id, is_verified, similarity, threshold, detected_speaker_id, latency_ms)
        }
        Err(e) => SpeakerVerifyResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć wyniku SpeakerEnroll.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_enroll(result: SpeakerEnrollResult) {
    unsafe {
        if !result.speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_id);
        }
        if !result.speaker_name.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_name);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Zwalnia pamięć wyniku SpeakerRemove.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_remove(result: SpeakerRemoveResult) {
    unsafe {
        if !result.speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_id);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Zwalnia pamięć wyniku SpeakerList.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_list(result: SpeakerListResult) {
    unsafe {
        if !result.speakers.is_null() && result.speakers_count > 0 {
            let speakers = std::slice::from_raw_parts_mut(result.speakers, result.speakers_count as usize);
            for speaker in speakers.iter_mut() {
                if !speaker.speaker_id.is_null() {
                    let _ = std::ffi::CString::from_raw(speaker.speaker_id);
                }
                if !speaker.speaker_name.is_null() {
                    let _ = std::ffi::CString::from_raw(speaker.speaker_name);
                }
            }
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(result.speakers, result.speakers_count as usize));
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Zwalnia pamięć wyniku SpeakerInfo.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_info(result: SpeakerInfoResult) {
    unsafe {
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Zwalnia pamięć wyniku SpeakerIdentify.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_identify(result: SpeakerIdentifyResult) {
    unsafe {
        if !result.speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_id);
        }
        if !result.speaker_name.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_name);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Zwalnia pamięć wyniku SpeakerVerify.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_speaker_verify(result: SpeakerVerifyResult) {
    unsafe {
        if !result.speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.speaker_id);
        }
        if !result.detected_speaker_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.detected_speaker_id);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// RAG
// ============================================================================

/// Wysyła zapytanie RAG z pełną kontrolą parametrów.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - query: Zapytanie użytkownika
/// - top_k: Maksymalna liczba wyników
/// - min_similarity: Minimalny próg podobieństwa (0.0-1.0)
/// - search_modes_flags: Bitflagi trybów wyszukiwania:
///   - 0x01 = FullTextSearch
///   - 0x02 = VectorSearch
///   - 0x04 = HiRAG
///   - 0x08 = GSW
///   - 0 = domyślnie VectorSearch
/// - use_reranking: -1 = None, 0 = false, 1 = true
/// - requires_llm: -1 = None (false), 0 = false, 1 = true
/// - requires_audio: -1 = None (false), 0 = false, 1 = true
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_rag(
    client: *const TentaFlowClient,
    query: *const c_char,
    top_k: u32,
    min_similarity: f32,
    search_modes_flags: u32,
    use_reranking: i32,
    requires_llm: i32,
    requires_audio: i32,
) -> RagResult {
    if client.is_null() || query.is_null() {
        return RagResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let query = unsafe { CStr::from_ptr(query).to_str().unwrap_or("") };

    // Konwertuj bitflagi na Vec<SearchMode>
    let search_modes = if search_modes_flags == 0 {
        None // domyślnie VectorSearch
    } else {
        let mut modes = Vec::new();
        if search_modes_flags & 0x01 != 0 {
            modes.push(SearchMode::FullTextSearch);
        }
        if search_modes_flags & 0x02 != 0 {
            modes.push(SearchMode::VectorSearch);
        }
        if search_modes_flags & 0x04 != 0 {
            modes.push(SearchMode::HiRAG);
        }
        if search_modes_flags & 0x08 != 0 {
            modes.push(SearchMode::GSW);
        }
        if modes.is_empty() {
            None
        } else {
            Some(modes)
        }
    };

    // Konwertuj -1/0/1 na Option<bool>
    let use_reranking_opt = match use_reranking {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    let requires_llm_opt = match requires_llm {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    let requires_audio_opt = match requires_audio {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };

    let result = get_runtime().block_on(async {
        client.rag(
            query,
            top_k,
            min_similarity,
            search_modes,
            use_reranking_opt,
            requires_llm_opt,
            requires_audio_opt,
        ).await
    });

    match result {
        Ok(rag_data) => {
            // Konwertuj chunki z client::RagChunkData na types::RagChunkInfo
            let chunks: Vec<RagChunkInfo> = rag_data.chunks.into_iter().map(|c| {
                // Konwertuj dokumenty
                let documents: Vec<ChunkDocument> = c.documents.into_iter().map(|d| {
                    ChunkDocument::new(d.doc_id, d.metadata)
                }).collect();

                RagChunkInfo::new(
                    c.chunk_id,
                    c.chunk_text,
                    c.source_file,
                    c.source_type,
                    c.similarity_score,
                    c.rank,
                    c.chunk_index,
                    documents,
                )
            }).collect();

            RagResult::success(rag_data.response, rag_data.chunks_found, rag_data.requires_llm, chunks)
        }
        Err(e) => RagResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć RagResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_rag(result: RagResult) {
    unsafe {
        if !result.response.is_null() {
            let _ = std::ffi::CString::from_raw(result.response);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
        // Zwolnij tablicę chunków
        if !result.chunks.is_null() && result.chunks_count > 0 {
            let chunks = std::slice::from_raw_parts_mut(result.chunks, result.chunks_count as usize);
            for chunk in chunks {
                if !chunk.chunk_id.is_null() {
                    let _ = std::ffi::CString::from_raw(chunk.chunk_id);
                }
                if !chunk.chunk_text.is_null() {
                    let _ = std::ffi::CString::from_raw(chunk.chunk_text);
                }
                if !chunk.source_file.is_null() {
                    let _ = std::ffi::CString::from_raw(chunk.source_file);
                }
                if !chunk.source_type.is_null() {
                    let _ = std::ffi::CString::from_raw(chunk.source_type);
                }
                // Zwolnij documents dla tego chunka
                if !chunk.documents.is_null() && chunk.documents_count > 0 {
                    let docs = std::slice::from_raw_parts_mut(chunk.documents, chunk.documents_count as usize);
                    for doc in docs {
                        if !doc.doc_id.is_null() {
                            let _ = std::ffi::CString::from_raw(doc.doc_id);
                        }
                        // Zwolnij metadata dla tego dokumentu
                        if !doc.metadata.is_null() && doc.metadata_count > 0 {
                            let meta = std::slice::from_raw_parts_mut(doc.metadata, doc.metadata_count as usize);
                            for kv in meta {
                                if !kv.key.is_null() {
                                    let _ = std::ffi::CString::from_raw(kv.key);
                                }
                                if !kv.value.is_null() {
                                    let _ = std::ffi::CString::from_raw(kv.value);
                                }
                            }
                            // Zwolnij tablicę metadata
                            let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                                doc.metadata,
                                doc.metadata_count as usize,
                            ));
                        }
                    }
                    // Zwolnij tablicę documents
                    let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        chunk.documents,
                        chunk.documents_count as usize,
                    ));
                }
            }
            // Zwolnij samą tablicę chunków
            let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                result.chunks,
                result.chunks_count as usize,
            ));
        }
    }
}

// ============================================================================
// INGEST (Document Ingestion)
// ============================================================================

/// Dodaje dokument tekstowy do RAG.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - document_id: Unikalny ID dokumentu
/// - text: Treść dokumentu
/// - metadata: Tablica par klucz-wartość (może być null)
/// - metadata_count: Liczba par metadata
///
/// # Zwraca
/// - IngestResult z wynikiem operacji
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_ingest_text(
    client: *const TentaFlowClient,
    document_id: *const c_char,
    text: *const c_char,
    metadata: *const MetadataEntry,
    metadata_count: usize,
) -> IngestResult {
    if client.is_null() || document_id.is_null() || text.is_null() {
        return IngestResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let document_id = unsafe {
        match CStr::from_ptr(document_id).to_str() {
            Ok(s) => s,
            Err(_) => return IngestResult::error("Invalid document_id"),
        }
    };

    let text = unsafe {
        match CStr::from_ptr(text).to_str() {
            Ok(s) => s,
            Err(_) => return IngestResult::error("Invalid text"),
        }
    };

    // Convert metadata
    let meta: Vec<(String, String)> = if metadata.is_null() || metadata_count == 0 {
        vec![]
    } else {
        unsafe {
            (0..metadata_count)
                .filter_map(|i| {
                    let entry = &*metadata.add(i);
                    if entry.key.is_null() || entry.value.is_null() {
                        return None;
                    }
                    let key = CStr::from_ptr(entry.key).to_str().ok()?.to_string();
                    let value = CStr::from_ptr(entry.value).to_str().ok()?.to_string();
                    Some((key, value))
                })
                .collect()
        }
    };

    // Call ingest_text
    let result = get_runtime().block_on(async {
        client.ingest_text(document_id, text, meta).await
    });

    match result {
        Ok(response) => {
            let status = match response.status {
                tentaflow_protocol::IngestionStatus::Success => 0,
                tentaflow_protocol::IngestionStatus::Duplicate => 1,
                tentaflow_protocol::IngestionStatus::Updated => 2,
                tentaflow_protocol::IngestionStatus::LinkedToDuplicate => 3,
                tentaflow_protocol::IngestionStatus::Error => 4,
            };
            IngestResult::success(
                response.document_id,
                status,
                response.chunk_count,
                response.vector_count,
                response.metrics.total_ms as u32,
            )
        }
        Err(e) => IngestResult::error(&e.to_string()),
    }
}

/// Dodaje plik binarny do RAG.
///
/// # Parametry
/// - client: Wskaźnik do klienta
/// - document_id: Unikalny ID dokumentu
/// - filename: Nazwa pliku (z rozszerzeniem)
/// - data: Surowe bajty pliku
/// - data_len: Długość danych w bajtach
/// - metadata: Tablica par klucz-wartość (może być null)
/// - metadata_count: Liczba par metadata
///
/// # Zwraca
/// - IngestResult z wynikiem operacji
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_ingest_file(
    client: *const TentaFlowClient,
    document_id: *const c_char,
    filename: *const c_char,
    data: *const u8,
    data_len: usize,
    metadata: *const MetadataEntry,
    metadata_count: usize,
) -> IngestResult {
    if client.is_null() || document_id.is_null() || filename.is_null() || data.is_null() || data_len == 0 {
        return IngestResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let document_id = unsafe {
        match CStr::from_ptr(document_id).to_str() {
            Ok(s) => s,
            Err(_) => return IngestResult::error("Invalid document_id"),
        }
    };

    let filename = unsafe {
        match CStr::from_ptr(filename).to_str() {
            Ok(s) => s,
            Err(_) => return IngestResult::error("Invalid filename"),
        }
    };

    let file_data: Vec<u8> = unsafe {
        std::slice::from_raw_parts(data, data_len).to_vec()
    };

    // Convert metadata
    let meta: Vec<(String, String)> = if metadata.is_null() || metadata_count == 0 {
        vec![]
    } else {
        unsafe {
            (0..metadata_count)
                .filter_map(|i| {
                    let entry = &*metadata.add(i);
                    if entry.key.is_null() || entry.value.is_null() {
                        return None;
                    }
                    let key = CStr::from_ptr(entry.key).to_str().ok()?.to_string();
                    let value = CStr::from_ptr(entry.value).to_str().ok()?.to_string();
                    Some((key, value))
                })
                .collect()
        }
    };

    // Call ingest_file
    let result = get_runtime().block_on(async {
        client.ingest_file(document_id, filename, file_data, meta).await
    });

    match result {
        Ok(response) => {
            let status = match response.status {
                tentaflow_protocol::IngestionStatus::Success => 0,
                tentaflow_protocol::IngestionStatus::Duplicate => 1,
                tentaflow_protocol::IngestionStatus::Updated => 2,
                tentaflow_protocol::IngestionStatus::LinkedToDuplicate => 3,
                tentaflow_protocol::IngestionStatus::Error => 4,
            };
            IngestResult::success(
                response.document_id,
                status,
                response.chunk_count,
                response.vector_count,
                response.metrics.total_ms as u32,
            )
        }
        Err(e) => IngestResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć IngestResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_ingest(result: IngestResult) {
    unsafe {
        if !result.document_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.document_id);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// CONVERSATION SESSIONS
// ============================================================================

/// Rozpoczyna nową sesję konwersacji głosowej.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_conversation_start(
    client: *const TentaFlowClient,
    config: *const ConversationSessionConfig,
) -> ConversationStartResult {
    if client.is_null() || config.is_null() {
        return ConversationStartResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };
    let config = unsafe { &*config };

    // Convert C strings to Rust strings
    let user_id = unsafe {
        if config.user_id.is_null() {
            None
        } else {
            CStr::from_ptr(config.user_id).to_str().ok().map(|s| s.to_string())
        }
    };

    let language = unsafe {
        if config.language.is_null() {
            Some("pl".to_string())
        } else {
            CStr::from_ptr(config.language).to_str().ok().map(|s| s.to_string())
        }
    };

    let stt_model = unsafe {
        if config.stt_model.is_null() {
            Some("whisper".to_string())
        } else {
            CStr::from_ptr(config.stt_model).to_str().ok().map(|s| s.to_string())
        }
    };

    let wake_words: Vec<String> = unsafe {
        if config.wake_words.is_null() {
            vec!["jarvis".to_string(), "hej jarvis".to_string()]
        } else {
            CStr::from_ptr(config.wake_words)
                .to_str()
                .ok()
                .map(|s| s.split(',').map(|w| w.trim().to_string()).collect())
                .unwrap_or_default()
        }
    };

    let stop_phrases: Vec<String> = unsafe {
        if config.stop_phrases.is_null() {
            vec!["dzięki jarvis to koniec".to_string(), "jarvis koniec".to_string()]
        } else {
            CStr::from_ptr(config.stop_phrases)
                .to_str()
                .ok()
                .map(|s| s.split(',').map(|w| w.trim().to_string()).collect())
                .unwrap_or_default()
        }
    };

    let silence_timeout_ms = if config.silence_timeout_ms == 0 { 30000 } else { config.silence_timeout_ms };

    let result = get_runtime().block_on(async {
        client.conversation_start(
            config.mode,
            user_id,
            language,
            stt_model,
            wake_words,
            stop_phrases,
            silence_timeout_ms,
            config.pre_wake_buffer_ms,
        ).await
    });

    match result {
        Ok((session_id, state)) => ConversationStartResult::success(session_id, state),
        Err(e) => ConversationStartResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć ConversationStartResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_conversation_start(result: ConversationStartResult) {
    unsafe {
        if !result.session_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.session_id);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Wysyła audio do aktywnej sesji konwersacji.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_conversation_audio(
    client: *const TentaFlowClient,
    session_id: *const c_char,
    audio_data: *const u8,
    audio_len: usize,
    timestamp_ms: u64,
) -> ConversationAudioResult {
    if client.is_null() || session_id.is_null() || audio_data.is_null() || audio_len == 0 {
        return ConversationAudioResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let session_id_str = unsafe {
        match CStr::from_ptr(session_id).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ConversationAudioResult::error("Invalid session_id"),
        }
    };

    let audio = unsafe { std::slice::from_raw_parts(audio_data, audio_len).to_vec() };

    let result = get_runtime().block_on(async {
        client.conversation_audio(&session_id_str, audio, timestamp_ms).await
    });

    match result {
        Ok((state, events, transcription, confidence)) => {
            ConversationAudioResult::success(session_id_str, state, events, transcription, confidence)
        }
        Err(e) => ConversationAudioResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć ConversationAudioResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_conversation_audio(result: ConversationAudioResult) {
    unsafe {
        if !result.session_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.session_id);
        }
        if !result.events.is_null() && result.events_count > 0 {
            let events = Vec::from_raw_parts(
                result.events,
                result.events_count as usize,
                result.events_count as usize,
            );
            for event in events {
                if !event.transcription.is_null() {
                    let _ = std::ffi::CString::from_raw(event.transcription);
                }
                if !event.wake_word.is_null() {
                    let _ = std::ffi::CString::from_raw(event.wake_word);
                }
                if !event.stop_phrase.is_null() {
                    let _ = std::ffi::CString::from_raw(event.stop_phrase);
                }
                if !event.user_id.is_null() {
                    let _ = std::ffi::CString::from_raw(event.user_id);
                }
            }
        }
        if !result.transcription.is_null() {
            let _ = std::ffi::CString::from_raw(result.transcription);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Kończy sesję konwersacji.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_conversation_end(
    client: *const TentaFlowClient,
    session_id: *const c_char,
    reason: *const c_char,
) -> ConversationEndResult {
    if client.is_null() || session_id.is_null() {
        return ConversationEndResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let session_id_str = unsafe {
        match CStr::from_ptr(session_id).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ConversationEndResult::error("Invalid session_id"),
        }
    };

    let reason_str = unsafe {
        if reason.is_null() {
            None
        } else {
            CStr::from_ptr(reason).to_str().ok().map(|s| s.to_string())
        }
    };

    let result = get_runtime().block_on(async {
        client.conversation_end(&session_id_str, reason_str).await
    });

    match result {
        Ok((final_transcription, stats)) => {
            ConversationEndResult::success(session_id_str, final_transcription, stats)
        }
        Err(e) => ConversationEndResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć ConversationEndResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_conversation_end(result: ConversationEndResult) {
    unsafe {
        if !result.session_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.session_id);
        }
        if !result.final_transcription.is_null() {
            let _ = std::ffi::CString::from_raw(result.final_transcription);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

/// Pobiera status sesji konwersacji.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_conversation_status(
    client: *const TentaFlowClient,
    session_id: *const c_char,
) -> ConversationStatusResult {
    if client.is_null() || session_id.is_null() {
        return ConversationStatusResult::error("Invalid arguments");
    }

    let client = unsafe { &*client };

    let session_id_str = unsafe {
        match CStr::from_ptr(session_id).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ConversationStatusResult::error("Invalid session_id"),
        }
    };

    let result = get_runtime().block_on(async {
        client.conversation_status(&session_id_str).await
    });

    match result {
        Ok((exists, state, mode, duration_ms, last_activity_ms)) => {
            ConversationStatusResult::success(
                session_id_str,
                exists,
                state,
                mode,
                duration_ms,
                last_activity_ms,
            )
        }
        Err(e) => ConversationStatusResult::error(&e.to_string()),
    }
}

/// Zwalnia pamięć ConversationStatusResult.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_conversation_status(result: ConversationStatusResult) {
    unsafe {
        if !result.session_id.is_null() {
            let _ = std::ffi::CString::from_raw(result.session_id);
        }
        if !result.error.is_null() {
            let _ = std::ffi::CString::from_raw(result.error);
        }
    }
}

// ============================================================================
// UTILITY
// ============================================================================

/// Zwalnia string alokowany przez bibliotekę.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(s);
        }
    }
}

/// Zwraca wersję biblioteki.
#[unsafe(no_mangle)]
pub extern "C" fn tentaflow_version() -> *const c_char {
    static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr() as *const c_char
}
