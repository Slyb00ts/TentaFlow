// =============================================================================
// File: services/service_call.rs — pure-async service dispatch (no WASM)
// =============================================================================
//
// Extracts the QUIC dispatch path from `addon/host_functions/service.rs`
// into a thin, WASM-free API the flow_runtime operators (Predict) call
// directly without having to fabricate an `AddonState`/`WasmCaller`. The
// WASM host wrapper now just reads guest memory, checks permission, and
// delegates here. Production semantics (rate limit, alias gate, pickup
// token injection, alias_calls audit row, dispatch timeout) are preserved
// — they belong to the dispatch, not the WASM ABI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{error, warn};

use crate::addon::permissions::PermissionChecker;
use crate::audit::RiskClass;
use crate::db::DbPool;
use crate::services::runtime::quic_handle::ServiceManager;
use crate::services::service_call_rate_limit::{
    note_denial_for_audit, service_call_rate_limiter, AuditEmitDecision, RateLimitResult,
    AUDIT_DENY_WINDOW,
};

/// Authoritative caller identity for a service call. Carries enough context
/// to satisfy the permission/audit invariants of the WASM ABI even when the
/// caller is not a WASM addon (e.g. a flow_runtime operator).
#[derive(Debug, Clone)]
pub struct CallerContext {
    pub addon_id: String,
    pub user_id: Option<String>,
    pub instance_id: Option<String>,
    /// `true` skips the per-user permission resolve. Flow operators run
    /// system-side and inherit the addon's manifest permission set.
    pub is_system_call: bool,
    /// F2 P1.b — owning organization of the call. `None` means "use the
    /// default tenant" and is recorded as `org-default` in audit rows.
    pub org_id: Option<String>,
}

/// Request shape for `dispatch`. `payload_json` is forwarded verbatim to the
/// backend service after `pickup_token` injection.
#[derive(Debug, Clone)]
pub struct ServiceCallRequest {
    pub caller: CallerContext,
    pub service_name: String,
    pub payload_json: String,
    /// 0 = use the default `DISPATCH_TIMEOUT`. The WASM ABI does not surface
    /// a timeout knob today; operators may dial it down in C1.
    pub timeout_ms: u32,
    /// When `true`, dispatch requires `service_name` to resolve to a row in
    /// `model_aliases`. The alias gate currently allows `Ok(None)` (treat as
    /// concrete service name) — flow operators that mint calls explicitly
    /// through an alias should set this so a revoked alias cannot fall
    /// through to a same-named live service.
    pub alias_required: bool,
}

#[derive(Debug, Clone)]
pub struct ServiceCallResponse {
    pub response_json: String,
    pub duration_ms: i64,
    pub frame_ref: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Error)]
pub enum ServiceCallError {
    #[error("permission denied for service '{service}'")]
    Permission { service: String },
    #[error("alias permission denied for '{service}'")]
    AliasPermission { service: String },
    #[error("rate limit exceeded (retry_after={retry_after_secs}s)")]
    RateLimit { retry_after_secs: u64 },
    #[error("service '{service}' not found")]
    NotFound { service: String },
    #[error("service_manager not initialized")]
    ServiceManagerNotInitialized,
    #[error("dispatch timeout (>{timeout_secs}s) for service '{service}'")]
    Timeout { service: String, timeout_secs: u64 },
    #[error("pickup token injection failed: {0}")]
    PickupTokenInjection(&'static str),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Bounded dispatch timeout — any service that legitimately needs >30 s is
/// a bug, and an unbounded wait would leave the alias_calls table without
/// any record of the call until the hang resolves (audit chain gap).
pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Pure-async service dispatch. Performs the same checks as the WASM host
/// function in this order: permission ("service" + per-resource), addon
/// rate limit, alias gate, pickup token injection, QUIC dispatch with the
/// per-call timeout. Returns a structured error so callers (WASM wrapper,
/// flow operator) can map to their own surface.
///
/// `permission_checker` may be `None` only when `caller.is_system_call` is
/// `true` AND the addon was already vetted by a higher layer; this matches
/// the AddonState semantics (system calls without user_id skip the resolve).
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    req: ServiceCallRequest,
    db: &DbPool,
    service_manager: Option<&Arc<ServiceManager>>,
    executor: Option<&Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>,
    permission_checker: Option<&PermissionChecker>,
    permissions: &[String],
) -> Result<ServiceCallResponse, ServiceCallError> {
    let addon_id = req.caller.addon_id.clone();
    let service_name = req.service_name.clone();

    // ---- permission check ("service" global + per-service resource) ----
    if !permission_granted(
        permissions,
        &req.caller,
        permission_checker,
        "service",
        None,
    ) || !permission_granted(
        permissions,
        &req.caller,
        permission_checker,
        "service",
        Some(&service_name),
    ) {
        emit_audit(
            db,
            &req.caller,
            "service.request",
            Some("service"),
            Some(&service_name),
            "denied",
            None,
        );
        return Err(ServiceCallError::Permission {
            service: service_name,
        });
    }

    // ---- per-addon rate limit (F1b §5) ----
    match service_call_rate_limiter().check(&addon_id) {
        RateLimitResult::Allow => {}
        RateLimitResult::AddonLimit {
            retry_after_secs, ..
        } => {
            if let AuditEmitDecision::Emit { denied_count } = note_denial_for_audit(&addon_id) {
                let details = serde_json::json!({
                    "reason": "rate_limit_exceeded",
                    "retry_after_secs": retry_after_secs.ceil().max(1.0) as u64,
                    "denied_count": denied_count,
                    "window_secs": AUDIT_DENY_WINDOW.as_secs(),
                    "service_name": service_name,
                })
                .to_string();
                emit_audit_full(
                    db,
                    &req.caller,
                    "service.request",
                    Some("service"),
                    Some(&service_name),
                    RiskClass::C,
                    "denied",
                    Some(&details),
                );
            }
            return Err(ServiceCallError::RateLimit {
                retry_after_secs: retry_after_secs.ceil().max(1.0) as u64,
            });
        }
    }

    // ---- alias gate (F1a §6.6) ----
    let alias_row = {
        match crate::db::repository::resolve_model_alias_for_addon(
            db,
            &service_name,
            Some(&addon_id),
            Some("service.request"),
            None,
        ) {
            Ok(None) if req.alias_required => {
                emit_audit(
                    db,
                    &req.caller,
                    "service.request",
                    Some("alias"),
                    Some(&service_name),
                    "denied",
                    Some("alias_required_not_found"),
                );
                return Err(ServiceCallError::NotFound {
                    service: service_name,
                });
            }
            Ok(row) => row,
            Err(e) => {
                if e.downcast_ref::<crate::db::repository::AliasPermissionDenied>()
                    .is_some()
                {
                    emit_audit(
                        db,
                        &req.caller,
                        "service.request",
                        Some("alias"),
                        Some(&service_name),
                        "denied",
                        Some("alias_permission_denied"),
                    );
                    return Err(ServiceCallError::AliasPermission {
                        service: service_name,
                    });
                }
                warn!(
                    "service_call: alias gate error for '{}': {}",
                    service_name, e
                );
                return Err(ServiceCallError::Internal(format!("alias_gate: {e}")));
            }
        }
    };

    // ---- alias failover (A1 §0.4) ----
    // Gdy `service_name` jest aktywnym aliasem ORAZ mamy `ModelRuntimeExecutor`,
    // dispatch idzie przez TĘ SAMĄ ścieżkę co `/v1` i flow: `AliasResolver`
    // rozwiązuje alias na `ResolvedExecutionTarget` (embedded/local/remote-node),
    // a pętla failoveru w executorze próbuje kandydatów [target_model] +
    // fallbacks w kolejności, schodząc na pierwszy DOSTĘPNY — w tym do modelu
    // EMBEDDED na telefonie (brak endpoint_url nie jest niedostępnością) i z
    // mesh-forwardem gdy właściciel modelu to inny węzeł. Bez executora (boot/
    // test DB-less) degradujemy do legacy dispatchu po nazwie aliasu poniżej.
    if let (Some(_), Some(executor)) = (&alias_row, executor) {
        let started = Instant::now();
        // RAG E1.0 — zasiej tożsamość addona-callera (instance_id) i org z
        // `req.caller`. `service_call::dispatch` to ścieżka addon-as-model
        // (host-fn `service_request` + operatory flow_runtime), więc `addon_id`
        // jest tu zawsze instancją addona. Executor przepisze to do
        // `FlowRequestMeta` dla `ResolvedExecutionTarget::Flow`.
        let mut exec_ctx = crate::services::runtime::context::ExecutionContext::new(None)
            .with_addon_identity(Some(req.caller.addon_id.clone()), req.caller.org_id.clone());
        let routed =
            route_alias_via_executor(executor, &service_name, &req.payload_json, &mut exec_ctx)
                .await;
        let duration_ms = started.elapsed().as_millis() as i64;
        let request_id = uuid::Uuid::new_v4().to_string();

        return match routed {
            Ok(routed) => {
                log_alias_call(
                    db,
                    &req.caller,
                    &service_name,
                    Some(&AliasCallRoute {
                        target_used: resolved_target_used(
                            exec_ctx.route_metadata.served_model.as_deref(),
                            &routed.target_model,
                        ),
                        target_node_id: exec_ctx.route_metadata.served_by_node.as_deref(),
                        chain_position: exec_ctx.route_metadata.fallbacks_tried as i64,
                        fallback_used: exec_ctx.route_metadata.fallbacks_tried > 0,
                    }),
                    &request_id,
                    duration_ms,
                    req.payload_json.len() as i64,
                    routed.response_json.len() as i64,
                    None,
                    "ok",
                    None,
                );
                emit_audit(
                    db,
                    &req.caller,
                    "service.request",
                    Some("alias"),
                    Some(&service_name),
                    "ok",
                    None,
                );
                Ok(ServiceCallResponse {
                    response_json: routed.response_json,
                    duration_ms,
                    frame_ref: None,
                    request_id,
                })
            }
            Err(AliasRouteError::NoTarget(msg)) => {
                log_alias_call(
                    db,
                    &req.caller,
                    &service_name,
                    None,
                    &request_id,
                    duration_ms,
                    req.payload_json.len() as i64,
                    0,
                    None,
                    "no_target",
                    Some(&msg),
                );
                emit_audit(
                    db,
                    &req.caller,
                    "service.request",
                    Some("alias"),
                    Some(&service_name),
                    "error",
                    Some("alias_no_target_available"),
                );
                Err(ServiceCallError::NotFound {
                    service: service_name,
                })
            }
            Err(AliasRouteError::Dispatch(msg)) => {
                error!(
                    "service_call: alias '{}' dispatch error: {}",
                    service_name, msg
                );
                log_alias_call(
                    db,
                    &req.caller,
                    &service_name,
                    None,
                    &request_id,
                    duration_ms,
                    req.payload_json.len() as i64,
                    0,
                    None,
                    "error",
                    Some(&msg),
                );
                emit_audit(
                    db,
                    &req.caller,
                    "service.request",
                    Some("alias"),
                    Some(&service_name),
                    "error",
                    Some(&msg),
                );
                Err(ServiceCallError::Internal(msg))
            }
        };
    }

    let service_manager = match service_manager {
        Some(sm) => sm.clone(),
        None => {
            emit_audit(
                db,
                &req.caller,
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("service_manager_not_initialized"),
            );
            return Err(ServiceCallError::ServiceManagerNotInitialized);
        }
    };

    // ---- pickup token mint (best-effort: only when payload carries frame_ref) ----
    let request_id = uuid::Uuid::new_v4().to_string();
    let (effective_payload, frame_ref_for_audit, minted_token_wire) =
        maybe_inject_pickup_token(&req.payload_json, &service_name, &request_id)
            .map_err(ServiceCallError::PickupTokenInjection)?;

    // ---- dispatch with bounded timeout ----
    let timeout = if req.timeout_ms == 0 {
        DISPATCH_TIMEOUT
    } else {
        Duration::from_millis(req.timeout_ms as u64)
    };
    let started = Instant::now();
    let dispatch_outcome = tokio::time::timeout(
        timeout,
        dispatch_to_service(
            &service_manager,
            &service_name,
            &effective_payload,
            &addon_id,
        ),
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as i64;

    let result = match dispatch_outcome {
        Ok(inner) => inner,
        Err(_) => {
            if let Some(ref wire) = minted_token_wire {
                crate::services::pickup_token_issuer().revoke(wire);
            }
            log_alias_call(
                db,
                &req.caller,
                &service_name,
                None,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                0,
                frame_ref_for_audit.as_deref(),
                "timeout",
                Some("dispatch_timeout"),
            );
            emit_audit(
                db,
                &req.caller,
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("dispatch_timeout"),
            );
            return Err(ServiceCallError::Timeout {
                service: service_name,
                timeout_secs: timeout.as_secs(),
            });
        }
    };

    match result {
        Ok(response_json) => {
            log_alias_call(
                db,
                &req.caller,
                &service_name,
                None,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                response_json.len() as i64,
                frame_ref_for_audit.as_deref(),
                "ok",
                None,
            );
            emit_audit(
                db,
                &req.caller,
                "service.request",
                Some("service"),
                Some(&service_name),
                "ok",
                None,
            );
            Ok(ServiceCallResponse {
                response_json,
                duration_ms,
                frame_ref: frame_ref_for_audit,
                request_id,
            })
        }
        Err(DispatchErr::NotFound) => {
            if let Some(ref wire) = minted_token_wire {
                crate::services::pickup_token_issuer().revoke(wire);
            }
            log_alias_call(
                db,
                &req.caller,
                &service_name,
                None,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                0,
                frame_ref_for_audit.as_deref(),
                "no_target",
                Some("not_found"),
            );
            emit_audit(
                db,
                &req.caller,
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some("not_found"),
            );
            Err(ServiceCallError::NotFound {
                service: service_name,
            })
        }
        Err(DispatchErr::Other(msg)) => {
            if let Some(ref wire) = minted_token_wire {
                crate::services::pickup_token_issuer().revoke(wire);
            }
            error!(
                "service_call: dispatch error for '{}': {}",
                service_name, msg
            );
            log_alias_call(
                db,
                &req.caller,
                &service_name,
                None,
                &request_id,
                duration_ms,
                effective_payload.len() as i64,
                0,
                frame_ref_for_audit.as_deref(),
                "error",
                Some(&msg),
            );
            emit_audit(
                db,
                &req.caller,
                "service.request",
                Some("service"),
                Some(&service_name),
                "error",
                Some(&msg),
            );
            Err(ServiceCallError::Internal(msg))
        }
    }
}

fn permission_granted(
    permissions: &[String],
    caller: &CallerContext,
    checker: Option<&PermissionChecker>,
    permission_type: &str,
    resource: Option<&str>,
) -> bool {
    if !permissions.iter().any(|p| p == permission_type) {
        return false;
    }
    let user_id = match caller.user_id.as_deref() {
        Some(id) => id,
        None => return caller.is_system_call,
    };
    let checker = match checker {
        Some(c) => c,
        // No checker available but addon declared the permission AND user_id
        // is set — treat as not-granted (conservative). System calls without
        // user_id already returned `true` above.
        None => return false,
    };
    checker
        .check(&caller.addon_id, user_id, permission_type, resource)
        .is_granted()
}

/// If `payload` is a JSON object containing a `frame_ref` field, mints a
/// `PickupToken` for `(frame_ref, service_id, request_id)` and rewrites the
/// payload to embed `pickup_token` next to it. Non-object payloads (raw
/// strings, arrays) and objects without `frame_ref` are returned verbatim.
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
    let (token, _) = issuer.issue(
        frame_ref.clone(),
        service_id.to_string(),
        request_id.to_string(),
    );
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

enum DispatchErr {
    NotFound,
    Other(String),
}

async fn dispatch_to_service(
    service_manager: &ServiceManager,
    service_name: &str,
    request_json: &str,
    addon_id: &str,
) -> Result<String, DispatchErr> {
    use tentaflow_protocol::*;

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
            // No QUIC sidecar — direct-http engines (the standard since the
            // sidecar removal). Reach the service over HTTP via its BackendClient.
            if let Some(http) = service_manager.find_http_backend_for_model(service_name) {
                return dispatch_http(&http, service_name, request_json).await;
            }
            return Err(DispatchErr::NotFound);
        }
    };

    let request_id = uuid::Uuid::new_v4().to_string();
    let model_request = ModelRequest {
        request_id,
        payload: ModelPayload::Completion(CompletionPayload {
            model: service_name.to_string(),
            prompt: Some(request_json.to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: request_json.to_string(),
                reasoning_content: None,
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

    let model_response = quic_client
        .send_request(model_request)
        .await
        .map_err(|e| DispatchErr::Other(format!("quic: {e}")))?;

    let response = match model_response.result {
        ModelResult::Completion(ref completion) => serde_json::json!({
            "status": "ok",
            "request_id": model_response.request_id,
            "text": completion.text,
            "model": completion.model,
            "finish_reason": completion.finish_reason,
        }),
        ModelResult::Error(ref err) => serde_json::json!({
            "status": "error",
            "request_id": model_response.request_id,
            "error": err.message,
        }),
        _ => serde_json::json!({
            "status": "ok",
            "request_id": model_response.request_id,
            "result_type": format!("{:?}", std::mem::discriminant(&model_response.result)),
        }),
    };
    serde_json::to_string(&response).map_err(|e| DispatchErr::Other(format!("serialize: {e}")))
}

/// Direct-http dispatch (no sidecar): wraps the addon's `request_json` as a chat
/// user message and calls the engine's OpenAI `/chat/completions` via the live
/// `BackendClient`, mirroring the QUIC path's Completion semantics. Returns the
/// same `{status, text, model, finish_reason}` shape.
async fn dispatch_http(
    client: &crate::services::backend::client::BackendClient,
    service_name: &str,
    request_json: &str,
) -> Result<String, DispatchErr> {
    use crate::api::openai::types::{ChatCompletionRequest, MessageContent};

    let req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": service_name,
        "messages": [{"role": "user", "content": request_json}],
    }))
    .map_err(|e| DispatchErr::Other(format!("build chat request: {e}")))?;

    let resp = client
        .chat_completion(req)
        .await
        .map_err(|e| DispatchErr::Other(format!("http: {e}")))?;

    let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|mc| match mc {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Parts(_) => String::new(),
        })
        .unwrap_or_default();
    let finish_reason = resp.choices.first().and_then(|c| c.finish_reason.clone());

    let response = serde_json::json!({
        "status": "ok",
        "text": text,
        "model": resp.model,
        "finish_reason": finish_reason,
    });
    serde_json::to_string(&response).map_err(|e| DispatchErr::Other(format!("serialize: {e}")))
}

/// Wynik routingu aliasu przez `ModelRuntimeExecutor`. `target_model` to model,
/// na którym dispatch realnie wylądował (primary albo fallback — telemetria
/// pozycji w `ExecutionContext.route_metadata`).
struct AliasRouteResult {
    response_json: String,
    target_model: String,
}

/// Błąd routingu aliasu przez executor — rozdzielony, żeby `dispatch` mapowało
/// "żaden target niedostępny" na `NotFound` (jak legacy `DispatchErr::NotFound`),
/// a realny błąd backendu na `Internal`.
enum AliasRouteError {
    NoTarget(String),
    Dispatch(String),
}

/// Routuje addonowy `service_request` celujący w alias przez TĘ SAMĄ ścieżkę co
/// `/v1`/flow: `ModelRuntimeExecutor`. Najpierw próbuje powierzchni chat
/// (`execute_chat`), a gdy resolver odrzuci ją jako capability-mismatch — alias
/// serwuje embeddingi, więc schodzi na `execute_embeddings`. Failover po
/// kandydatach (primary + fallbacks, w tym EMBEDDED na telefonie) i metryka
/// fallbacku dzieją się WEWNĄTRZ executora; tu tylko mapujemy payload addona
/// (surowy JSON) na request OpenAI i odpowiedź z powrotem na kształt
/// `{status, text|embeddings, model}` którego oczekuje SDK addona.
async fn route_alias_via_executor(
    executor: &Arc<crate::services::runtime::executor::ModelRuntimeExecutor>,
    alias: &str,
    payload_json: &str,
    exec_ctx: &mut crate::services::runtime::context::ExecutionContext,
) -> Result<AliasRouteResult, AliasRouteError> {
    use crate::api::openai::types::{ChatCompletionRequest, EmbeddingRequest, MessageContent};
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::resolver::ResolveError;

    // Chat: payload addona jako pojedyncza wiadomość user (lustro
    // `dispatch_to_service`/`dispatch_http` — semantyka Completion zachowana).
    let chat_req: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model": alias,
        "messages": [{"role": "user", "content": payload_json}],
    }))
    .map_err(|e| AliasRouteError::Dispatch(format!("build chat request: {e}")))?;

    match executor.execute_chat(chat_req, exec_ctx).await {
        Ok(resp) => {
            let text = resp
                .choices
                .first()
                .and_then(|c| c.message.content.as_ref())
                .map(|mc| match mc {
                    MessageContent::Text(s) => s.clone(),
                    MessageContent::Parts(_) => String::new(),
                })
                .unwrap_or_default();
            let finish_reason = resp.choices.first().and_then(|c| c.finish_reason.clone());
            let body = serde_json::json!({
                "status": "ok",
                "text": text,
                "model": resp.model,
                "finish_reason": finish_reason,
            });
            let response_json = serde_json::to_string(&body)
                .map_err(|e| AliasRouteError::Dispatch(format!("serialize: {e}")))?;
            return Ok(AliasRouteResult {
                response_json,
                target_model: resp.model,
            });
        }
        // Alias nie serwuje powierzchni chat — spróbuj embeddingów. Pozostałe
        // błędy resolvera (np. zła konfiguracja primary) propagujemy.
        Err(ExecutorError::Resolve(ResolveError::CapabilityUnsupported { .. })) => {}
        Err(ExecutorError::Resolve(
            ResolveError::NoLiveInstance(_) | ResolveError::UnknownModel(_),
        )) => {
            return Err(AliasRouteError::NoTarget(
                "alias_no_target_available".to_string(),
            ));
        }
        Err(ExecutorError::AllCandidatesFailed { last_error, .. }) => {
            return Err(AliasRouteError::Dispatch(last_error));
        }
        Err(e) => return Err(AliasRouteError::Dispatch(e.to_string())),
    }

    // Embeddings: input z pola `input` payloadu (string lub lista) — gdy brak,
    // cały payload traktujemy jako pojedynczy tekst do osadzenia.
    let input = embeddings_input_from_payload(payload_json);
    let emb_req = EmbeddingRequest {
        model: alias.to_string(),
        input,
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: serde_json::Map::new(),
    };
    match executor.execute_embeddings(emb_req, exec_ctx).await {
        Ok(resp) => {
            let embeddings: Vec<&Vec<f32>> = resp.data.iter().map(|d| &d.embedding).collect();
            let body = serde_json::json!({
                "status": "ok",
                "model": resp.model,
                "embeddings": embeddings,
            });
            let response_json = serde_json::to_string(&body)
                .map_err(|e| AliasRouteError::Dispatch(format!("serialize: {e}")))?;
            Ok(AliasRouteResult {
                response_json,
                target_model: resp.model,
            })
        }
        Err(ExecutorError::Resolve(
            ResolveError::NoLiveInstance(_)
            | ResolveError::UnknownModel(_)
            | ResolveError::CapabilityUnsupported { .. },
        )) => Err(AliasRouteError::NoTarget(
            "alias_no_target_available".to_string(),
        )),
        Err(ExecutorError::AllCandidatesFailed { last_error, .. }) => {
            Err(AliasRouteError::Dispatch(last_error))
        }
        Err(e) => Err(AliasRouteError::Dispatch(e.to_string())),
    }
}

/// Wyciąga input embeddingów z payloadu addona. Akceptuje `{"input": "..."}`,
/// `{"input": ["..."]}`, gołego stringa JSON i fallback: cały payload jako
/// jeden tekst (addon mógł wysłać surowy tekst bez obudowy).
fn embeddings_input_from_payload(payload_json: &str) -> crate::api::openai::types::EmbeddingInput {
    use crate::api::openai::types::EmbeddingInput;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) {
        if let Some(field) = value.get("input") {
            if let Some(s) = field.as_str() {
                return EmbeddingInput::Single(s.to_string());
            }
            if let Some(arr) = field.as_array() {
                let texts: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                if !texts.is_empty() {
                    return EmbeddingInput::Multiple(texts);
                }
            }
        }
        if let Some(s) = value.as_str() {
            return EmbeddingInput::Single(s.to_string());
        }
    }
    EmbeddingInput::Single(payload_json.to_string())
}

// =============================================================================
// Audit helpers — write directly to DB so callers without an AddonState can
// emit the same chain as the WASM host wrapper.
// =============================================================================

fn emit_audit(
    db: &DbPool,
    caller: &CallerContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    result: &str,
    error_message: Option<&str>,
) {
    emit_audit_full(
        db,
        caller,
        action,
        resource_type,
        resource_id,
        RiskClass::Unclassified,
        result,
        error_message,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_audit_full(
    db: &DbPool,
    caller: &CallerContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    risk_class: RiskClass,
    result: &str,
    error_message: Option<&str>,
) {
    let Ok(conn) = db.write() else {
        return;
    };
    emit_audit_inner(
        &conn,
        caller,
        action,
        resource_type,
        resource_id,
        risk_class,
        result,
        error_message,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_audit_inner(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    risk_class: RiskClass,
    result: &str,
    error_message: Option<&str>,
) {
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let risk_db = risk_class.as_db_str();
    let action_hash = crate::addon::utils::fnv1a_hash(action);
    let instance = caller.instance_id.as_deref();
    let hash_input = crate::audit::chain::AuditRowHashInput {
        user_id: caller.user_id.as_deref(),
        addon_id: Some(caller.addon_id.as_str()),
        instance_id: instance,
        action,
        resource: None,
        resource_type,
        resource_id,
        result: Some(result),
        error_message,
        details: None,
        ip_address: None,
        node_id: None,
        severity: Some("info"),
        risk_class: risk_db,
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = match crate::audit::chain::compute_chain_for_insert(conn, &hash_input) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("service_call audit chain compute failed: {e}");
            return;
        }
    };
    // F2 P1.b — write `org_id` alongside the existing chained columns.
    // The owning org rides on `CallerContext.org_id`; system calls / boot
    // paths that do not pin a tenant fall back to `org-default` so the
    // column stays populated for new rows.
    let org_for_row = caller
        .org_id
        .as_deref()
        .unwrap_or(crate::services::org::DEFAULT_ORG_ID);
    let _ = conn.execute(
        "INSERT INTO audit_log (user_id, addon_id, instance_id, action, resource_type, resource_id, result, error_message, action_hash, risk_class, related_claim_id, request_id, timestamp, prev_hash, hash, org_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            caller.user_id, &caller.addon_id, instance,
            action, resource_type, resource_id,
            result, error_message, action_hash,
            risk_db, None::<&str>, None::<&str>,
            timestamp, prev_hash, hash,
            org_for_row
        ],
    );
}

/// Realny target rozwiązany przez `ModelRuntimeExecutor` dla wiersza
/// `alias_calls`. Wypełnia kolumny `target_used`/`target_node_id`/`service_id`/
/// `fallback_used`/`fallback_chain_position` wartościami z istniejącej ścieżki
/// failoveru zamiast twardych 0/NULL. `None` (executor niewpięty / dispatch
/// padł przed wyborem targetu) → kolumny pozostają nieznane.
struct AliasCallRoute<'a> {
    target_used: &'a str,
    target_node_id: Option<&'a str>,
    chain_position: i64,
    fallback_used: bool,
}

/// Wybiera realny `target_used` dla wiersza `alias_calls`. Pierwszenstwo ma
/// `served_model` z metadanych zwycieskiego `ResolvedExecutionTarget` (realny
/// model_name kandydata, na ktorym dispatch wyladowal), a NIE `model` z body
/// odpowiedzi: dla zdalnego `MeshForward` peer echo'uje alias jako `model`,
/// wiec body niesie alias, nie realny target. Fallback na `body_model` tylko
/// gdy metadane sa puste (dispatch padl przed wyborem targetu) — wtedy i tak
/// nie ma failoveru do zaudytowania.
fn resolved_target_used<'a>(served_model: Option<&'a str>, body_model: &'a str) -> &'a str {
    served_model.unwrap_or(body_model)
}

#[allow(clippy::too_many_arguments)]
fn log_alias_call(
    db: &DbPool,
    caller: &CallerContext,
    service_name: &str,
    route: Option<&AliasCallRoute<'_>>,
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
    let Ok(conn) = db.write() else {
        return;
    };
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

    // A1 §0.4: realne wartości z rozwiązanego targetu zamiast twardych 0/NULL.
    // Bez `route` (executor niewpięty / dispatch padł przed wyborem) logujemy
    // nazwę aliasu jako target i pozycję 0 — failover się nie odbył, więc to
    // nie jest cicha degradacja. `service_id` z lokalnego ownera targetu (gdy
    // znany); remote/embedded nie ma lokalnego id i zostaje NULL.
    let target_used = route.map(|r| r.target_used).unwrap_or(service_name);
    let target_node_id: Option<&str> = route.and_then(|r| r.target_node_id);
    let chain_position: i64 = route.map(|r| r.chain_position).unwrap_or(0);
    let fallback_used: i64 = route.map(|r| i64::from(r.fallback_used)).unwrap_or(0);

    let _ = conn.execute(
        "INSERT INTO alias_calls \
             (alias_id, alias_name, method, target_used, target_node_id, service_id, \
              caller_addon_id, caller_user_id, request_id, duration_ms, payload_bytes, \
              response_bytes, fallback_used, fallback_chain_position, result, error_code, ts) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            alias_id,
            service_name,
            "service.request",
            target_used,
            target_node_id,
            caller.addon_id,
            caller.user_id,
            request_id,
            duration_ms,
            payload_bytes,
            response_bytes,
            fallback_used,
            chain_position,
            result,
            error_code,
            ts,
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::EmbeddingInput;

    fn make_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("in-memory db")
    }

    /// Buduje executor z PUSTYM katalogiem (żaden model się nie rozwiązuje).
    /// Mirror `executor::tests::dummy_executor` — używany do dowodu, że alias
    /// idzie przez TĘ ścieżkę, a brak kandydata mapuje się na `no_target`.
    fn empty_executor() -> Arc<crate::services::runtime::executor::ModelRuntimeExecutor> {
        use crate::services::handles_cache::LiveHandlesCache;
        use crate::services::runtime::executor::ModelRuntimeExecutor;
        use crate::services::runtime::resolver::AliasResolver;
        let catalog = Arc::new(crate::services::catalog::CatalogProvider::new());
        let handles = Arc::new(LiveHandlesCache::new());
        let resolver = Arc::new(AliasResolver::new_with_static_id(
            handles,
            "local-node".to_string(),
        ));
        let local_inference = Arc::new(crate::inference::local::LocalInferenceHandler::new(
            crate::inference::shared_inference_manager(),
        ));
        Arc::new(ModelRuntimeExecutor::new(
            catalog,
            resolver,
            None,
            local_inference,
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            None,
        ))
    }

    fn system_caller(addon_id: &str) -> CallerContext {
        CallerContext {
            addon_id: addon_id.to_string(),
            user_id: None,
            instance_id: None,
            is_system_call: true,
            org_id: None,
        }
    }

    #[test]
    fn embeddings_input_single_string_field() {
        let input = embeddings_input_from_payload(r#"{"input":"hello"}"#);
        assert!(matches!(input, EmbeddingInput::Single(s) if s == "hello"));
    }

    #[test]
    fn embeddings_input_array_field() {
        let input = embeddings_input_from_payload(r#"{"input":["a","b"]}"#);
        match input {
            EmbeddingInput::Multiple(v) => assert_eq!(v, vec!["a", "b"]),
            other => panic!("expected Multiple, got {:?}", other),
        }
    }

    #[test]
    fn embeddings_input_falls_back_to_whole_payload() {
        // Brak pola `input` i nie-string JSON → cały payload jako jeden tekst.
        let input = embeddings_input_from_payload(r#"{"other":1}"#);
        assert!(matches!(input, EmbeddingInput::Single(s) if s == r#"{"other":1}"#));
    }

    /// Addonowy alias bez żadnego dostępnego targetu (pusty katalog executora)
    /// → routing przez executor zwraca `NotFound`, a `alias_calls` dostaje
    /// wiersz `no_target`. Dowodzi, że alias idzie PRZEZ executor (nie
    /// dispatch-by-name), a diagnostyka braku targetu jest sensowna.
    #[tokio::test]
    async fn addon_alias_no_target_routes_through_executor_and_logs() {
        let db = make_db();
        crate::db::repository::create_or_reactivate_model_alias_with_active(
            &db,
            "fixture-llm",
            "big-model",
            "first_available",
            "addon",
            Some("rag-addon"),
            true,
        )
        .expect("install alias");

        let executor = empty_executor();
        let req = ServiceCallRequest {
            caller: system_caller("rag-addon"),
            service_name: "fixture-llm".to_string(),
            payload_json: r#"{"input":"czesc"}"#.to_string(),
            timeout_ms: 0,
            alias_required: true,
        };
        let err = dispatch(
            req,
            &db,
            None,
            Some(&executor),
            None,
            &["service".to_string()],
        )
        .await
        .expect_err("no available target → NotFound");
        assert!(matches!(err, ServiceCallError::NotFound { .. }));

        // Wiersz alias_calls zapisany z wynikiem no_target.
        let conn = db.write().unwrap();
        let (result, fallback_used): (String, i64) = conn
            .query_row(
                "SELECT result, fallback_used FROM alias_calls WHERE alias_name = ?1",
                rusqlite::params!["fixture-llm"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("alias_calls row written");
        assert_eq!(result, "no_target");
        assert_eq!(fallback_used, 0);
    }

    /// `log_alias_call` z realnym `AliasCallRoute` (fallback na pozycji 2)
    /// zapisuje `fallback_used=1` + pozycję + realny `target_used`/`node_id`
    /// zamiast twardych 0/NULL.
    #[test]
    fn log_alias_call_persists_real_fallback_fields() {
        let db = make_db();
        crate::db::repository::create_or_reactivate_model_alias_with_active(
            &db,
            "rag-emb",
            "primary-emb",
            "first_available",
            "addon",
            Some("rag-addon"),
            true,
        )
        .expect("install alias");

        let route = AliasCallRoute {
            target_used: "tiny-emb",
            target_node_id: Some("phone-node"),
            chain_position: 2,
            fallback_used: true,
        };
        log_alias_call(
            &db,
            &system_caller("rag-addon"),
            "rag-emb",
            Some(&route),
            "req-1",
            12,
            5,
            7,
            None,
            "ok",
            None,
        );

        let conn = db.write().unwrap();
        let (target_used, node_id, fallback_used, position): (String, Option<String>, i64, i64) =
            conn.query_row(
                "SELECT target_used, target_node_id, fallback_used, fallback_chain_position \
                 FROM alias_calls WHERE alias_name = ?1",
                rusqlite::params!["rag-emb"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("alias_calls row");
        assert_eq!(target_used, "tiny-emb");
        assert_eq!(node_id.as_deref(), Some("phone-node"));
        assert_eq!(fallback_used, 1);
        assert_eq!(position, 2);
    }

    /// Bez `route` (executor niewpięty) `log_alias_call` loguje nazwę aliasu
    /// jako target i pozycję 0 — failover się nie odbył, brak cichej degradacji.
    #[test]
    fn log_alias_call_without_route_defaults_to_alias_name() {
        let db = make_db();
        crate::db::repository::create_or_reactivate_model_alias_with_active(
            &db,
            "rag-plain",
            "model-x",
            "first_available",
            "addon",
            Some("rag-addon"),
            true,
        )
        .expect("install alias");

        log_alias_call(
            &db,
            &system_caller("rag-addon"),
            "rag-plain",
            None,
            "req-2",
            1,
            1,
            1,
            None,
            "ok",
            None,
        );

        let conn = db.write().unwrap();
        let (target_used, fallback_used, position): (String, i64, i64) = conn
            .query_row(
                "SELECT target_used, fallback_used, fallback_chain_position \
                 FROM alias_calls WHERE alias_name = ?1",
                rusqlite::params!["rag-plain"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("alias_calls row");
        assert_eq!(target_used, "rag-plain");
        assert_eq!(fallback_used, 0);
        assert_eq!(position, 0);
    }

    /// Bug 1: zdalny (MeshForward) fallback. `served_model` z metadanych
    /// rozwiazanej sciezki niesie REALNY model targetu, a body odpowiedzi
    /// echo'uje alias — `target_used` musi pochodzic z metadanych, nie z body.
    #[test]
    fn resolved_target_used_prefers_served_model_over_body_alias() {
        // Peer odeslal alias jako `model` (MeshForward echo), ale metadane
        // zwycieskiego targetu znaja realny model_name — logujemy realny.
        assert_eq!(
            resolved_target_used(Some("tiny-emb-on-peer"), "rag-emb-alias"),
            "tiny-emb-on-peer"
        );
        // Brak metadanych (dispatch padl przed wyborem) → fallback na body.
        assert_eq!(resolved_target_used(None, "rag-emb-alias"), "rag-emb-alias");
        // Local/Embedded: metadane i body sie zgadzaja — realny model.
        assert_eq!(
            resolved_target_used(Some("small-model"), "small-model"),
            "small-model"
        );
    }
}
