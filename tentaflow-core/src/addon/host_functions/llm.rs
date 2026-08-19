// =============================================================================
// Plik: addon/host_functions/llm.rs
// Opis: Host functions LLM API — generowanie tekstu (synchroniczne i strumieniowe).
//       Addon wywoluje te funkcje aby korzystac z modeli LLM dostepnych w Core.
// Uprawnienia: "llm" (wywolanie LLM), "llm_model" z resource=<model_name>
//              (per-model whitelist). Fail-closed — brak uprawnienia przerywa
//              operacje zanim trafi do backendu inferencji.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use tentaflow_sdk_spec::{LlmStreamNextInput, LlmStreamNextOutput};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_ERR_RATE_LIMIT,
};

use crate::addon::errors::AbiError;
use crate::addon::rate_limiter::ResourceType;
use crate::api::openai::types::{
    ChatCompletionRequest, EmbeddingInput, EmbeddingRequest, Message, MessageContent,
};

/// MemGraphRAG D5 — twardy cap liczby par aliasow encji przepuszczanych z opcji wywolania do
/// flow.meta (`entity_aliases`). Alias-rewrite seedow PPR to retrieval-side ulatwienie; addon
/// nie moze wstrzyknac nieograniczonej listy do meta flow. Reszta degraduje do braku rewrite.
const ENTITY_ALIASES_META_CAP: usize = 256;

/// MemGraphRAG eq. 19 (Information Density) — twardy cap liczby wpisow rzadkosci encji
/// (`entity_density = [{id, density}]`) przepuszczanych z opcji wywolania do flow.meta.
/// Mapa to retrieval-side ulatwienie (skalowanie P_init relevance); addon nie moze
/// wstrzyknac nieograniczonej listy do meta flow. Lustro capu addon-side, ale GRANICA
/// bezpieczenstwa jest TU (options pochodza z addona). Reszta degraduje do density=0.
const ENTITY_DENSITY_META_CAP: usize = 512;

// =============================================================================
// Wspolna autoryzacja wywolan LLM (generate + stream_start)
// =============================================================================

/// Autoryzuje wywolanie LLM: uprawnienie "llm", bramka aliasow (F1a §6.6),
/// wlasne modele engine-flow addonu oraz per-model "llm_model" dla surowych
/// nadpisan. Fail-closed — kazda odmowa jest audytowana pod `action` i mapowana
/// na kod ABI. Wspoldzielone przez `llm_generate` i `llm_generate_stream_start`,
/// zeby semantyka autoryzacji nie rozjezdzala sie miedzy sciezkami.
fn authorize_llm_call(
    state: &AddonState,
    action: &str,
    model_name: Option<&str>,
) -> Result<(), i32> {
    if !check_permission(state, "llm", None) {
        audit_log(state, action, Some("llm"), model_name, "denied", None);
        return Err(ABI_ERR_PERMISSION);
    }

    let Some(model) = model_name else {
        return Ok(());
    };

    let addon_id = state.addon_id.clone();

    // F1a §6.6 alias gate. If the requested name resolves to an active
    // alias, enforce visibility + addon_uses_alias for the calling addon.
    // Non-alias names return Ok(None) → pass-through. Denial is audited
    // inside the resolver (alias_calls + audit_log risk_class=A).
    //
    // An alias name is authorized SOLELY by this gate (the addon declared
    // [[uses_alias]] + admin-approved visibility). The "llm_model" permission
    // applies only to raw (non-alias) model overrides, so it runs below only
    // when the name did not resolve to an alias.
    let is_alias = match crate::db::repository::resolve_model_alias_for_addon(
        &state.db,
        model,
        Some(&addon_id),
        Some(action),
        None,
    ) {
        Ok(resolved) => resolved.is_some(),
        Err(e) => {
            if e.downcast_ref::<crate::db::repository::AliasPermissionDenied>()
                .is_some()
            {
                audit_log(
                    state,
                    action,
                    Some("alias"),
                    Some(model),
                    "denied",
                    Some("alias_permission_denied"),
                );
                return Err(ABI_ERR_PERMISSION);
            }
            warn!("{action}: alias gate error for '{model}': {e}");
            return Err(ABI_ERR_OPERATION);
        }
    };

    // Model engine-flow nalezacy do wolajacego addonu (np. "rag-...:query")
    // jest jego wlasnym, opublikowanym flow zarejestrowanym w FlowDispatcher
    // jako "<addon_id>:<flow_id>" — nie jest surowym nadpisaniem modelu, wiec
    // nie wymaga permission "llm_model". Wewnetrzne node'y flow autoryzuja
    // swoje aliasy osobno w kontekscie wykonania flow.
    let owns_model = model.starts_with(&format!("{addon_id}:"));

    // Raw model override: gate on the per-model permission. Aliases skip
    // this — they passed the alias gate above.
    if !is_alias && !owns_model && !check_permission(state, "llm_model", Some(model)) {
        audit_log(
            state,
            action,
            Some("llm_model"),
            Some(model),
            "denied",
            None,
        );
        return Err(ABI_ERR_PERMISSION);
    }

    Ok(())
}

/// RAG E2.0 — wąska allowlista opcji wywołania przepuszczana do flow.meta:
/// `collection_id` (str), `top_k` (dodatnia liczba całkowita), `graph_enabled`
/// (bool), `entity_aliases` i `entity_density` (listy z capem). Reszta opcji NIE
/// jest przepuszczana, żeby addon nie wstrzyknął dowolnych pól w meta flow.
fn build_flow_meta(
    options: Option<&serde_json::Value>,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut flow_meta: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    let Some(opts) = options else {
        return flow_meta;
    };
    if let Some(cid) = opts.get("collection_id").and_then(|v| v.as_str()) {
        if !cid.is_empty() {
            flow_meta.insert(
                "collection_id".to_string(),
                serde_json::Value::String(cid.to_string()),
            );
        }
    }
    if let Some(k) = opts
        .get("top_k")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
    {
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
                e.get("alias")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
                    && e.get("canonical")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty())
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
            flow_meta.insert(
                "entity_aliases".to_string(),
                serde_json::Value::Array(pairs),
            );
        }
    }
    // MemGraphRAG eq. 19 — Information Density seedow grafu (retrieval-side). Addon RAG
    // przekazuje znormalizowane IDF encji `[{id, density∈[0,1]}]`; `rag_graph_facts` skaluje
    // nim P_init relevance. Twardy cap (ENTITY_DENSITY_META_CAP) + walidacja ksztaltu chronia
    // meta przed wstrzyknieciem ogromnej/zlej listy. Tylko poprawne `id` (str) + skonczone
    // `density` (liczba) przechodza; density jest clampowane do [0,1] (powtorne, defense-in-depth).
    if let Some(arr) = opts.get("entity_density").and_then(|v| v.as_array()) {
        let entries: Vec<serde_json::Value> = arr
            .iter()
            .filter_map(|e| {
                let id = e
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())?;
                let d = e
                    .get("density")
                    .and_then(|v| v.as_f64())
                    .filter(|x| x.is_finite())?;
                Some(serde_json::json!({ "id": id, "density": d.clamp(0.0, 1.0) }))
            })
            .take(ENTITY_DENSITY_META_CAP)
            .collect();
        if !entries.is_empty() {
            flow_meta.insert(
                "entity_density".to_string(),
                serde_json::Value::Array(entries),
            );
        }
    }
    flow_meta
}

/// Gorny limit dlugosci system prompta (`options.system`) w znakach. System prompt jest
/// kontrolowany przez addon; cap chroni backend inferencji przed nadmiernie dlugim wsadem.
const SYSTEM_PROMPT_MAX_CHARS: usize = 8192;

/// Buduje liste wiadomosci czatu dla wywolania LLM. Gdy `options.system` zawiera niepusty
/// (po przycieciu) string, poprzedza wiadomosc uzytkownika wiadomoscia roli `system`;
/// w przeciwnym razie zwraca sam prompt uzytkownika (dotychczasowe zachowanie). Wspoldzielone
/// przez `llm_generate` i `llm_generate_stream_start`, zeby konstrukcja messages sie nie rozjezdzala.
fn build_messages(options: Option<&serde_json::Value>, prompt: String) -> Vec<Message> {
    let mut messages = Vec::with_capacity(2);
    if let Some(system) = options
        .and_then(|o| o.get("system"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let system = if system.chars().count() > SYSTEM_PROMPT_MAX_CHARS {
            system.chars().take(SYSTEM_PROMPT_MAX_CHARS).collect()
        } else {
            system.to_string()
        };
        messages.push(Message {
            audio: None,
            role: "system".to_string(),
            content: Some(MessageContent::Text(system)),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.push(Message {
        audio: None,
        role: "user".to_string(),
        content: Some(MessageContent::Text(prompt)),
        reasoning_content: None,
        name: None,
        tool_calls: None,
        tool_call_id: None,
    });
    messages
}

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

    // Sprawdz uprawnienia + bramka aliasow + per-model "llm_model"
    if let Err(code) = authorize_llm_call(caller.data(), "llm.generate", model_name.as_deref()) {
        return code;
    }

    let addon_id = caller.data().addon_id.clone();

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

    // Rozgałęzienie po zadaniu: `task=="embedding"` idzie przez dedykowaną ścieżkę
    // embeddingów (FlowDispatcher / mesh-forward), a NIE przez chat completion.
    // Bez tego embedding-only model dostaje request czatu i zwraca pusty tekst.
    let task = _options_json
        .as_ref()
        .and_then(|o| o.get("task"))
        .and_then(|v| v.as_str());

    if task == Some("embedding") {
        let dimensions = _options_json
            .as_ref()
            .and_then(|o| o.get("dimensions"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        // Alias przekazujemy bez zmian — dispatcher embeddingów rozwiązuje aliasy
        // tak samo jak ścieżka czatu (`try_dispatch` po nazwie modelu).
        let request = EmbeddingRequest {
            model: model_name.clone().unwrap_or_else(|| "default".to_string()),
            input: EmbeddingInput::Single(prompt),
            encoding_format: None,
            dimensions,
            user: Some(format!("addon:{}", addon_id)),
            extra: serde_json::Map::new(),
        };

        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(router.route_embeddings_for_user(request, None))
        });

        let route_result = match result {
            Ok(rr) => rr,
            Err(e) => {
                error!(
                    "llm_generate: blad routera (embeddings) dla addon='{}': {}",
                    addon_id, e
                );
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

        let response = route_result.response;
        let result_text = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                error!(
                    "llm_generate: serializacja embeddingu dla addon='{}': {}",
                    addon_id, e
                );
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

        if let Some(ref rate_limiter) = caller.data().rate_limiter {
            let estimated_tokens = (result_text.len() / 4).max(1) as u64;
            rate_limiter.record_usage(&addon_id, ResourceType::LlmTokens, estimated_tokens);
        }

        audit_log(caller.data(), "llm.generate", Some("llm"), None, "ok", None);

        return write_guest_output(
            &memory,
            &mut caller,
            out_ptr,
            out_cap,
            out_len_ptr,
            result_text.as_bytes(),
        );
    }

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
        reasoning_effort: None,
        modalities: None,
        audio: None,
        model: model_name.unwrap_or_else(|| "default".to_string()),
        messages: build_messages(_options_json.as_ref(), prompt),
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
        extra: Default::default(),
    };

    // Most async→sync: host function jest synchroniczna, router jest async.
    // Uzywamy tokio::task::block_in_place aby uniknac deadlocka w wielowatkowym runtime.
    let flow_meta = build_flow_meta(_options_json.as_ref());

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
// StreamManager — rejestr aktywnych strumieni LLM per addon
// =============================================================================

/// Zdarzenie strumienia LLM przekazywane z pump-taska do `stream_next`.
#[derive(Debug)]
pub(crate) enum LlmStreamEvent {
    Chunk(String),
    Done { finish_reason: Option<String> },
    Error(String),
}

/// Slot aktywnego strumienia. Drop slotu (cancel / reap / unload) zamyka
/// receiver — pump-task widzi Closed przy `send` i porzuca strumien backendu,
/// co anuluje generacje (CancelOnDropStream w warstwie routingu).
struct LlmStreamSlot {
    /// `stream_next` jest sync po stronie hosta, ale mutuje receiver — rownolegle
    /// wywolania na tym samym callback_id musza sie serializowac.
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<LlmStreamEvent>>>,
    abort: tokio::task::AbortHandle,
    /// Ostatni `stream_next` — podstawa reapowania bezczynnych strumieni.
    last_activity: Arc<parking_lot::Mutex<Instant>>,
    /// Ustawiane przez `stream_next` na czas draina (moze trwac do
    /// `LLM_STREAM_MAX_WAIT_MS`). Reaper NIE usuwa slotow `in_use`, wiec aktywnie
    /// pollowany strumien nie zostanie ubity mimo starego `last_activity`.
    in_use: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for LlmStreamSlot {
    fn drop(&mut self) {
        // Kazda sciezka usuniecia slotu (cancel, reap, unload, finish) musi
        // ubic pump-task; abort na zakonczonym tasku jest no-opem.
        self.abort.abort();
    }
}

/// Panic-safe RAII guard flagi `in_use`. Ustawiony po `in_use=true`, w Drop
/// zawsze przywraca `in_use=false` — niezaleznie czy `stream_next` wroci Ok/Err,
/// czy panika w drainie odwinie stos. Bez tego wisząca flaga na stale wykluczyla
/// slot z reapera (wyciek).
struct InUseGuard {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl InUseGuard {
    fn new(flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self { flag }
    }
}

impl Drop for InUseGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Limit rownoleglych strumieni LLM per addon.
const MAX_LLM_STREAMS_PER_ADDON: usize = 4;
/// Strumien bez `stream_next` przez ten czas jest anulowany i usuwany.
const LLM_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Gorny limit pojedynczego oczekiwania w `stream_next`.
const LLM_STREAM_MAX_WAIT_MS: u64 = 30_000;
/// Pojemnosc kanalu pump→next. Pelny kanal wstrzymuje pompe (backpressure),
/// nie gubi tokenow.
const LLM_STREAM_CHANNEL_CAP: usize = 256;
/// Interwal background-sweepera reapujacego porzucone strumienie.
const LLM_STREAM_SWEEP_INTERVAL: Duration = Duration::from_secs(15);

type LlmStreamKey = (String, i32);

/// Caly rejestr pod JEDNYM lockiem — czyni sekcje krytyczna reap+count+insert
/// atomowa (P1-2: rownolegle `stream_start` nie moga przekroczyc kwoty). Lock
/// jest trzymany WYLACZNIE dla szybkich operacji na mapie; drain strumienia
/// (blocking await) dzieje sie PO sklonowaniu `Arc`ow i zwolnieniu locka.
static LLM_STREAMS: OnceLock<parking_lot::Mutex<HashMap<LlmStreamKey, LlmStreamSlot>>> =
    OnceLock::new();
static LLM_CALLBACK_COUNTER: AtomicI32 = AtomicI32::new(1);
/// Gwarantuje pojedyncze uruchomienie background-sweepera.
static LLM_SWEEPER_STARTED: std::sync::Once = std::sync::Once::new();

fn llm_streams() -> &'static parking_lot::Mutex<HashMap<LlmStreamKey, LlmStreamSlot>> {
    LLM_STREAMS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn next_callback_id() -> i32 {
    // Callback_id musi byc > 0 (0/ujemne to kody bledow ABI). Po przekreceniu
    // licznika wracamy do 1 — kolizja wymagalaby ~2^31 zywych slotow.
    loop {
        let id = LLM_CALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
        if id > 0 {
            return id;
        }
        LLM_CALLBACK_COUNTER.store(1, Ordering::Relaxed);
    }
}

/// Usuwa strumienie bez aktywnosci przez `LLM_STREAM_IDLE_TIMEOUT`. Slot
/// `in_use` (aktywnie pollowany) jest POMIJANY (P2-1) — jego `last_activity`
/// moze byc stary w trakcie dlugiego draina, ale to nie znaczy porzucenie.
/// Wolane pod trzymanym lockiem rejestru.
fn reap_idle_locked(streams: &mut HashMap<LlmStreamKey, LlmStreamSlot>) {
    let now = Instant::now();
    streams.retain(|_, slot| {
        slot.in_use.load(Ordering::Acquire)
            || now.duration_since(*slot.last_activity.lock()) < LLM_STREAM_IDLE_TIMEOUT
    });
}

/// Background-sweeper: periodycznie reapuje porzucone strumienie (P1-3). Guest
/// Drop nie jest gwarantowany przy wycieku, wiec host MUSI mieć wlasny sweeper —
/// inaczej porzucony `LlmStream` zostawia slot + pump-task + strumien backendu
/// zywy w nieskonczonosc (pelny mpsc parkuje pompe na zawsze). Uruchamiany raz.
fn ensure_sweeper_started() {
    LLM_SWEEPER_STARTED.call_once(|| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async {
                let mut ticker = tokio::time::interval(LLM_STREAM_SWEEP_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let mut streams = llm_streams().lock();
                    reap_idle_locked(&mut streams);
                }
            });
        }
    });
}

/// Sprzata wszystkie strumienie addonu — wolane przy stop/unload instancji.
pub fn cleanup_addon_streams(addon_id: &str) {
    llm_streams().lock().retain(|(aid, _), _| aid != addon_id);
}

/// Rejestruje nowy strumien; egzekwuje kwote per-addon ATOMOWO (P1-2). Reap +
/// count + insert dzieja sie pod jednym lockiem, wiec rownolegle `stream_start`
/// nie moga wszystkie zobaczyc <limit i wstawic >limit.
fn register_stream(
    addon_id: &str,
    receiver: mpsc::Receiver<LlmStreamEvent>,
    abort: tokio::task::AbortHandle,
) -> Result<i32, AbiError> {
    ensure_sweeper_started();
    let mut streams = llm_streams().lock();
    reap_idle_locked(&mut streams);
    let live = streams.keys().filter(|(aid, _)| aid == addon_id).count();
    if live >= MAX_LLM_STREAMS_PER_ADDON {
        return Err(AbiError::QuotaExceeded);
    }
    let id = next_callback_id();
    streams.insert(
        (addon_id.to_string(), id),
        LlmStreamSlot {
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
            abort,
            last_activity: Arc::new(parking_lot::Mutex::new(Instant::now())),
            in_use: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    Ok(id)
}

/// Batch-drain kolejki strumienia: czeka (max `timeout`) na PIERWSZE zdarzenie,
/// potem zbiera wszystko co juz czeka bez dalszego czekania. Zwraca gotowy
/// `LlmStreamNextOutput`; `finished == true` oznacza ze caller ma usunac slot.
async fn drain_stream_batch(
    rx: &mut mpsc::Receiver<LlmStreamEvent>,
    timeout: Duration,
) -> LlmStreamNextOutput {
    let mut out = LlmStreamNextOutput {
        chunks: Vec::new(),
        finished: false,
        finish_reason: None,
        error: None,
    };

    // Kanal zamkniety bez Done/Error = pump-task zostal abortowany w locie.
    let mark_aborted = |out: &mut LlmStreamNextOutput| {
        out.finished = true;
        out.finish_reason = Some("error".to_string());
        out.error = Some("stream aborted".to_string());
    };

    let mut apply = |out: &mut LlmStreamNextOutput, ev: LlmStreamEvent| -> bool {
        match ev {
            LlmStreamEvent::Chunk(text) => {
                out.chunks.push(text);
                false
            }
            LlmStreamEvent::Done { finish_reason } => {
                out.finished = true;
                out.finish_reason = finish_reason.or_else(|| Some("stop".to_string()));
                true
            }
            LlmStreamEvent::Error(e) => {
                out.finished = true;
                out.finish_reason = Some("error".to_string());
                out.error = Some(e);
                true
            }
        }
    };

    // Pierwsze zdarzenie: blokujace z timeoutem (timeout 0 = pure drain).
    if timeout.is_zero() {
        match rx.try_recv() {
            Ok(ev) => {
                if apply(&mut out, ev) {
                    return out;
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => return out,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                mark_aborted(&mut out);
                return out;
            }
        }
    } else {
        match tokio::time::timeout(timeout, rx.recv()).await {
            Err(_) => return out, // timeout — pusta partia, addon polluje dalej
            Ok(None) => {
                mark_aborted(&mut out);
                return out;
            }
            Ok(Some(ev)) => {
                if apply(&mut out, ev) {
                    return out;
                }
            }
        }
    }

    // Reszta partii: wszystko co juz czeka w kolejce, bez czekania.
    loop {
        match rx.try_recv() {
            Ok(ev) => {
                if apply(&mut out, ev) {
                    return out;
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => return out,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                mark_aborted(&mut out);
                return out;
            }
        }
    }
}

// =============================================================================
// llm_generate_stream_start — rozpoczecie strumieniowego generowania
// =============================================================================

/// Host function: rozpoczyna strumieniowe generowanie tekstu.
/// Uruchamia pump-task konsumujacy streaming router'a do kolejki per-strumien;
/// addon odbiera partie fragmentow przez `llm_generate_stream_next`.
///
/// ABI:
/// - prompt_ptr/prompt_len: prompt
/// - model_ptr/model_len: model (0,0 = domyslny)
/// - options_ptr/options_len: opcje JSON {temperature, max_tokens, top_p, ...}
/// - Zwraca: callback_id (>0) lub blad (<0 stary kod ABI, >0 nigdy nie koliduje
///   bo QuotaExceeded zwracamy jako ujemna wartosc AbiError)
pub fn llm_generate_stream_start(
    mut caller: WasmCaller<'_, AddonState>,
    prompt_ptr: i32,
    prompt_len: i32,
    model_ptr: i32,
    model_len: i32,
    options_ptr: i32,
    options_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj prompt
    let prompt = match read_guest_string(&memory, &caller, prompt_ptr, prompt_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj model
    let model_name = if model_ptr != 0 && model_len > 0 {
        read_guest_string(&memory, &caller, model_ptr, model_len).map(|s| s.to_string())
    } else {
        None
    };

    // Odczytaj opcje jako JSON
    let options_json = if options_ptr != 0 && options_len > 0 {
        read_guest_string(&memory, &caller, options_ptr, options_len)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    } else {
        None
    };

    // Sprawdz uprawnienia + bramka aliasow + per-model "llm_model" — ta sama
    // semantyka co llm_generate (wspolny helper).
    if let Err(code) =
        authorize_llm_call(caller.data(), "llm.generate_stream", model_name.as_deref())
    {
        return code;
    }

    let addon_id = caller.data().addon_id.clone();

    // Rate limit tokenow LLM — jak w llm_generate; zuzycie realne rejestruje
    // stream_next per partia.
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        if rate_limiter
            .check(&addon_id, ResourceType::LlmTokens)
            .is_err()
        {
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm"),
                model_name.as_deref(),
                "error",
                Some("rate limit exceeded"),
            );
            return ABI_ERR_RATE_LIMIT;
        }
    }

    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            warn!(
                "llm_generate_stream_start: router niedostepny dla addon='{}'",
                addon_id
            );
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm"),
                model_name.as_deref(),
                "error",
                Some("router unavailable"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // Pump-task musi zyc na runtime tokio — host function jest wolana z watku
    // workerow tokio, wiec Handle::try_current() jest dostepny.
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm"),
                model_name.as_deref(),
                "error",
                Some("no tokio runtime"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    info!(
        "llm_generate_stream_start: addon='{}', model={:?}, prompt_len={}",
        addon_id,
        model_name,
        prompt.len()
    );

    let temperature = options_json
        .as_ref()
        .and_then(|o| o.get("temperature"))
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let max_tokens = options_json
        .as_ref()
        .and_then(|o| o.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let top_p = options_json
        .as_ref()
        .and_then(|o| o.get("top_p"))
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    let request = ChatCompletionRequest {
        reasoning_effort: None,
        modalities: None,
        audio: None,
        model: model_name.clone().unwrap_or_else(|| "default".to_string()),
        messages: build_messages(options_json.as_ref(), prompt),
        temperature,
        max_tokens,
        top_p,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: true,
        stream_options: None,
        user: Some(format!("addon:{}", addon_id)),
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
        extra: Default::default(),
    };

    // Compliance: zdarzenie AI startuje w route_chat_completion_stream i jest
    // domykane przez ComplianceAuditStream po skonsumowaniu / porzuceniu
    // strumienia — identycznie jak dla klienta SSE.
    let compliance_context = crate::compliance::ai_gateway::AiGatewayContext {
        org_id: caller.data().org_id.clone(),
        addon_id: Some(addon_id.clone()),
        instance_id: Some(caller.data().instance_id.clone()),
        flow_id: None,
        flow_node_id: None,
        agent_id: None,
        agent_run_id: None,
        correlation_id: None,
        flow_meta: build_flow_meta(options_json.as_ref()),
    };

    let (tx, rx) = mpsc::channel::<LlmStreamEvent>(LLM_STREAM_CHANNEL_CAP);
    let pump_addon = addon_id.clone();
    let task = handle.spawn(async move {
        let result = router
            .route_chat_completion_stream(
                request,
                None,
                Some(compliance_context),
                crate::routing::streaming::ChatFlowSelector::Auto,
            )
            .await;
        match result {
            Ok(route_result) => {
                let mut stream = route_result.response;
                let mut finish_reason: Option<String> = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => {
                            if let Some(choice) = chunk.choices.first() {
                                if let Some(fr) = &choice.finish_reason {
                                    finish_reason = Some(fr.clone());
                                }
                                if let Some(text) = &choice.delta.content {
                                    if !text.is_empty()
                                        && tx
                                            .send(LlmStreamEvent::Chunk(text.clone()))
                                            .await
                                            .is_err()
                                    {
                                        // Konsument porzucil strumien (cancel /
                                        // reap / unload) — drop strumienia
                                        // backendu anuluje generacje.
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(
                                "llm stream pump: blad strumienia dla addon='{}': {}",
                                pump_addon, e
                            );
                            let _ = tx.send(LlmStreamEvent::Error(e.to_string())).await;
                            return;
                        }
                    }
                }
                let _ = tx.send(LlmStreamEvent::Done { finish_reason }).await;
            }
            Err(e) => {
                error!(
                    "llm stream pump: blad routera dla addon='{}': {}",
                    pump_addon, e
                );
                let _ = tx.send(LlmStreamEvent::Error(e.to_string())).await;
            }
        }
    });

    match register_stream(&addon_id, rx, task.abort_handle()) {
        Ok(callback_id) => {
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm"),
                model_name.as_deref(),
                "ok",
                Some(&format!("callback_id={callback_id}")),
            );
            callback_id
        }
        Err(e) => {
            task.abort();
            audit_log(
                caller.data(),
                "llm.generate_stream",
                Some("llm"),
                model_name.as_deref(),
                "denied",
                Some("streams_quota_per_addon"),
            );
            // Ujemna wartosc — nie koliduje z callback_id (>0).
            -e.as_i32()
        }
    }
}

// =============================================================================
// llm_generate_stream_next — pobranie kolejnej partii fragmentow strumienia
// =============================================================================

/// Host function: pobiera kolejna PARTIE fragmentow strumienia LLM.
///
/// ABI (CBOR):
/// - input: `LlmStreamNextInput { callback_id, timeout_ms }` — timeout dotyczy
///   pierwszego fragmentu partii i jest clampowany do 30 s
/// - output: `LlmStreamNextOutput { chunks, finished, finish_reason?, error? }`
/// - Zwraca: AbiError (0 = OK); po `finished == true` callback_id jest niewazny
pub fn llm_generate_stream_next(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let input: LlmStreamNextInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => return e.as_i32(),
    };

    if input.callback_id <= 0 {
        return AbiError::Operation.as_i32();
    }

    // Fail-closed: kazdy krok strumienia rewaliduje uprawnienie "llm".
    // Cofniecie uprawnienia w trakcie strumienia natychmiast blokuje pobrania.
    if !check_permission(caller.data(), "llm", None) {
        audit_log(
            caller.data(),
            "llm.generate_stream_next",
            Some("llm"),
            Some(&input.callback_id.to_string()),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id.clone(), input.callback_id);
    // Pod lockiem: sklonuj Arc'y, oznacz `in_use` i odswiez `last_activity`
    // ATOMOWO wzgledem reapera (P2-1). Reaper widzi `in_use=true` i nie usunie
    // slotu w trakcie draina, a swiezy `last_activity` chroni po zwolnieniu
    // flagi. Lock puszczamy PRZED blokujacym drainem.
    let (rx_arc, in_use_guard) = {
        let streams = llm_streams().lock();
        match streams.get(&key) {
            Some(slot) => {
                slot.in_use.store(true, Ordering::Release);
                *slot.last_activity.lock() = Instant::now();
                // Guard RAII: KAZDA sciezka wyjscia (Ok/Err/timeout/finished oraz
                // panika w drainie) czysci `in_use` w Drop. Bez tego panika miedzy
                // set(true) a set(false) zostawilaby slot na stale `in_use=true`,
                // wiec sweeper pomijalby go w nieskonczonosc (wyciek — dokladnie
                // czemu mial zapobiegac P1-3).
                (slot.receiver.clone(), InUseGuard::new(slot.in_use.clone()))
            }
            None => return AbiError::StreamNotFound.as_i32(),
        }
    };

    let timeout = Duration::from_millis(input.timeout_ms.min(LLM_STREAM_MAX_WAIT_MS));
    let out = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let mut rx = rx_arc.lock().await;
            drain_stream_batch(&mut rx, timeout).await
        })
    });

    // Po drainie: odswiez activity, zeby reaper mierzyl bezczynnosc od KONCA tego
    // wywolania, nie od poczatku. `in_use` zdejmie guard w Drop (panic-safe).
    {
        let streams = llm_streams().lock();
        if let Some(slot) = streams.get(&key) {
            *slot.last_activity.lock() = Instant::now();
        }
    }
    drop(in_use_guard);

    // Zuzycie tokenow rejestrujemy per partia (przyblizenie jak w llm_generate).
    if !out.chunks.is_empty() {
        if let Some(ref rate_limiter) = caller.data().rate_limiter {
            let bytes: usize = out.chunks.iter().map(|c| c.len()).sum();
            let estimated_tokens = (bytes / 4).max(1) as u64;
            rate_limiter.record_usage(&addon_id, ResourceType::LlmTokens, estimated_tokens);
        }
    }

    if out.finished {
        llm_streams().lock().remove(&key);
        audit_log(
            caller.data(),
            "llm.generate_stream_next",
            Some("llm"),
            Some(&input.callback_id.to_string()),
            if out.error.is_some() { "error" } else { "ok" },
            out.error.as_deref(),
        );
    }

    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// =============================================================================
// llm_generate_stream_cancel — jawne zamkniecie strumienia
// =============================================================================

/// Host function: anuluje strumien LLM i zwalnia zasoby (slot + pump-task).
///
/// ABI:
/// - callback_id: ID strumienia z llm_generate_stream_start
/// - Zwraca: AbiError (0 = OK, StreamNotFound gdy slot nie istnieje)
pub fn llm_generate_stream_cancel(caller: WasmCaller<'_, AddonState>, callback_id: i32) -> i32 {
    if callback_id <= 0 {
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    if llm_streams()
        .lock()
        .remove(&(addon_id, callback_id))
        .is_some()
    {
        audit_log(
            caller.data(),
            "llm.generate_stream_cancel",
            Some("llm"),
            Some(&callback_id.to_string()),
            "ok",
            None,
        );
        AbiError::Ok.as_i32()
    } else {
        AbiError::StreamNotFound.as_i32()
    }
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
    fn build_messages_without_system_keeps_user_only() {
        // Brak `options.system` → dotychczasowe zachowanie: pojedyncza wiadomosc uzytkownika.
        let msgs = build_messages(None, "hej".to_string());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert!(matches!(&msgs[0].content, Some(MessageContent::Text(t)) if t == "hej"));

        // Pusty / bialy system tez degraduje do samego usera.
        let opts = serde_json::json!({ "system": "   " });
        let msgs = build_messages(Some(&opts), "hej".to_string());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn build_messages_with_system_prepends_system_message() {
        // Niepusty `options.system` → [system, user] w tej kolejnosci, system przyciety.
        let opts = serde_json::json!({ "system": "  You are terse.  " });
        let msgs = build_messages(Some(&opts), "hej".to_string());
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert!(matches!(&msgs[0].content, Some(MessageContent::Text(t)) if t == "You are terse."));
        assert_eq!(msgs[1].role, "user");
        assert!(matches!(&msgs[1].content, Some(MessageContent::Text(t)) if t == "hej"));
    }

    #[test]
    fn build_messages_caps_overlong_system_prompt() {
        // System dluzszy niz cap jest obcinany do SYSTEM_PROMPT_MAX_CHARS znakow.
        let long = "x".repeat(SYSTEM_PROMPT_MAX_CHARS + 100);
        let opts = serde_json::json!({ "system": long });
        let msgs = build_messages(Some(&opts), "hej".to_string());
        assert_eq!(msgs.len(), 2);
        match &msgs[0].content {
            Some(MessageContent::Text(t)) => assert_eq!(t.chars().count(), SYSTEM_PROMPT_MAX_CHARS),
            other => panic!("oczekiwano tekstowego system prompta, mam {other:?}"),
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

    // =========================================================================
    // StreamManager — testy rejestru strumieni LLM
    // =========================================================================

    fn spawn_idle_task() -> (mpsc::Receiver<LlmStreamEvent>, tokio::task::AbortHandle) {
        let (_tx, rx) = mpsc::channel::<LlmStreamEvent>(4);
        // Task-pustak trzymajacy _tx przy zyciu nie jest potrzebny — testy
        // rejestru operuja na slotach, nie na pompie.
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        (rx, task.abort_handle())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_manager_enforces_per_addon_quota() {
        let addon = format!("quota-{}", uuid::Uuid::new_v4());
        let mut ids = Vec::new();
        for _ in 0..MAX_LLM_STREAMS_PER_ADDON {
            let (rx, abort) = spawn_idle_task();
            ids.push(register_stream(&addon, rx, abort).expect("register within quota"));
        }
        let (rx, abort) = spawn_idle_task();
        assert_eq!(
            register_stream(&addon, rx, abort).unwrap_err(),
            AbiError::QuotaExceeded,
            "5. strumien per addon musi byc odrzucony"
        );
        // Callback_id sa unikalne i dodatnie.
        assert!(ids.iter().all(|id| *id > 0));
        cleanup_addon_streams(&addon);
        assert_eq!(
            llm_streams()
                .lock()
                .keys()
                .filter(|(aid, _)| *aid == addon)
                .count(),
            0,
            "cleanup_addon_streams musi usunac wszystkie sloty addonu"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_manager_reaps_idle_streams() {
        let addon = format!("reap-{}", uuid::Uuid::new_v4());
        let (rx, abort) = spawn_idle_task();
        let id = register_stream(&addon, rx, abort).expect("register");
        // Cofnij last_activity poza prog bezczynnosci i wymus reap.
        {
            let streams = llm_streams().lock();
            let slot = streams.get(&(addon.clone(), id)).expect("slot exists");
            *slot.last_activity.lock() = Instant::now() - LLM_STREAM_IDLE_TIMEOUT;
        }
        {
            let mut streams = llm_streams().lock();
            reap_idle_locked(&mut streams);
        }
        assert!(
            llm_streams().lock().get(&(addon.clone(), id)).is_none(),
            "bezczynny strumien (60s bez next) musi zostac usuniety"
        );
    }

    #[test]
    fn in_use_guard_clears_flag_on_drop_and_panic() {
        use std::sync::atomic::AtomicBool;
        // Normalna sciezka: drop guarda czysci flage.
        let flag = Arc::new(AtomicBool::new(false));
        flag.store(true, Ordering::Release);
        {
            let _g = InUseGuard::new(flag.clone());
            assert!(
                flag.load(Ordering::Acquire),
                "flaga trzymana w trakcie zycia guarda"
            );
        }
        assert!(
            !flag.load(Ordering::Acquire),
            "drop guarda musi wyczyscic in_use"
        );

        // Sciezka paniki: rozwijanie stosu przez guard i tak czysci flage.
        let flag2 = Arc::new(AtomicBool::new(false));
        flag2.store(true, Ordering::Release);
        let flag2_probe = flag2.clone();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = InUseGuard::new(flag2);
            panic!("symulowana panika w drainie");
        }));
        assert!(res.is_err(), "panika musi sie zmaterializowac");
        assert!(
            !flag2_probe.load(Ordering::Acquire),
            "panika miedzy set(true) a set(false) NIE moze zostawic in_use=true (Drop czysci)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_manager_reap_skips_in_use_streams() {
        // P2-1: slot oznaczony `in_use` NIE moze byc zreapowany mimo starego
        // last_activity — inaczej aktywnie pollowany strumien (drain do 30s)
        // zostalby ubity spod nog.
        let addon = format!("inuse-{}", uuid::Uuid::new_v4());
        let (rx, abort) = spawn_idle_task();
        let id = register_stream(&addon, rx, abort).expect("register");
        {
            let streams = llm_streams().lock();
            let slot = streams.get(&(addon.clone(), id)).expect("slot exists");
            *slot.last_activity.lock() = Instant::now() - LLM_STREAM_IDLE_TIMEOUT;
            slot.in_use.store(true, Ordering::Release);
        }
        {
            let mut streams = llm_streams().lock();
            reap_idle_locked(&mut streams);
        }
        assert!(
            llm_streams().lock().get(&(addon.clone(), id)).is_some(),
            "strumien in_use nie moze byc zreapowany mimo starego last_activity"
        );
        cleanup_addon_streams(&addon);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_batch_collects_all_queued_chunks() {
        let (tx, mut rx) = mpsc::channel::<LlmStreamEvent>(16);
        tx.send(LlmStreamEvent::Chunk("Hel".into())).await.unwrap();
        tx.send(LlmStreamEvent::Chunk("lo ".into())).await.unwrap();
        tx.send(LlmStreamEvent::Chunk("world".into()))
            .await
            .unwrap();
        let out = drain_stream_batch(&mut rx, Duration::from_millis(500)).await;
        assert_eq!(
            out.chunks,
            vec!["Hel", "lo ", "world"],
            "batch = cala kolejka, nie 1 token"
        );
        assert!(!out.finished);
        assert!(out.error.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_batch_reports_done_with_finish_reason() {
        let (tx, mut rx) = mpsc::channel::<LlmStreamEvent>(16);
        tx.send(LlmStreamEvent::Chunk("koniec".into()))
            .await
            .unwrap();
        tx.send(LlmStreamEvent::Done {
            finish_reason: Some("length".into()),
        })
        .await
        .unwrap();
        let out = drain_stream_batch(&mut rx, Duration::from_millis(500)).await;
        assert_eq!(out.chunks, vec!["koniec"]);
        assert!(out.finished);
        assert_eq!(out.finish_reason.as_deref(), Some("length"));
        assert!(out.error.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_batch_times_out_with_empty_batch() {
        let (_tx, mut rx) = mpsc::channel::<LlmStreamEvent>(4);
        let start = Instant::now();
        let out = drain_stream_batch(&mut rx, Duration::from_millis(50)).await;
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(out.chunks.is_empty());
        assert!(
            !out.finished,
            "timeout to NIE koniec strumienia — addon polluje dalej"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_batch_error_event_finishes_stream() {
        let (tx, mut rx) = mpsc::channel::<LlmStreamEvent>(4);
        tx.send(LlmStreamEvent::Error("backend down".into()))
            .await
            .unwrap();
        let out = drain_stream_batch(&mut rx, Duration::from_millis(500)).await;
        assert!(out.finished);
        assert_eq!(out.finish_reason.as_deref(), Some("error"));
        assert_eq!(out.error.as_deref(), Some("backend down"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_batch_closed_channel_marks_aborted() {
        let (tx, mut rx) = mpsc::channel::<LlmStreamEvent>(4);
        drop(tx);
        let out = drain_stream_batch(&mut rx, Duration::from_millis(500)).await;
        assert!(out.finished);
        assert_eq!(out.finish_reason.as_deref(), Some("error"));
        assert!(
            out.error.is_some(),
            "abort pompy bez Done/Error musi byc widoczny dla addonu"
        );
    }
}
