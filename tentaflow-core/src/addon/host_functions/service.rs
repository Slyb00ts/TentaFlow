// =============================================================================
// Plik: addon/host_functions/service.rs
// Opis: Host function Service API — wysylanie requestow do zarejestrowanych
//       serwisow (kontenerow Docker) przez infrastrukture QUIC routera.
//       Addon podaje nazwe serwisu i JSON payload, Core routuje przez QUIC.
// Uprawnienia: "service" (globalny) oraz "service" z resource=<service_name>
//              (per-service whitelist). Obydwa musza byc przyznane — fail-closed.
// =============================================================================

use tracing::{error, info, warn};

use super::{
    audit_log, audit_log_with_risk, check_permission, get_memory, read_guest_string,
    write_guest_output, AddonState, WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION,
    ABI_ERR_PERMISSION, ABI_ERR_RATE_LIMIT,
};

use crate::addon::errors::AbiError;
use crate::addon::rate_limiter::ResourceType;
use crate::services::service_call_rate_limit::{
    note_denial_for_audit, service_call_rate_limiter, AuditEmitDecision, RateLimitResult,
    AUDIT_DENY_WINDOW,
};

// =============================================================================
// service_request — wyslanie requestu do nazwanego serwisu przez QUIC
// =============================================================================

/// Host function: wysyla request do zarejestrowanego serwisu (kontenera Docker).
///
/// ABI:
/// - service_name_ptr/service_name_len: nazwa serwisu (UTF-8)
/// - request_json_ptr/request_json_len: JSON payload do wyslania
/// - out_ptr/out_cap: bufor na odpowiedz JSON
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn service_request(
    mut caller: WasmCaller<'_, AddonState>,
    service_name_ptr: i32,
    service_name_len: i32,
    request_json_ptr: i32,
    request_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Odczytaj nazwe serwisu z pamieci WASM
    let service_name = match read_guest_string(&memory, &caller, service_name_ptr, service_name_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik service_name");
            return ABI_ERR_OPERATION;
        }
    };

    // Odczytaj request JSON z pamieci WASM
    let request_json = match read_guest_string(&memory, &caller, request_json_ptr, request_json_len)
    {
        Some(s) => s.to_string(),
        None => {
            warn!("service_request: niepoprawny wskaznik request_json");
            return ABI_ERR_OPERATION;
        }
    };

    // Sprawdz uprawnienie "service"
    if !check_permission(caller.data(), "service", None) {
        audit_log(
            caller.data(),
            "service.request",
            Some("service"),
            Some(&service_name),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    // Sprawdz uprawnienie do konkretnego serwisu
    if !check_permission(caller.data(), "service", Some(&service_name)) {
        audit_log(
            caller.data(),
            "service.request",
            Some("service"),
            Some(&service_name),
            "denied",
            Some(&format!("serwis '{}' niedozwolony", service_name)),
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();

    // F1b §5 (P5) per-addon rate limit for `service_call_v1`. Default budget:
    // 100 burst + 1000 req/min sustain. Denials emit a collapsed `audit_log`
    // row (at most one per 60 s per addon, carrying the in-window count) and
    // return `AbiError::QuotaExceeded` so the WASM caller can react.
    match service_call_rate_limiter().check(&addon_id) {
        RateLimitResult::Allow => {}
        RateLimitResult::AddonLimit { retry_after_secs, .. } => {
            if let AuditEmitDecision::Emit { denied_count } = note_denial_for_audit(&addon_id) {
                let details = serde_json::json!({
                    "reason": "rate_limit_exceeded",
                    "retry_after_secs": retry_after_secs.ceil().max(1.0) as u64,
                    "denied_count": denied_count,
                    "window_secs": AUDIT_DENY_WINDOW.as_secs(),
                    "service_name": service_name,
                })
                .to_string();
                audit_log_with_risk(
                    caller.data(),
                    "service.request",
                    Some("service"),
                    Some(&service_name),
                    crate::audit::RiskClass::C,
                    None,
                    None,
                    "denied",
                    Some(&details),
                );
            }
            return AbiError::QuotaExceeded.into();
        }
    }

    // F1a §6.6 alias gate. `service_name` may be an alias resolving to a
    // backend service — apply the same visibility / addon_uses_alias check
    // before dispatch. Non-alias service names return Ok(None) → pass.
    {
        let db = caller.data().db.clone();
        match crate::db::repository::resolve_model_alias_for_addon(
            &db,
            &service_name,
            Some(&addon_id),
            Some("service.request"),
            None,
        ) {
            Ok(_) => {}
            Err(e) => {
                if e.downcast_ref::<crate::db::repository::AliasPermissionDenied>().is_some() {
                    audit_log(
                        caller.data(),
                        "service.request",
                        Some("alias"),
                        Some(&service_name),
                        "denied",
                        Some("alias_permission_denied"),
                    );
                    return ABI_ERR_PERMISSION;
                }
                warn!(
                    "service_request: alias gate error for '{}': {}",
                    service_name, e
                );
                return ABI_ERR_OPERATION;
            }
        }
    }
    info!(
        "service_request: addon='{}', service='{}', payload_len={}",
        addon_id,
        service_name,
        request_json.len()
    );

    // Rate limit + router availability are checked BEFORE minting the pickup
    // token so a denied call does not leave an orphan entry in the inflight
    // map. Token mint comes only when we are committed to dispatching.
    if let Some(ref rate_limiter) = caller.data().rate_limiter {
        if rate_limiter
            .check(&addon_id, ResourceType::HttpRequests)
            .is_err()
        {
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("rate limit exceeded"),
            );
            return ABI_ERR_RATE_LIMIT;
        }
        rate_limiter.record_usage(&addon_id, ResourceType::HttpRequests, 1);
    }

    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            warn!(
                "service_request: router niedostepny dla addon='{}'",
                addon_id
            );
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("router unavailable"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // M1.W7 — mint PickupToken only after rate-limit + router are green. If
    // the dispatch fails downstream we revoke the token explicitly so the
    // inflight map cannot grow without bound under partial failures.
    let request_id = uuid::Uuid::new_v4().to_string();
    let (effective_payload, frame_ref_for_audit, minted_token_wire) =
        match maybe_inject_pickup_token(&request_json, &service_name, &request_id) {
            Ok((payload, fref, wire)) => (payload, fref, wire),
            Err(reason) => {
                warn!(
                    "service_request: pickup token injection failed for '{}': {}",
                    service_name, reason
                );
                audit_log(
                    caller.data(),
                    "service.request",
                    Some("service"),
                    Some(&service_name),
                    "error",
                    Some(reason),
                );
                return ABI_ERR_OPERATION;
            }
        };

    // Znajdz QUIC client dla serwisu — szukamy po kolei w roznych typach
    let service_manager = router.service_manager();

    // Bounded dispatch timeout — any service that legitimately needs >30 s is
    // a bug, and an unbounded wait would leave the alias_calls table without
    // any record of the call until the hang resolves (audit chain gap).
    const DISPATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let dispatch_started = std::time::Instant::now();
    let dispatch_outcome = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            tokio::time::timeout(
                DISPATCH_TIMEOUT,
                dispatch_to_service(service_manager, &service_name, &effective_payload, &addon_id),
            )
            .await
        })
    });
    let duration_ms = dispatch_started.elapsed().as_millis() as i64;
    let result = match dispatch_outcome {
        Ok(inner) => inner,
        Err(_) => {
            warn!(
                "service_request: dispatch timeout (>{}s) for service='{}'",
                DISPATCH_TIMEOUT.as_secs(),
                service_name
            );
            // Revoke minted pickup token so it cannot be replayed against a
            // service that never received its envelope.
            if let Some(ref wire) = minted_token_wire {
                crate::services::pickup_token_issuer().revoke(wire);
            }
            log_alias_call(
                caller.data(),
                &service_name,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                0,
                frame_ref_for_audit.as_deref(),
                "timeout",
                Some("dispatch_timeout"),
            );
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("dispatch_timeout"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    match result {
        Ok(response_json) => {
            let response_bytes = response_json.as_bytes();

            log_alias_call(
                caller.data(),
                &service_name,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                response_bytes.len() as i64,
                frame_ref_for_audit.as_deref(),
                "ok",
                None,
            );

            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "ok",
                None,
            );

            write_guest_output(
                &memory,
                &mut caller,
                out_ptr,
                out_cap,
                out_len_ptr,
                response_bytes,
            )
        }
        Err(err_code) => {
            // Dispatch failed before the receiving service could consume the
            // pickup token — revoke it so it cannot leak past this call.
            if let Some(ref wire) = minted_token_wire {
                crate::services::pickup_token_issuer().revoke(wire);
            }
            let err_msg = match err_code {
                ABI_ERR_NOT_FOUND => format!("serwis '{}' nie znaleziony", service_name),
                _ => format!("blad wysylania do serwisu '{}'", service_name),
            };
            error!("service_request: addon='{}': {}", addon_id, err_msg);
            let result_label = if err_code == ABI_ERR_NOT_FOUND {
                "no_target"
            } else {
                "error"
            };
            log_alias_call(
                caller.data(),
                &service_name,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                0,
                frame_ref_for_audit.as_deref(),
                result_label,
                Some(&err_code.to_string()),
            );
            audit_log(
                caller.data(),
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some(&err_msg),
            );
            err_code
        }
    }
}

/// If `payload` is a JSON object containing a `frame_ref` field, mints a
/// `PickupToken` for `(frame_ref, service_id, request_id)` and rewrites the
/// payload to embed `pickup_token` next to it. Non-object payloads (raw
/// strings, arrays) and objects without `frame_ref` are returned verbatim.
/// Returns `(rewritten_payload_json, frame_ref_for_audit, minted_wire_token)`
/// where the wire token is `Some` only when a token was actually minted —
/// callers use it to revoke the inflight entry on dispatch failure.
fn maybe_inject_pickup_token(
    payload_json: &str,
    service_id: &str,
    request_id: &str,
) -> Result<(String, Option<String>, Option<String>), &'static str> {
    let parsed: serde_json::Value = match serde_json::from_str(payload_json) {
        Ok(v) => v,
        Err(_) => return Ok((payload_json.to_string(), None, None)),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return Ok((payload_json.to_string(), None, None)),
    };
    let frame_ref = match obj.get("frame_ref").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Ok((payload_json.to_string(), None, None)),
    };

    let issuer = crate::services::pickup_token_issuer();
    let (token, _) = issuer.issue(frame_ref.clone(), service_id.to_string(), request_id.to_string());
    let wire = token.wire();
    let mut new_obj = obj.clone();
    new_obj.insert(
        "pickup_token".to_string(),
        serde_json::Value::String(wire.clone()),
    );
    new_obj.insert(
        "request_id".to_string(),
        serde_json::Value::String(request_id.to_string()),
    );
    new_obj.insert(
        "service_id".to_string(),
        serde_json::Value::String(service_id.to_string()),
    );
    serde_json::to_string(&serde_json::Value::Object(new_obj))
        .map(|s| (s, Some(frame_ref), Some(wire)))
        .map_err(|_| "rewrite_serialize_failed")
}

/// Best-effort `alias_calls` row. The resolver in `repository.rs` already
/// writes the `permission_denied` path; here we cover the dispatch outcomes
/// (`ok`, `error`, `no_target`). Service names that are not registered
/// aliases simply skip the insert (FK to `model_aliases.id` would fail).
#[allow(clippy::too_many_arguments)]
fn log_alias_call(
    state: &AddonState,
    service_name: &str,
    request_id: &str,
    duration_ms: i64,
    payload_bytes: i64,
    response_bytes: i64,
    frame_ref: Option<&str>,
    result: &str,
    error_code: Option<&str>,
) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    // Look up alias_id — if `service_name` is not a known alias we silently
    // skip (FK constraint would fail). Pure service calls without an alias
    // remain logged through `audit_log` above.
    let alias_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM model_aliases WHERE alias = ?1",
            rusqlite::params![service_name],
            |row| row.get::<_, i64>(0),
        )
        .ok();
    let Some(alias_id) = alias_id else {
        return;
    };
    let _ = frame_ref; // reserved for richer logging when schema gets a column
    let _ = conn.execute(
        "INSERT INTO alias_calls \
             (alias_id, alias_name, method, target_used, target_node_id, service_id, \
              caller_addon_id, caller_user_id, request_id, duration_ms, payload_bytes, \
              response_bytes, fallback_used, fallback_chain_position, result, error_code, ts) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, NULL, ?12, ?13, ?14)",
        rusqlite::params![
            alias_id,
            service_name,
            "service.request",
            service_name,
            service_name,
            state.addon_id,
            state.user_id,
            request_id,
            duration_ms,
            payload_bytes,
            response_bytes,
            result,
            error_code,
            ts,
        ],
    );
}

/// Wysyla request do serwisu przez QUIC — probuje znalezc klienta
/// w mapach LLM, Embedding, TTS, STT i Memory.
async fn dispatch_to_service(
    service_manager: &crate::services::runtime::quic_handle::ServiceManager,
    service_name: &str,
    request_json: &str,
    addon_id: &str,
) -> std::result::Result<String, i32> {
    use tentaflow_protocol::*;

    // Szukaj QUIC client w kolejnosci: LLM, Embedding, TTS, STT
    let quic_client = service_manager
        .get_quic_llm_client(service_name)
        .await
        .or(service_manager
            .get_quic_embedding_client(service_name)
            .await)
        .or(service_manager.get_quic_tts_client(service_name).await)
        .or(service_manager.get_quic_stt_client(service_name).await);

    let quic_client = match quic_client {
        Some(c) => c,
        None => {
            warn!(
                "service_request: brak QUIC klienta dla serwisu '{}'",
                service_name
            );
            return Err(ABI_ERR_NOT_FOUND);
        }
    };

    // Zbuduj ModelRequest z JSON payload w polu prompt CompletionPayload
    let request_id = uuid::Uuid::new_v4().to_string();
    let model_request = ModelRequest {
        request_id,
        payload: ModelPayload::Completion(CompletionPayload {
            model: service_name.to_string(),
            prompt: Some(request_json.to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: request_json.to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        }),
        stream: false,
        metadata: Some(vec![("addon_id".to_string(), addon_id.to_string())]),
        session_id: None,
    };

    let model_response = quic_client.send_request(model_request).await.map_err(|e| {
        error!("service_request: blad QUIC dla '{}': {}", service_name, e);
        ABI_ERR_OPERATION
    })?;

    // Serializuj cala odpowiedz jako JSON
    let response = match model_response.result {
        ModelResult::Completion(ref completion) => {
            serde_json::json!({
                "status": "ok",
                "request_id": model_response.request_id,
                "text": completion.text,
                "model": completion.model,
                "finish_reason": completion.finish_reason,
            })
        }
        ModelResult::Error(ref err) => {
            serde_json::json!({
                "status": "error",
                "request_id": model_response.request_id,
                "error": err.message,
            })
        }
        _ => {
            // Inne typy odpowiedzi — zwroc surowy opis
            serde_json::json!({
                "status": "ok",
                "request_id": model_response.request_id,
                "result_type": format!("{:?}", std::mem::discriminant(&model_response.result)),
            })
        }
    };

    serde_json::to_string(&response).map_err(|_| ABI_ERR_OPERATION)
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

    /// Tworzy in-memory DB z pelnym schematem
    fn create_test_db() -> crate::db::DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    /// Tworzy minimalny AddonState do testowania check_permission
    fn create_test_addon_state(
        addon_id: &str,
        permissions: Vec<String>,
        user_id: Option<i64>,
        is_system_call: bool,
    ) -> AddonState {
        let db = create_test_db();
        let event_bus = Arc::new(EventBus::new());
        let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
        let settings_cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));

        AddonState {
            addon_id: addon_id.to_string(),
            instance_id: "test-instance".to_string(),
            user_id,
            db,
            permissions,
            event_bus,
            permission_checker,
            fuel_consumed: 0,
            is_system_call,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher,
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
    fn check_permission_no_service_permission_returns_false() {
        // Addon bez uprawnienia "service" — check_permission zwraca false

        // Arrange
        let state = create_test_addon_state(
            "test-addon",
            vec!["llm".to_string(), "storage".to_string()],
            None,
            true,
        );

        // Act & Assert
        assert!(
            !check_permission(&state, "service", None),
            "Addon bez 'service' w permissions powinien byc odrzucony"
        );
    }

    #[test]
    fn check_permission_with_service_permission_system_call_returns_true() {
        // Addon z uprawnieniem "service" i is_system_call=true — check_permission zwraca true

        // Arrange
        let state = create_test_addon_state(
            "teams-bot",
            vec!["service".to_string(), "llm".to_string()],
            None,
            true,
        );

        // Act & Assert
        assert!(
            check_permission(&state, "service", None),
            "Addon z 'service' i is_system_call=true powinien miec uprawnienie"
        );
    }

    #[test]
    fn check_permission_without_system_call_no_user_returns_false() {
        // Addon z uprawnieniem "service" ale bez user_id i is_system_call=false

        // Arrange
        let state =
            create_test_addon_state("untrusted-addon", vec!["service".to_string()], None, false);

        // Act & Assert
        assert!(
            !check_permission(&state, "service", None),
            "Addon bez user_id i is_system_call=false powinien byc odrzucony"
        );
    }

    #[test]
    fn check_permission_service_with_resource_name_no_permission_returns_false() {
        // Addon bez "service" w permissions — nawet z nazwa serwisu nie przejdzie

        // Arrange
        let state = create_test_addon_state("test-addon", vec!["llm".to_string()], None, true);

        // Act & Assert
        assert!(
            !check_permission(&state, "service", Some("teams-stt")),
            "Addon bez 'service' nie powinien miec dostepu do zadnego serwisu"
        );
    }

    #[test]
    fn check_permission_empty_permissions_returns_false() {
        // Addon bez zadnych uprawnien

        // Arrange
        let state = create_test_addon_state("empty-addon", vec![], None, true);

        // Act & Assert
        assert!(!check_permission(&state, "service", None));
        assert!(!check_permission(&state, "llm", None));
        assert!(!check_permission(&state, "storage", None));
    }
}
