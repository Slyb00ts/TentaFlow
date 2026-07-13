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
    max_context_tokens: i32,
    memory_budget_mb: i32,
    token_callback: TokenCallbackFn,
    callback_context: *mut c_void,
    context: *mut c_void,
) -> i32;

/// Sentinel zwracany przez Swift gdy kontekst/model przekracza limit pamieci —
/// guard przerywa generacje zamiast OOM. Rust mapuje to na czysty blad zamiast
/// generycznego "blad generowania". Musi byc zgodny ze strona Swift (@_cdecl).
const MLX_ERR_OUT_OF_MEMORY: i32 = -10;

/// Callback wolany przez Swift dla kazdego wygenerowanego tokena
type TokenCallbackFn = extern "C" fn(
    token_text: *const c_char,
    is_final: bool,
    prompt_tokens: u32,
    completion_tokens: u32,
    prefill_tps: f32,
    completion_tps: f32,
    callback_context: *mut c_void,
);

/// Callback: pobierz info o modelu (nazwa, backend, rozmiar). Zwraca JSON C string (caller musi zwolnic)
type ModelInfoFn = extern "C" fn(context: *mut c_void) -> *mut c_char;

/// Callback: policz embedding dla jednego tekstu. Zwraca bufor `f32` zaalokowany
/// przez malloc (Rust zwalnia przez libc free) i zapisuje dlugosc do `out_len`.
/// NULL = blad. Rejestrowany osobno od czworki gen (`tentaflow_register_mlx_swift_embed`),
/// zeby nie zmieniac sygnatury rejestracji LLM uzywanej tez na iOS.
type EmbedFn =
    extern "C" fn(text: *const c_char, out_len: *mut i32, context: *mut c_void) -> *mut f32;

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

/// Callback embeddingow — rejestrowany osobno, bo nie kazda binarka z bridge'm
/// ma symbol `MLXBridge_embed` (starsze dyliby). Kontekst (singleton silnika)
/// wspoldzielony z `SWIFT_CALLBACKS`.
struct SwiftEmbedCallback {
    embed_fn: EmbedFn,
}
unsafe impl Send for SwiftEmbedCallback {}
unsafe impl Sync for SwiftEmbedCallback {}
static SWIFT_EMBED: OnceLock<SwiftEmbedCallback> = OnceLock::new();

/// Rejestruje callback embeddingow MLX (osobno od czworki gen). Wolane z
/// `mlx_swift_init.rs` gdy dylib eksponuje `MLXBridge_embed`.
#[no_mangle]
pub extern "C" fn tentaflow_register_mlx_swift_embed(embed_fn: EmbedFn) {
    let _ = SWIFT_EMBED.set(SwiftEmbedCallback { embed_fn });
    tracing::info!("Swift MLX embed callback zarejestrowany");
}

/// Callback rerankera — `query` + `docs_json` (JSON array stringow) -> malloc'owany
/// bufor `out_len` floatow (score per dokument, kolejnosc wejsciowa). Rejestrowany
/// osobno (nie kazdy dylib ma `MLXBridge_rerank`). Model rerankera (Qwen3 + projector)
/// ladowany ta sama sciezka co embedder (`load_embedder_model`).
type RerankFn = extern "C" fn(
    query: *const c_char,
    docs_json: *const c_char,
    out_len: *mut i32,
    context: *mut c_void,
) -> *mut f32;

struct SwiftRerankCallback {
    rerank_fn: RerankFn,
}
unsafe impl Send for SwiftRerankCallback {}
unsafe impl Sync for SwiftRerankCallback {}
static SWIFT_RERANK: OnceLock<SwiftRerankCallback> = OnceLock::new();

/// Rejestruje callback rerankera MLX. Wolane z `mlx_swift_init.rs` gdy dylib
/// eksponuje `MLXBridge_rerank`.
#[no_mangle]
pub extern "C" fn tentaflow_register_mlx_swift_rerank(rerank_fn: RerankFn) {
    let _ = SWIFT_RERANK.set(SwiftRerankCallback { rerank_fn });
    tracing::info!("Swift MLX rerank callback zarejestrowany");
}

/// Liczy score'y rerankera in-process (embedded MLX): cross-encoder query vs docs.
/// Zwraca `relevance_score` per dokument W KOLEJNOSCI WEJSCIOWEJ (sortowanie robi
/// caller). Reuzywa model zaladowany przez `load_embedder_model` (Qwen3 reranker).
pub async fn rerank(query: &str, documents: &[String]) -> Result<Vec<f32>> {
    let cb = SWIFT_RERANK.get().context(
        "Swift MLX rerank callback nie zostal zarejestrowany (stary libMLXBridge.dylib?)",
    )?;
    let callbacks = get_callbacks()?;
    let rerank_fn = cb.rerank_fn;
    let ctx = SendPtr::from_raw(callbacks.context);
    let query = query.to_string();
    let docs_json = serde_json::to_string(documents).context("serializacja docs do rerank")?;
    let expected = documents.len();

    // Swift blokuje — na dedykowanym watku.
    let scores = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
        let c_query = to_cstring(&query);
        let c_docs = to_cstring(&docs_json);
        let mut len: i32 = 0;
        let ptr = rerank_fn(
            c_query.as_ptr(),
            c_docs.as_ptr(),
            &mut len as *mut i32,
            ctx.as_ptr(),
        );
        if ptr.is_null() || len <= 0 {
            anyhow::bail!("Swift MLX: rerank zwrocil pusty wynik");
        }
        // SAFETY: Swift zaalokowal `len` floatow przez malloc; kopiujemy i zwalniamy.
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        let vec = slice.to_vec();
        unsafe {
            libc_free(ptr as *mut c_void);
        }
        Ok(vec)
    })
    .await
    .context("Blad watku rerank")??;

    if scores.len() != expected {
        anyhow::bail!(
            "Swift MLX rerank: zwrocono {} score'ow, oczekiwano {}",
            scores.len(),
            expected
        );
    }
    Ok(scores)
}

/// Callback: zaladuj model embeddingow (wymusza sciezke EmbedderModelFactory
/// niezaleznie od `1_Pooling`). Zwraca 0=OK, <0=blad.
type LoadEmbedderFn = extern "C" fn(model_path: *const c_char, context: *mut c_void) -> i32;

struct SwiftLoadEmbedderCallback {
    load_fn: LoadEmbedderFn,
}
unsafe impl Send for SwiftLoadEmbedderCallback {}
unsafe impl Sync for SwiftLoadEmbedderCallback {}
static SWIFT_LOAD_EMBEDDER: OnceLock<SwiftLoadEmbedderCallback> = OnceLock::new();

/// Rejestruje callback ladowania embeddera MLX (osobno od czworki gen). Wolane
/// z `mlx_swift_init.rs` gdy dylib eksponuje `MLXBridge_loadEmbedder`.
#[no_mangle]
pub extern "C" fn tentaflow_register_mlx_swift_load_embedder(load_fn: LoadEmbedderFn) {
    let _ = SWIFT_LOAD_EMBEDDER.set(SwiftLoadEmbedderCallback { load_fn });
    tracing::info!("Swift MLX load-embedder callback zarejestrowany");
}

/// Laduje model embeddingow do slotu embeddera w MLXBridge (osobny od LLM).
/// Wolane przez embedded deploy embeddingow (jina-embed-mlx) — wymusza
/// EmbedderModelFactory niezaleznie od heurystyki `1_Pooling`.
pub async fn load_embedder_model(model_path: &std::path::Path) -> Result<()> {
    let cb = SWIFT_LOAD_EMBEDDER.get().context(
        "Swift MLX load-embedder callback nie zostal zarejestrowany (stary libMLXBridge.dylib?)",
    )?;
    let callbacks = get_callbacks()?;
    let load_fn = cb.load_fn;
    let ctx = SendPtr::from_raw(callbacks.context);
    let path_str = model_path
        .to_str()
        .context("Sciezka modelu zawiera nieprawidlowe znaki UTF-8")?
        .to_string();
    let result = tokio::task::spawn_blocking(move || {
        let c_path = to_cstring(&path_str);
        load_fn(c_path.as_ptr(), ctx.as_ptr())
    })
    .await
    .context("Blad watku ladowania embeddera")?;
    if result < 0 {
        anyhow::bail!("Swift MLX: blad ladowania embeddera (kod: {})", result);
    }
    Ok(())
}

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
    prompt_tokens: u32,
    completion_tokens: u32,
    prefill_tps: f32,
    completion_tps: f32,
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

    // Ignorujemy blad wyslania — moze sie zdarzyc jesli odbiorca zostal porzucony.
    // Swift nie przekazuje powodu zakonczenia w tym callbacku, wiec finish_reason
    // zostawiamy None (konsument mapuje finalny token bez bledu na EndOfText).
    // Liczniki sa niezerowe tylko na tokenie finalnym (Swift liczy prompt/gen).
    let _ = tx.blocking_send(StreamToken {
        text,
        is_final,
        prompt_tokens,
        completion_tokens,
        finish_reason: None,
        error: None,
        prefill_tps,
        completion_tps,
        // MLX nie eksponuje granicy faz dla TTFT — konsument liczy wall-clock.
        ttft_ms: 0,
    });
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
        let max_context_tokens = params.max_context_tokens as i32;
        let memory_budget_mb = params.memory_budget_mb as i32;

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
                max_context_tokens,
                memory_budget_mb,
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

        if gen_result == MLX_ERR_OUT_OF_MEMORY {
            anyhow::bail!(
                "Brak pamieci: kontekst lub model przekracza limit (max_context_tokens={}, memory_budget_mb={}). Zadanie nie zostalo wykonane.",
                params.max_context_tokens,
                params.memory_budget_mb
            );
        }
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
        let max_context_tokens = params.max_context_tokens as i32;
        let memory_budget_mb = params.memory_budget_mb as i32;

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
                max_context_tokens,
                memory_budget_mb,
                rust_token_callback,
                tx_ptr,
                ctx.as_ptr(),
            );

            if result < 0 {
                // Wyslij token bledu jesli generowanie sie nie powiodlo
                let msg = if result == MLX_ERR_OUT_OF_MEMORY {
                    "[BLAD: brak pamieci — kontekst/model przekracza limit, zadanie przerwane]"
                        .to_string()
                } else {
                    format!("[BLAD: Swift MLX zwrocil kod {}]", result)
                };
                let _ = tx.blocking_send(StreamToken {
                    text: String::new(),
                    is_final: true,
                    finish_reason: None,
                    error: Some(msg),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    prefill_tps: 0.0,
                    completion_tps: 0.0,
                    ttft_ms: 0,
                });
            }

            // tx jest dropowany tutaj — zamyka kanal
            drop(tx);
        });

        Ok(rx)
    }

    async fn embeddings(&self, params: EmbeddingParams) -> Result<EmbeddingResult> {
        let embed = SWIFT_EMBED.get().context(
            "Swift MLX embed callback nie zostal zarejestrowany (stary libMLXBridge.dylib?)",
        )?;
        let callbacks = get_callbacks()?;
        let embed_fn = embed.embed_fn;
        let ctx = SendPtr::from_raw(callbacks.context);
        let texts = params.texts.clone();

        // Kazdy tekst liczony osobno na dedykowanym watku — Swift blokuje.
        let embeddings = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in &texts {
                let c_text = to_cstring(t);
                let mut len: i32 = 0;
                let ptr = embed_fn(c_text.as_ptr(), &mut len as *mut i32, ctx.as_ptr());
                if ptr.is_null() || len <= 0 {
                    anyhow::bail!("Swift MLX: embed zwrocil pusty wynik");
                }
                // SAFETY: Swift zaalokowal `len` floatow przez malloc; kopiujemy
                // i zwalniamy przez libc free (ten sam alokator).
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                let vec = slice.to_vec();
                unsafe {
                    libc_free(ptr as *mut c_void);
                }
                out.push(vec);
            }
            Ok(out)
        })
        .await
        .context("Blad watku embeddingow")??;

        let dimensions = embeddings.first().map(|v| v.len()).unwrap_or(0);
        Ok(EmbeddingResult {
            embeddings,
            dimensions,
        })
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
