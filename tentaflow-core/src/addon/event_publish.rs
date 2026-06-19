// =============================================================================
// File: addon/event_publish.rs — pure-async EventBus publish (no WASM)
// =============================================================================
//
// Process-wide handle to the live `EventBus` (the one owned by the core's
// AddonManager) plus a thin function the WASM host wrapper and the
// flow_runtime operators both call to publish an event. Keeps the
// permission gate ("events" + per-event resource), audit row, and DB
// subscription side effects identical across the two call paths.

use std::sync::{Arc, OnceLock};

use thiserror::Error;
use tracing::warn;

use crate::addon::event_bus::{Event, EventBus};
use crate::addon::permissions::PermissionChecker;
use crate::audit::RiskClass;
use crate::db::DbPool;
use crate::services::service_call::CallerContext;

static GLOBAL_BUS: OnceLock<Arc<EventBus>> = OnceLock::new();

/// Installs the process-wide EventBus handle. First caller wins; subsequent
/// calls are no-ops so tests that bring up multiple buses do not panic. The
/// core boot path calls this exactly once with the bus owned by
/// `AddonManager`. Unit tests that need bus-isolated setups leave the global
/// unset and consume the bus explicitly.
pub fn init_global(bus: Arc<EventBus>) {
    if GLOBAL_BUS.set(bus).is_err() {
        tracing::warn!(
            "event_publish::init_global called more than once; subsequent bus ignored. \
             flow_runtime operators will publish to the first-installed bus."
        );
    }
}

/// Returns the global bus or `None` if `init_global` was never called.
/// Flow operators treat `None` as fatal-but-clean (operator returns
/// Internal). The WASM host wrapper never reaches this — addons use the
/// bus carried inside their `AddonState`.
pub fn global() -> Option<Arc<EventBus>> {
    GLOBAL_BUS.get().cloned()
}

#[derive(Debug, Error)]
pub enum EventPublishError {
    #[error("permission denied for event '{event_type}'")]
    Permission { event_type: String },
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Pure-async publish. `bus` is taken explicitly so the WASM wrapper can
/// pass its `state.event_bus` while flow operators pass `global()`.
pub fn publish_event(
    bus: &EventBus,
    db: &DbPool,
    caller: &CallerContext,
    permission_checker: Option<&PermissionChecker>,
    permissions: &[String],
    event_type: &str,
    payload: serde_json::Value,
) -> Result<(), EventPublishError> {
    if !permission_granted(
        permissions,
        caller,
        permission_checker,
        "events",
        Some(event_type),
    ) {
        emit_audit(
            db,
            caller,
            "event.publish",
            Some("events"),
            Some(event_type),
            "denied",
            None,
        );
        return Err(EventPublishError::Permission {
            event_type: event_type.to_string(),
        });
    }

    let event = Event {
        event_type: event_type.to_string(),
        source_addon: Some(caller.addon_id.clone()),
        source_user: caller.user_id.clone(),
        payload,
        timestamp: chrono::Utc::now(),
    };
    bus.publish(event);

    emit_audit(
        db,
        caller,
        "event.publish",
        Some("events"),
        Some(event_type),
        "ok",
        None,
    );
    Ok(())
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
        None => return false,
    };
    checker
        .check(&caller.addon_id, user_id, permission_type, resource)
        .is_granted()
}

fn emit_audit(
    db: &DbPool,
    caller: &CallerContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    result: &str,
    error_message: Option<&str>,
) {
    let Ok(conn) = db.write() else {
        return;
    };
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let action_hash = crate::addon::utils::fnv1a_hash(action);
    let risk_db = RiskClass::Unclassified.as_db_str();
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
    let (prev_hash, hash) = match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input)
    {
        Ok(p) => p,
        Err(e) => {
            warn!("event_publish audit chain compute failed: {e}");
            return;
        }
    };
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
            timestamp, prev_hash, hash, org_for_row
        ],
    );
}
