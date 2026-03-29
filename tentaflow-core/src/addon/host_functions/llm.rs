// =============================================================================
// Plik: addon/host_functions/llm.rs
// Opis: Host functions LLM API — generowanie tekstu (synchroniczne i strumieniowe).
//       Addon wywoluje te funkcje aby korzystac z modeli LLM dostepnych w Core.
// =============================================================================

use tracing::{info, warn};

use super::{
    AddonState, ABI_ERR_PERMISSION, ABI_ERR_OPERATION,
    get_memory, read_guest_string, write_guest_output, audit_log, check_permission,
    WasmCaller,
};

// =============================================================================
// llm_generate — synchroniczne generowanie tekstu
// =============================================================================

/// Host function: generuje tekst za pomoca LLM (synchronicznie).
///
/// ABI:
/// - prompt_ptr/prompt_len: wskaznik do UTF-8 stringa z promptem
/// - model_ptr/model_len: opcjonalna nazwa modelu (0,0 = domyslny)
/// - options_ptr/options_len: JSON z opcjami {temperature, max_tokens, ...}
/// - out_ptr/out_cap: bufor na odpowiedz
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn llm_generate(
    mut caller: WasmCaller<'_, AddonState>,
    prompt_ptr: i32,
    prompt_len: i32,
    model_ptr: i32,
    model_len: i32,
    options_ptr: i32,
    options_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj prompt z pamieci WASM
    let prompt = match read_guest_string(&memory, &caller, prompt_ptr, prompt_len) {
        Some(s) => s.to_string(),
        None => {
            warn!("llm_generate: niepoprawny wskaznik promptu");
            return ABI_ERR_OPERATION;
        }
    };

    // Odczytaj opcjonalna nazwe modelu
    let model_name = if model_ptr != 0 && model_len > 0 {
        read_guest_string(&memory, &caller, model_ptr, model_len)
            .map(|s| s.to_string())
    } else {
        None
    };

    // Odczytaj opcje jako JSON
    let _options_json = if options_ptr != 0 && options_len > 0 {
        read_guest_string(&memory, &caller, options_ptr, options_len)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    } else {
        None
    };

    // Sprawdz uprawnienia
    let has_llm_perm = check_permission(caller.data(), "llm", None);
    if !has_llm_perm {
        audit_log(caller.data(), "llm.generate", Some("llm"), model_name.as_deref(), "denied", None);
        return ABI_ERR_PERMISSION;
    }

    // Sprawdz uprawnienie do konkretnego modelu jesli podany
    if let Some(ref model) = model_name {
        if !check_permission(caller.data(), "llm_model", Some(model)) {
            audit_log(caller.data(), "llm.generate", Some("llm_model"), Some(model), "denied", None);
            return ABI_ERR_PERMISSION;
        }
    }

    let addon_id = caller.data().addon_id.clone();
    info!("llm_generate: addon='{}', model={:?}, prompt_len={}", addon_id, model_name, prompt.len());

    // Wywolaj LLM przez router — synchronicznie
    // W produkcji to przejdzie przez crate::routing::router
    // Na razie symulujemy odpowiedz (integracja z routerem w nastepnej fazie)
    let result_text = format!(
        "[LLM response placeholder — addon='{}', model={:?}, prompt_len={}]",
        addon_id,
        model_name.as_deref().unwrap_or("default"),
        prompt.len()
    );

    let result_bytes = result_text.as_bytes();

    // Loguj do audit
    audit_log(
        caller.data(),
        "llm.generate",
        Some("llm"),
        model_name.as_deref(),
        "ok",
        None,
    );

    // Zapisz wynik do pamieci guest
    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, result_bytes)
}

// =============================================================================
// llm_generate_stream_start — rozpoczecie strumieniowego generowania
// =============================================================================

/// Host function: rozpoczyna strumieniowe generowanie tekstu.
/// Rejestruje callback_id; Core wywola guest export `on_stream_chunk(callback_id, chunk_ptr, chunk_len)`.
///
/// ABI:
/// - prompt_ptr/prompt_len: prompt
/// - model_ptr/model_len: model (0,0 = domyslny)
/// - options_ptr/options_len: opcje JSON
/// - Zwraca: callback_id (>0) lub blad (<0)
pub fn llm_generate_stream_start(
    mut caller: WasmCaller<'_, AddonState>,
    prompt_ptr: i32,
    prompt_len: i32,
    model_ptr: i32,
    model_len: i32,
    _options_ptr: i32,
    _options_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj prompt
    let _prompt = match read_guest_string(&memory, &caller, prompt_ptr, prompt_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj model
    let model_name = if model_ptr != 0 && model_len > 0 {
        read_guest_string(&memory, &caller, model_ptr, model_len)
            .map(|s| s.to_string())
    } else {
        None
    };

    // Sprawdz uprawnienia
    if !check_permission(caller.data(), "llm", None) {
        audit_log(caller.data(), "llm.generate_stream", Some("llm"), model_name.as_deref(), "denied", None);
        return ABI_ERR_PERMISSION;
    }

    if let Some(ref model) = model_name {
        if !check_permission(caller.data(), "llm_model", Some(model)) {
            audit_log(caller.data(), "llm.generate_stream", Some("llm_model"), Some(model), "denied", None);
            return ABI_ERR_PERMISSION;
        }
    }

    let addon_id = caller.data().addon_id.clone();
    info!("llm_generate_stream_start: addon='{}', model={:?}", addon_id, model_name);

    // Generuj callback_id — prosty inkrementalny ID
    // W produkcji to bedzie zarzadzane przez StreamManager
    static CALLBACK_COUNTER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    let callback_id = CALLBACK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    audit_log(
        caller.data(),
        "llm.generate_stream",
        Some("llm"),
        model_name.as_deref(),
        "ok",
        None,
    );

    // Callback_id > 0 oznacza sukces
    callback_id
}

// =============================================================================
// llm_generate_stream_next — pobranie nastepnego fragmentu strumienia
// =============================================================================

/// Host function: pobiera nastepny fragment strumienia LLM.
///
/// ABI:
/// - callback_id: ID strumienia z llm_generate_stream_start
/// - out_ptr/out_cap: bufor na fragment
/// - out_len_ptr: ile bajtow zapisano (0 = koniec strumienia)
/// - Zwraca: ABI_OK lub kod bledu
pub fn llm_generate_stream_next(
    mut caller: WasmCaller<'_, AddonState>,
    callback_id: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    if callback_id <= 0 {
        return ABI_ERR_OPERATION;
    }

    // W produkcji: pobierz nastepny fragment z kolejki strumienia
    // Na razie zwracamy pusty fragment (koniec strumienia)
    let empty: &[u8] = &[];
    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, empty)
}
