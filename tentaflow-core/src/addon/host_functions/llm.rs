// =============================================================================
// Plik: addon/host_functions/llm.rs
// Opis: Host functions LLM API — generowanie tekstu (synchroniczne i strumieniowe).
//       Addon wywoluje te funkcje aby korzystac z modeli LLM dostepnych w Core.
// Uprawnienia: "llm" (wywolanie LLM), "llm_model" z resource=<model_name>
//              (per-model whitelist). Fail-closed — brak uprawnienia przerywa
//              operacje zanim trafi do backendu inferencji.
// =============================================================================

use tracing::{error, info, warn};

use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_ERR_RATE_LIMIT,
};

use crate::addon::rate_limiter::ResourceType;
use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};

/// MemGraphRAG D5 — twardy cap liczby par aliasow encji przepuszczanych z opcji wywolania do
/// flow.meta (`entity_aliases`). Alias-rewrite seedow PPR to retrieval-side ulatwienie; addon
/// nie moze wstrzyknac nieograniczonej listy do meta flow. Reszta degraduje do braku rewrite.
const ENTITY_ALIASES_META_CAP: usize = 256;

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
        read_guest_string(&memory, &caller, model_ptr, model_len).map(|s| s.to_string())
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
        audit_log(
            caller.data(),
            "llm.generate",
            Some("llm"),
            model_name.as_deref(),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();

    // F1a §6.6 alias gate. If the requested name resolves to an active
    // alias, enforce visibility + addon_uses_alias for the calling addon.
    // Non-alias names return Ok(None) → pass-through. Denial is audited
    // inside the resolver (alias_calls + audit_log risk_class=A).
    //
    // An alias name is authorized SOLELY by this gate (the addon declared
    // [[uses_alias]] + admin-approved visibility). The "llm_model" permission
    // applies only to raw (non-alias) model overrides, so it runs below only
    // when the name did not resolve to an alias.
    if let Some(ref model) = model_name {
        let db = caller.data().db.clone();
        let is_alias = match crate::db::repository::resolve_model_alias_for_addon(
            &db,
            model,
            Some(&addon_id),
            Some("llm.generate"),
            None,
        ) {
            Ok(resolved) => resolved.is_some(),
            Err(e) => {
                if e.downcast_ref::<crate::db::repository::AliasPermissionDenied>()
                    .is_some()
                {
                    audit_log(
                        caller.data(),
                        "llm.generate",
                        Some("alias"),
                        Some(model),
                        "denied",
                        Some("alias_permission_denied"),
                    );
                    return ABI_ERR_PERMISSION;
                }
                warn!("llm_generate: alias gate error for '{}': {}", model, e);
                return ABI_ERR_OPERATION;
            }
        };

        // Raw model override: gate on the per-model permission. Aliases skip
        // this — they passed the alias gate above.
        if !is_alias && !check_permission(caller.data(), "llm_model", Some(model)) {
            audit_log(
                caller.data(),
                "llm.generate",
                Some("llm_model"),
                Some(model),
                "denied",
                None,
            );
            return ABI_ERR_PERMISSION;
        }
    }

    info!(
        "llm_generate: addon='{}', model={:?}, prompt_len={}",
        addon_id,
        model_name,
        prompt.len()
    );

    // Sprawdz rate limit LLM przez in-memory rate limiter
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        if rate_limiter
            .check(&addon_id, ResourceType::LlmTokens)
            .is_err()
        {
            audit_log(
                caller.data(),
                "llm.generate",
                Some("llm"),
                model_name.as_deref(),
                "error",
                Some("rate limit exceeded"),
            );
            return ABI_ERR_RATE_LIMIT;
        }
    }

    // Pobierz router z AddonState
    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            warn!("llm_generate: router niedostepny dla addon='{}'", addon_id);
            audit_log(
                caller.data(),
                "llm.generate",
                Some("llm"),
                model_name.as_deref(),
                "error",
                Some("router unavailable"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // Parsuj opcje z JSON
    let temperature = _options_json
        .as_ref()
        .and_then(|o| o.get("temperature"))
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let max_tokens = _options_json
        .as_ref()
        .and_then(|o| o.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let top_p = _options_json
        .as_ref()
        .and_then(|o| o.get("top_p"))
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    // Zbuduj ChatCompletionRequest
    let request = ChatCompletionRequest {
        model: model_name.unwrap_or_else(|| "default".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: Some(MessageContent::Text(prompt)),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        temperature,
        max_tokens,
        top_p,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: false,
        stream_options: None,
        user: Some(format!("addon:{}", addon_id)),
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
    };

    // Most async→sync: host function jest synchroniczna, router jest async.
    // Uzywamy tokio::task::block_in_place aby uniknac deadlocka w wielowatkowym runtime.
    // RAG E2.0 — wąska allowlista opcji wywołania przepuszczana do flow.meta:
    // tylko `collection_id` (str) i `top_k` (dodatnia liczba całkowita). Reszta
    // opcji NIE jest przepuszczana, żeby addon nie wstrzyknął dowolnych pól w
    // meta flow. Węzeł `vector` flow czyta z tego filtr po kolekcji i top_k.
    let mut flow_meta: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    if let Some(opts) = _options_json.as_ref() {
        if let Some(cid) = opts.get("collection_id").and_then(|v| v.as_str()) {
            if !cid.is_empty() {
                flow_meta.insert(
                    "collection_id".to_string(),
                    serde_json::Value::String(cid.to_string()),
                );
            }
        }
        if let Some(k) = opts.get("top_k").and_then(|v| v.as_u64()).filter(|n| *n > 0) {
            flow_meta.insert("top_k".to_string(), serde_json::Value::from(k));
        }
        // Toggle opcjonalnego grafu: addon RAG wysyla `graph_enabled` (bool) przy ask,
        // by wezly grafowe flow (rag_graph_seed/rag_graph_facts) wiedzialy, czy fuzja
        // grafowa jest wlaczona. Bez przepuszczenia tej flagi do flow.meta query przy
        // OFF nadal fuzowalby istniejacy graf (wyciek). Tylko jawny bool przechodzi.
        if let Some(enabled) = opts.get("graph_enabled").and_then(|v| v.as_bool()) {
            flow_meta.insert(
                "graph_enabled".to_string(),
                serde_json::Value::Bool(enabled),
            );
        }
        // MemGraphRAG D5 — alias-rewrite seedow grafu (R5, TYLKO retrieval-side). Addon RAG
        // przekazuje aktywne aliasy encji `[{alias, canonical}]`; `rag_graph_seed` przepisuje
        // alias->canonical na seedach PPR. Twardy cap (ENTITY_ALIASES_META_CAP) chroni meta
        // przed wstrzyknieciem ogromnej listy. Tylko poprawne pary str->str przechodza.
        if let Some(arr) = opts.get("entity_aliases").and_then(|v| v.as_array()) {
            let pairs: Vec<serde_json::Value> = arr
                .iter()
                .filter(|e| {
                    e.get("alias").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
                        && e.get("canonical").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
                })
                .take(ENTITY_ALIASES_META_CAP)
                .map(|e| {
                    serde_json::json!({
                        "alias": e.get("alias").and_then(|v| v.as_str()).unwrap_or_default(),
                        "canonical": e.get("canonical").and_then(|v| v.as_str()).unwrap_or_default(),
                    })
                })
                .collect();
            if !pairs.is_empty() {
                flow_meta.insert("entity_aliases".to_string(), serde_json::Value::Array(pairs));
            }
        }
    }

    let compliance_context = crate::compliance::ai_gateway::AiGatewayContext {
        org_id: caller.data().org_id.clone(),
        addon_id: Some(addon_id.clone()),
        instance_id: Some(caller.data().instance_id.clone()),
        flow_id: None,
        flow_node_id: None,
        agent_id: None,
        agent_run_id: None,
        // Root context: the routing session event anchors the turn to its own
        // request_id (§3.4); per-call flow events copy it from there.
        correlation_id: None,
        flow_meta,
    };
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(router.route_chat_completion(
            request,
            None,
            Some(compliance_context),
        ))
    });

    let result_text = match result {
        Ok(route_result) => {
            // Wyciagnij tekst z pierwszego choice
            let response = route_result.response;
            response
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_ref())
                .map(|content| match content {
                    MessageContent::Text(text) => text.clone(),
                    MessageContent::Parts(parts) => {
                        // Sklej czesci tekstowe
                        parts
                            .iter()
                            .filter_map(|p| {
                                if let crate::api::openai::types::ContentPart::Text { text } = p {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    }
                })
                .unwrap_or_default()
        }
        Err(e) => {
            error!("llm_generate: blad routera dla addon='{}': {}", addon_id, e);
            audit_log(
                caller.data(),
                "llm.generate",
                Some("llm"),
                None,
                "error",
                Some(&e.to_string()),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // Zarejestruj zuzycie tokenow (przyblizone na podstawie dlugosci odpowiedzi)
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        let estimated_tokens = (result_text.len() / 4).max(1) as u64;
        rate_limiter.record_usage(&addon_id, ResourceType::LlmTokens, estimated_tokens);
    }

    let result_bytes = result_text.as_bytes();

    // Loguj do audit
    audit_log(caller.data(), "llm.generate", Some("llm"), None, "ok", None);

    // Zapisz wynik do pamieci guest
    write_guest_output(
        &memory,
        &mut caller,
        out_ptr,
        out_cap,
        out_len_ptr,
        result_bytes,
    )
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
        read_guest_string(&memory, &caller, model_ptr, model_len).map(|s| s.to_string())
    } else {
        None
    };

    // Sprawdz uprawnienia
    if !check_permission(caller.data(), "llm", None) {
        audit_log(
            caller.data(),
            "llm.generate_stream",
            Some("llm"),
            model_name.as_deref(),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    if let Some(ref model) = model_name {
        if !check_permission(caller.data(), "llm_model", Some(model)) {
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm_model"),
                Some(model),
                "denied",
                None,
            );
            return ABI_ERR_PERMISSION;
        }
    }

    let addon_id = caller.data().addon_id.clone();

    // F1a §6.6 alias gate — see llm_generate for rationale.
    if let Some(ref model) = model_name {
        let db = caller.data().db.clone();
        match crate::db::repository::resolve_model_alias_for_addon(
            &db,
            model,
            Some(&addon_id),
            Some("llm.generate_stream"),
            None,
        ) {
            Ok(_) => {}
            Err(e) => {
                if e.downcast_ref::<crate::db::repository::AliasPermissionDenied>()
                    .is_some()
                {
                    audit_log(
                        caller.data(),
                        "llm.generate_stream",
                        Some("alias"),
                        Some(model),
                        "denied",
                        Some("alias_permission_denied"),
                    );
                    return ABI_ERR_PERMISSION;
                }
                warn!(
                    "llm_generate_stream_start: alias gate error for '{}': {}",
                    model, e
                );
                return ABI_ERR_OPERATION;
            }
        }
    }

    info!(
        "llm_generate_stream_start: addon='{}', model={:?}",
        addon_id, model_name
    );

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

    // Fail-closed: kazdy krok strumienia wymaga ciagle uprawnienia "llm".
    // Cofniecie uprawnienia w trakcie strumienia natychmiast blokuje kolejne pobrania.
    if !check_permission(caller.data(), "llm", None) {
        audit_log(
            caller.data(),
            "llm.generate_stream_next",
            Some("llm"),
            Some(&callback_id.to_string()),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    // W produkcji: pobierz nastepny fragment z kolejki strumienia
    // Na razie zwracamy pusty fragment (koniec strumienia)
    let empty: &[u8] = &[];
    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::host_functions::check_permission;
    use crate::addon::host_functions::network::NetworkConnectionManager;
    use crate::addon::permissions::PermissionChecker;
    use crate::addon::AddonManifest;
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::Arc;

    fn make_state(permissions: Vec<String>) -> AddonState {
        let db = crate::db::init(Path::new(":memory:")).unwrap();
        AddonState {
            addon_id: "llm-test-addon".to_string(),
            instance_id: "t".to_string(),
            user_id: None,
            org_id: None,
            db: db.clone(),
            permissions,
            event_bus: Arc::new(EventBus::new()),
            permission_checker: Arc::new(PermissionChecker::new(db)),
            fuel_consumed: 0,
            is_system_call: true,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher: Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32])),
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    #[test]
    fn llm_generate_denied_without_permission() {
        // Addon bez "llm" — wszystkie 3 host functions (generate, stream_start, stream_next) odrzucaja.
        let state = make_state(vec!["storage".to_string()]);
        assert!(
            !check_permission(&state, "llm", None),
            "Brak 'llm' w permissions → Denied"
        );
    }

    #[test]
    fn llm_stream_next_denied_mid_stream_when_permission_missing() {
        // Nawet jesli stream_start przeszedl, kazdy stream_next rewalidauje.
        // Symulujemy addon ktory nigdy nie mial uprawnienia → stream_next odrzuca.
        let state = make_state(vec![]);
        assert!(!check_permission(&state, "llm", None),
            "stream_next bez 'llm' → Denied (ochrona przed cofnietym uprawnieniem w trakcie strumienia)");
    }
}
