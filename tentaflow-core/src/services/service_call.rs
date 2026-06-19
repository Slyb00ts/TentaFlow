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
pub async fn dispatch(
    req: ServiceCallRequest,
    db: &DbPool,
    service_manager: Option<&Arc<ServiceManager>>,
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
    {
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
            Ok(_) => {}
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
        None => return Err(DispatchErr::NotFound),
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

#[allow(clippy::too_many_arguments)]
fn log_alias_call(
    db: &DbPool,
    caller: &CallerContext,
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
            caller.addon_id,
            caller.user_id,
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
