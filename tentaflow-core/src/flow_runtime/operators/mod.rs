// =============================================================================
// File: flow_runtime/operators/mod.rs — operator dispatch + shared helpers
// =============================================================================
//
// Each operator lives in its own submodule and exposes `pub async fn run(...)`
// with the same signature so the scheduler can dispatch them uniformly. The
// shared `OperatorContext` carries the per-invocation handles every operator
// needs (DB pool, declared permissions, ServiceManager, EventBus, audit
// caller context). Lifecycle is per-operator-task: the scheduler builds one
// `OperatorContext` per operator instance, passes it by value into the
// spawned tokio task, and the task drops it on completion.
//
// Error mapping: every operator returns `Result<(), OperatorError>` which the
// scheduler converts to `OperatorOutcome::Failed(String)`. Per-record errors
// are absorbed by the `on_error` policy and surfaced as audit rows; they do
// NOT bubble up unless `on_error="fail"`.

use std::sync::Arc;

use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::addon::event_bus::EventBus;
use crate::addon::permissions::PermissionChecker;
use crate::db::DbPool;
use crate::services::runtime::quic_handle::ServiceManager;
use crate::services::service_call::CallerContext;

use super::bounded_drop_oldest::BoundedDropOldest;
use super::scheduler::FlowMessage;

pub mod aggregate;
pub mod branch;
pub mod predict;
pub mod sink;
pub mod source;
pub mod threshold;

pub use aggregate::run as run_aggregate;
pub use branch::run as run_branch;
pub use predict::run as run_predict;
pub use sink::run as run_sink;
pub use source::run as run_source;
pub use threshold::run as run_threshold;

/// Per-operator-task context. Cloned cheaply (Arcs everywhere) by the
/// scheduler per spawned operator. The `caller` carries the addon_id /
/// instance_id used for audit/permission attribution; it is system-side
/// (`is_system_call=true`) because the flow runtime runs on behalf of an
/// installed addon but has no user session.
#[derive(Clone)]
pub struct OperatorContext {
    pub addon_id: String,
    pub flow_id: String,
    pub invocation_id: String,
    pub operator_id: String,
    pub input_toml: toml::Value,
    pub params: toml::Value,
    pub db: DbPool,
    /// Manifest-declared permission identifiers (`"service"`, `"events"`, ...).
    pub permissions: Vec<String>,
    pub permission_checker: Arc<PermissionChecker>,
    pub service_manager: Option<Arc<ServiceManager>>,
    pub event_bus: Option<Arc<EventBus>>,
    /// Shared collector for Sink kind="invocation_result". The scheduler
    /// owns the Vec; every Sink task appends into it.
    pub sink_outputs: Arc<AsyncMutex<Vec<toml::Value>>>,
    /// Tenant scope for SQL sinks, audit rows, and any per-org subsystem the
    /// operator touches. `None` when the invocation was started outside a
    /// resolved OrgContext (legacy host-fn callers, boot recovery sweeps);
    /// SQL sink falls back to `org-default` with a warn in that case.
    pub org_id: Option<String>,
}

impl OperatorContext {
    pub fn caller(&self) -> CallerContext {
        CallerContext {
            addon_id: self.addon_id.clone(),
            user_id: None,
            instance_id: None,
            is_system_call: true,
            org_id: self.org_id.clone(),
        }
    }
}

/// Outbound edge plus its optional port label (Some only for Branch fan-out).
pub type OutboundEdge = (Option<String>, Arc<BoundedDropOldest<FlowMessage>>);

#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    #[error("alias not found: {0}")]
    AliasNotFound(String),
    #[error("alias inactive: {0}")]
    AliasInactive(String),
    #[error("service call failed: {0}")]
    ServiceCallFailed(String),
    #[error("field missing: {0}")]
    FieldMissing(String),
    #[error("bad params: {0}")]
    BadParams(String),
    #[error("expression failed: {0}")]
    ExpressionFailed(String),
    #[error("sink failed: {0}")]
    SinkFailed(String),
    #[error("timeout after {0}ms")]
    Timeout(u32),
    #[error("internal: {0}")]
    Internal(String),
    #[error("subsystem not initialized: {0}")]
    SubsystemNotInitialized(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    Fail,
    Skip,
    EmitNull,
}

impl OnError {
    pub fn from_params(params: &toml::Value, default: OnError) -> Self {
        match params.get("on_error").and_then(|v| v.as_str()) {
            Some("fail") => OnError::Fail,
            Some("skip") => OnError::Skip,
            Some("emit_null") => OnError::EmitNull,
            _ => default,
        }
    }
}

/// Reads a string param. Returns `None` for missing key or non-string type.
pub fn read_param_string(params: &toml::Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn read_param_u32(params: &toml::Value, key: &str) -> Option<u32> {
    let v = params.get(key)?;
    if let Some(i) = v.as_integer() {
        if i >= 0 && i <= u32::MAX as i64 {
            return Some(i as u32);
        }
    }
    None
}

pub fn read_param_f64(params: &toml::Value, key: &str) -> Option<f64> {
    let v = params.get(key)?;
    if let Some(f) = v.as_float() {
        return Some(f);
    }
    v.as_integer().map(|i| i as f64)
}

/// Default per-operator timeout. PM design: 10 s.
pub const DEFAULT_TIMEOUT_MS: u32 = 10_000;

pub fn timeout_ms_from_params(params: &toml::Value) -> u32 {
    read_param_u32(params, "timeout_ms").unwrap_or(DEFAULT_TIMEOUT_MS)
}

/// Resolves a dotted path inside a TOML record. Empty path returns the root.
/// Returns `None` if any path segment is missing or traverses a non-table.
pub fn record_field_dot<'a>(record: &'a toml::Value, dot_path: &str) -> Option<&'a toml::Value> {
    if dot_path.is_empty() {
        return Some(record);
    }
    let mut cur = record;
    for seg in dot_path.split('.') {
        cur = cur.as_table()?.get(seg)?;
    }
    Some(cur)
}

/// Sends a record to every outbound edge matching `port`. Branch uses this.
pub fn send_to_port(outbound: &[OutboundEdge], port: &str, record: toml::Value) -> bool {
    let mut delivered = false;
    for (p, edge) in outbound {
        if p.as_deref() == Some(port) {
            edge.send(FlowMessage::Record(record.clone()));
            delivered = true;
        }
    }
    delivered
}

/// Forwards `Eof` to every outbound edge and closes the channel.
pub fn close_outbound(outbound: &[OutboundEdge]) {
    for (_, edge) in outbound {
        edge.send(FlowMessage::Eof);
        edge.close();
    }
}

/// Reads the next message from any of the inbound edges using per-edge EOF
/// tracking. Returns `None` when every inbound edge is EOF, `Some(Err(()))`
/// when cancelled, otherwise `Some(Ok(record))`.
///
/// Sinks/operators that need to distinguish individual edges should drive
/// this loop themselves; this helper is the common-case fan-in.
pub async fn next_record(
    inbound: &[Arc<BoundedDropOldest<FlowMessage>>],
    eof_received: &mut [bool],
    cancel: &CancellationToken,
) -> Option<Result<toml::Value, ()>> {
    loop {
        if eof_received.iter().all(|d| *d) {
            return None;
        }
        let mut made_progress = false;
        for (idx, edge) in inbound.iter().enumerate() {
            if eof_received[idx] {
                continue;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Some(Err(())),
                msg = edge.recv() => {
                    made_progress = true;
                    match msg {
                        Some(FlowMessage::Record(v)) => return Some(Ok(v)),
                        Some(FlowMessage::Eof) | None => {
                            eof_received[idx] = true;
                        }
                    }
                }
            }
        }
        if !made_progress {
            return None;
        }
    }
}

/// Emits an `audit_log` row with `action = "flow.op.<name>.<outcome>"`. Best
/// effort — a failed insert is logged at `warn` and dropped.
pub fn emit_op_audit(
    db: &DbPool,
    addon_id: &str,
    flow_id: &str,
    invocation_id: &str,
    operator_id: &str,
    op_name: &str,
    outcome: &str,
    result: &str,
    details: Option<serde_json::Value>,
    org_id: Option<&str>,
) {
    let action = format!("flow.op.{op_name}.{outcome}");
    let conn = match db.write() {
        Ok(c) => c,
        Err(_) => return,
    };
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut details_obj = serde_json::Map::new();
    details_obj.insert(
        "flow_id".to_string(),
        serde_json::Value::String(flow_id.to_string()),
    );
    details_obj.insert(
        "invocation_id".to_string(),
        serde_json::Value::String(invocation_id.to_string()),
    );
    details_obj.insert(
        "operator_id".to_string(),
        serde_json::Value::String(operator_id.to_string()),
    );
    if let Some(extra) = details {
        if let Some(map) = extra.as_object() {
            for (k, v) in map {
                details_obj.insert(k.clone(), v.clone());
            }
        }
    }
    let details_json = serde_json::Value::Object(details_obj).to_string();
    let hash_input = crate::audit::chain::AuditRowHashInput {
        user_id: None,
        addon_id: Some(addon_id),
        instance_id: None,
        action: &action,
        resource: None,
        resource_type: Some("flow"),
        resource_id: Some(flow_id),
        result: Some(result),
        error_message: None,
        details: Some(&details_json),
        ip_address: None,
        node_id: None,
        severity: Some("info"),
        risk_class: "C",
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input)
    {
        Ok(p) => p,
        Err(e) => {
            warn!("flow_runtime op audit: chain compute failed: {e}");
            return;
        }
    };
    let org_for_row = org_id.unwrap_or(crate::services::org::DEFAULT_ORG_ID);
    let _ = conn.execute(
        "INSERT INTO audit_log \
            (timestamp, user_id, addon_id, action, resource_type, resource_id, \
             result, error_message, severity, risk_class, details, prev_hash, hash, org_id) \
         VALUES (?1, NULL, ?2, ?3, 'flow', ?4, ?5, NULL, 'info', 'C', ?6, ?7, ?8, ?9)",
        rusqlite::params![
            timestamp,
            addon_id,
            action,
            flow_id,
            result,
            details_json,
            prev_hash,
            hash,
            org_for_row
        ],
    );
}

/// Helper: wraps `tokio::time::timeout` and converts elapsed into `Timeout`.
pub async fn with_timeout<F, T>(timeout_ms: u32, fut: F) -> Result<T, OperatorError>
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(Duration::from_millis(timeout_ms as u64), fut).await {
        Ok(v) => Ok(v),
        Err(_) => Err(OperatorError::Timeout(timeout_ms)),
    }
}

/// Converts a TOML value to a JSON value. Used to bridge the record world
/// (TOML) with subsystems that take JSON (event_publish, service_call,
/// storage_sql_exec parameters).
pub fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut m = serde_json::Map::with_capacity(t.len());
            for (k, vv) in t.iter() {
                m.insert(k.clone(), toml_to_json(vv));
            }
            serde_json::Value::Object(m)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

/// Converts a JSON value to a TOML value. Used to fold a Predict response
/// back into the in-flight record.
pub fn json_to_toml(v: &serde_json::Value) -> toml::Value {
    match v {
        serde_json::Value::Null => toml::Value::String(String::new()),
        serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => toml::Value::String(s.clone()),
        serde_json::Value::Array(arr) => toml::Value::Array(arr.iter().map(json_to_toml).collect()),
        serde_json::Value::Object(map) => {
            let mut t = toml::value::Table::new();
            for (k, vv) in map.iter() {
                t.insert(k.clone(), json_to_toml(vv));
            }
            toml::Value::Table(t)
        }
    }
}
