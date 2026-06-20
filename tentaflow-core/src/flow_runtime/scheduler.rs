// =============================================================================
// File: flow_runtime/scheduler.rs — per-invocation DAG orchestrator
// =============================================================================
//
// One `FlowScheduler` exists per process (initialised by core boot via
// `init()`). Each call to `invoke` materialises a fresh invocation:
//
//   1.  Concurrency cap check (per `addon_id`, default 10). Denial returns
//       `ConcurrencyCapExceeded` and emits a collapsed `audit_log` row.
//   2.  UUIDv7 invocation_id minted (time-ordered → DB index stays hot for
//       recent rows).
//   3.  `INSERT INTO flow_invocations` with status='running'.
//   4.  Per-edge `BoundedDropOldest<FlowMessage>` (cap 100). Drop-oldest is
//       the documented backpressure policy (PM decision Q3).
//   5.  Per-operator tokio task spawned in topological order. Each operator
//       reads from every inbound edge and forwards to outbound edges using
//       the chunk-B passthrough semantics — chunk C swaps these for the
//       real Source/Predict/Threshold/Branch/Aggregate/Sink bodies.
//   6.  Sinks append every record they receive to a shared `Vec<toml::Value>`
//       (`sink_outputs`). When all operator tasks finish the vec is encoded
//       to a TOML scalar and written into `flow_invocations.result_toml`.
//   7.  If `wait_ms > 0` the call awaits completion with a tokio timeout. On
//       timeout the invocation continues in the background and the caller
//       receives the `running` status (the DB row is the source of truth).
//   8.  `cancel()` triggers the per-invocation `CancellationToken`; operator
//       tasks unwind on the next `select!` cycle and finalize writes
//       status='cancelled'.
//
// `flow_invocations` writes are wrapped in `spawn_blocking` because
// `DbPool` (the `Db` writer connection) and rusqlite are sync.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::anyhow;
use chrono::Utc;
use parking_lot::Mutex as PlMutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::addon::event_bus::EventBus;
use crate::addon::permissions::PermissionChecker;
use crate::db::DbPool;
use crate::services::runtime::quic_handle::ServiceManager;

use super::bounded_drop_oldest::BoundedDropOldest;
use super::operators::{self, OperatorContext, OperatorError, OutboundEdge};
use super::registry;
use super::types::{CompiledFlow, OperatorType};

/// Default ceiling on concurrent invocations per addon. PM decision Q1.
/// Per-addon override is loaded from manifest `[runtime] max_concurrency`
/// via `FlowScheduler::set_addon_concurrency_cap`.
pub const DEFAULT_CONCURRENCY_CAP: usize = 10;

/// Per-edge buffer capacity. Larger than the steady-state working set so
/// transient bursts (e.g. a yolo backend stall) are absorbed; once exceeded
/// the oldest record is dropped under the documented drop-oldest policy.
pub const EDGE_BUFFER_CAPACITY: usize = 100;

/// Upper bound for the sync wait window. PM decision Q2. Values above this
/// are clamped silently — `flow_invoke_v1` already validates the input
/// shape and a host call that holds the wasmtime thread for >30 s blocks
/// every other addon's host calls on the same worker.
pub const MAX_SYNC_WAIT_MS: u32 = 30_000;

/// Message flowing through the DAG. A `Record` carries one TOML value;
/// `Eof` is the in-band terminator broadcast when an operator's last input
/// closes. Operators forward `Eof` to their outbound edges and then close
/// the underlying channels so downstream receivers exit their loops.
#[derive(Debug, Clone)]
pub enum FlowMessage {
    Record(toml::Value),
    Eof,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum InvokeError {
    #[error("flow not found: addon='{addon_id}' flow='{flow_id}'")]
    FlowNotFound { addon_id: String, flow_id: String },
    #[error("concurrency cap exceeded for addon='{addon_id}' (cap={cap})")]
    ConcurrencyCapExceeded { addon_id: String, cap: usize },
    #[error("invocation not found: id='{0}'")]
    NotFound(String),
    #[error("database error: {0}")]
    Db(String),
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct InvocationStatus {
    pub invocation_id: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub operators_completed: i64,
    pub operators_total: i64,
    pub error: Option<String>,
    pub result_toml: Option<String>,
}

// -----------------------------------------------------------------------------
// Per-invocation runtime state held while the DAG is in flight.
// -----------------------------------------------------------------------------

struct InFlight {
    addon_id: String,
    cancel: CancellationToken,
    /// Edges keyed by (from, to[, port]). Held so the finalize step can sum
    /// drop counts across the whole DAG into a single backpressure audit row.
    edges: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
}

pub struct FlowScheduler {
    db: DbPool,
    /// addon_id -> set of running invocation_ids. Used both for the cap
    /// check and for `cancel()` / `status()` lookups without touching DB
    /// for in-memory state (DB still owns the authoritative status text).
    by_addon: PlMutex<HashMap<String, HashSet<String>>>,
    /// Per-addon concurrency cap overrides loaded from manifest
    /// `[runtime] max_concurrency`. Addons absent from this map fall back
    /// to `DEFAULT_CONCURRENCY_CAP`. Populated by `set_addon_concurrency_cap`
    /// at addon install/start; cleared by `clear_addon_concurrency_cap` on
    /// uninstall.
    concurrency_overrides: PlMutex<HashMap<String, usize>>,
    in_flight: PlMutex<HashMap<String, InFlight>>,
    /// Late-bound by boot wiring after Router is constructed. Operators in
    /// chunk C treat `None` as fatal-but-clean (Predict returns Internal).
    /// `ArcSwap`-style: one writer, many readers, no blocking on the hot path.
    service_manager: PlMutex<Option<Arc<ServiceManager>>>,
    /// Late-bound after AddonManager owns the canonical EventBus. Sink
    /// operators emitting events publish here; tests may leave this `None`.
    event_bus: PlMutex<Option<Arc<EventBus>>>,
    /// Late-bound `ModelRuntimeExecutor` (A1 §0.4). Predict-by-alias routes
    /// through it for availability-aware failover (embedded/local/remote)
    /// — the same path `/v1` uses. `None` until `set_executor`; Predict then
    /// keeps the legacy dispatch-by-name behavior.
    executor:
        PlMutex<Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>>>,
}

static GLOBAL: OnceLock<Arc<FlowScheduler>> = OnceLock::new();

/// RAII guard returned by `reserve_slot`. Releases the concurrency slot AND
/// removes the in-flight entry on drop so a panic anywhere between
/// reservation and the normal finalize path cannot leak a slot. Move the
/// guard into the spawned background task whenever the invocation outlives
/// the caller frame (e.g. `wait_ms` timeout) — dropping it early would free
/// the slot before the DAG actually finishes.
struct CapGuard {
    scheduler: Arc<FlowScheduler>,
    addon_id: String,
    invocation_id: String,
}

impl Drop for CapGuard {
    fn drop(&mut self) {
        self.scheduler
            .release_slot(&self.addon_id, &self.invocation_id);
        let mut g = self.scheduler.in_flight.lock();
        g.remove(&self.invocation_id);
    }
}

impl FlowScheduler {
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            by_addon: PlMutex::new(HashMap::new()),
            concurrency_overrides: PlMutex::new(HashMap::new()),
            in_flight: PlMutex::new(HashMap::new()),
            service_manager: PlMutex::new(None),
            event_bus: PlMutex::new(None),
            executor: PlMutex::new(None),
        }
    }

    /// Records the manifest-declared concurrency cap for an addon. Called
    /// from `addon::lifecycle::install_addon` (and on every start so a
    /// `[runtime]` rewrite picked up by a reload becomes immediately
    /// effective). `cap == 0` is treated as "remove override" so a manifest
    /// edit that drops the section re-anchors the addon to the default.
    pub fn set_addon_concurrency_cap(&self, addon_id: &str, cap: u32) {
        let mut g = self.concurrency_overrides.lock();
        if cap == 0 {
            g.remove(addon_id);
        } else {
            g.insert(addon_id.to_string(), cap as usize);
        }
    }

    /// Drops the per-addon cap entry. Called from `uninstall_addon` so a
    /// reinstall starts from a clean slate.
    pub fn clear_addon_concurrency_cap(&self, addon_id: &str) {
        self.concurrency_overrides.lock().remove(addon_id);
    }

    /// Looks up the effective cap for an addon — manifest override when
    /// present, `DEFAULT_CONCURRENCY_CAP` otherwise.
    pub fn effective_concurrency_cap(&self, addon_id: &str) -> usize {
        self.concurrency_overrides
            .lock()
            .get(addon_id)
            .copied()
            .unwrap_or(DEFAULT_CONCURRENCY_CAP)
    }

    /// Late-bind the service manager handle. Called once from core boot
    /// after `Router::new`. Subsequent calls overwrite — production wiring
    /// triggers this exactly once; rebinding is harmless because the handle
    /// is read on every operator dispatch.
    pub fn set_service_manager(&self, sm: Arc<ServiceManager>) {
        *self.service_manager.lock() = Some(sm);
    }

    /// Late-bind the event bus handle (same lifecycle as `set_service_manager`).
    pub fn set_event_bus(&self, bus: Arc<EventBus>) {
        *self.event_bus.lock() = Some(bus);
    }

    /// Late-bind the runtime executor (same lifecycle as `set_service_manager`).
    /// Wired once from boot after `Router::new` so Predict-by-alias reaches the
    /// availability-aware failover path.
    pub fn set_executor(
        &self,
        executor: Arc<crate::services::runtime::executor::ModelRuntimeExecutor>,
    ) {
        *self.executor.lock() = Some(executor);
    }

    /// Snapshot of the service manager handle. `None` until `set_service_manager`
    /// is called — operators raising side effects against it must surface a
    /// clean `Internal` error in that case.
    pub fn service_manager(&self) -> Option<Arc<ServiceManager>> {
        self.service_manager.lock().clone()
    }

    /// Snapshot of the event bus handle (same semantics as `service_manager`).
    pub fn event_bus(&self) -> Option<Arc<EventBus>> {
        self.event_bus.lock().clone()
    }

    /// Snapshot of the runtime executor handle (same semantics as
    /// `service_manager`). `None` until `set_executor`.
    pub fn executor(
        &self,
    ) -> Option<Arc<crate::services::runtime::executor::ModelRuntimeExecutor>> {
        self.executor.lock().clone()
    }

    /// Installs the process-wide singleton. First caller wins; subsequent
    /// calls are no-ops so test harnesses (which may init multiple times via
    /// `db::init`) do not panic.
    pub fn init(db: DbPool) {
        let _ = GLOBAL.set(Arc::new(Self::new(db)));
    }

    /// Returns the global instance. Panics only if `init` was never called —
    /// boot code wires this up before any host function can fire.
    pub fn global() -> Arc<FlowScheduler> {
        GLOBAL
            .get()
            .cloned()
            .expect("FlowScheduler::init must be called before global()")
    }

    /// Synchronous entry point. Returns the final status when the invocation
    /// finishes inside `wait_ms`, otherwise returns the running status with
    /// the live invocation_id (caller polls via `status`).
    pub async fn invoke(
        self: &Arc<Self>,
        addon_id: &str,
        flow_id: &str,
        input: toml::Value,
        wait_ms: u32,
        actor_user_id: Option<String>,
        org_id: Option<String>,
    ) -> Result<InvocationStatus, InvokeError> {
        let flow =
            registry::global()
                .get(addon_id, flow_id)
                .ok_or_else(|| InvokeError::FlowNotFound {
                    addon_id: addon_id.to_string(),
                    flow_id: flow_id.to_string(),
                })?;

        let invocation_id = uuid::Uuid::now_v7().to_string();
        let started_at = Utc::now().to_rfc3339();

        // Cap check + reservation happen BEFORE any DB write so a denied
        // invocation never leaves an orphan row behind. The returned guard
        // releases the slot automatically if any subsequent step (DB insert,
        // task spawn, panic in run_invocation) fails before normal finalize.
        let guard = self
            .reserve_slot(addon_id, &invocation_id, org_id.as_deref())
            .await?;

        let operators_total = flow.def.operators.len() as i64;

        let db = self.db.clone();
        let inv_id_owned = invocation_id.clone();
        let addon_id_owned = addon_id.to_string();
        let flow_id_owned = flow_id.to_string();
        let started_at_owned = started_at.clone();
        let insert_res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = db
                .write()
                .map_err(|e| anyhow!("db pool poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO flow_invocations \
                    (id, addon_id, flow_id, started_at, status, operators_completed, operators_total, actor_user_id) \
                 VALUES (?1, ?2, ?3, ?4, 'running', 0, ?5, ?6)",
                rusqlite::params![
                    inv_id_owned,
                    addon_id_owned,
                    flow_id_owned,
                    started_at_owned,
                    operators_total,
                    actor_user_id
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| InvokeError::Internal(format!("join error: {e}")))?;

        if let Err(e) = insert_res {
            // Roll back the reservation — we never started the DAG. The guard
            // is dropped here implicitly when this function returns, freeing
            // the slot and clearing the in-flight entry (which is empty —
            // nothing was inserted yet).
            drop(guard);
            return Err(InvokeError::Db(e.to_string()));
        }

        let cancel = CancellationToken::new();
        let edges = match build_edges(&flow) {
            Ok(e) => e,
            Err(e) => {
                // Guard drops here → slot released, in_flight entry never
                // existed. DB row stays 'running' — boot recovery will sweep.
                drop(guard);
                return Err(e);
            }
        };
        let operators_completed = Arc::new(AtomicI64::new(0));
        let sink_outputs: Arc<AsyncMutex<Vec<toml::Value>>> = Arc::new(AsyncMutex::new(Vec::new()));

        {
            let mut g = self.in_flight.lock();
            g.insert(
                invocation_id.clone(),
                InFlight {
                    addon_id: addon_id.to_string(),
                    cancel: cancel.clone(),
                    edges: edges.values().cloned().collect(),
                },
            );
        }

        let run_future = self.clone().run_invocation(
            invocation_id.clone(),
            addon_id.to_string(),
            flow.clone(),
            input,
            edges,
            sink_outputs,
            operators_completed.clone(),
            cancel.clone(),
            started_at.clone(),
            operators_total,
            guard,
            org_id.clone(),
        );

        if wait_ms == 0 {
            tokio::spawn(run_future);
            return Ok(InvocationStatus {
                invocation_id,
                status: "running".into(),
                started_at,
                finished_at: None,
                operators_completed: 0,
                operators_total,
                error: None,
                result_toml: None,
            });
        }

        let clamped = wait_ms.min(MAX_SYNC_WAIT_MS);
        let handle = tokio::spawn(run_future);
        let wait =
            tokio::time::timeout(std::time::Duration::from_millis(clamped as u64), handle).await;
        match wait {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(join_err)) => {
                // Task panicked — finalize must have already marked failed,
                // but we still surface a synthetic error so the caller sees
                // something meaningful instead of a stale 'running'.
                Err(InvokeError::Internal(format!("task panic: {join_err}")))
            }
            Err(_elapsed) => {
                // Timeout — task is still alive (the guard moved into the
                // spawned future keeps the slot reserved until the DAG
                // finishes). Return current DB state.
                self.status(&invocation_id, addon_id).await
            }
        }
    }

    /// Reads the authoritative status row from the DB. Async because rusqlite
    /// is sync and the connection mutex must be acquired off the tokio worker.
    pub async fn status(
        &self,
        invocation_id: &str,
        addon_id: &str,
    ) -> Result<InvocationStatus, InvokeError> {
        let db = self.db.clone();
        let inv = invocation_id.to_string();
        let addon = addon_id.to_string();
        let row = tokio::task::spawn_blocking(move || -> Result<InvocationStatus, InvokeError> {
            let conn = db
                .read()
                .map_err(|e| InvokeError::Db(format!("pool poisoned: {e}")))?;
            conn.query_row(
                "SELECT id, status, started_at, finished_at, operators_completed, \
                        operators_total, error, result_toml \
                 FROM flow_invocations WHERE id = ?1 AND addon_id = ?2",
                rusqlite::params![inv, addon],
                |r| {
                    Ok(InvocationStatus {
                        invocation_id: r.get::<_, String>(0)?,
                        status: r.get::<_, String>(1)?,
                        started_at: r.get::<_, String>(2)?,
                        finished_at: r.get::<_, Option<String>>(3)?,
                        operators_completed: r.get::<_, i64>(4)?,
                        operators_total: r.get::<_, i64>(5)?,
                        error: r.get::<_, Option<String>>(6)?,
                        result_toml: r.get::<_, Option<String>>(7)?,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => InvokeError::NotFound(inv.clone()),
                other => InvokeError::Db(other.to_string()),
            })
        })
        .await
        .map_err(|e| InvokeError::Internal(format!("join error: {e}")))??;
        Ok(row)
    }

    /// Requests cancellation. Idempotent: cancelling a finished invocation is
    /// a no-op (the token has no observers) and `status()` continues to
    /// surface the terminal state.
    pub async fn cancel(&self, invocation_id: &str, addon_id: &str) -> Result<(), InvokeError> {
        let token = {
            let g = self.in_flight.lock();
            match g.get(invocation_id) {
                Some(inf) if inf.addon_id == addon_id => Some(inf.cancel.clone()),
                Some(_) => {
                    return Err(InvokeError::NotFound(invocation_id.to_string()));
                }
                None => None,
            }
        };
        match token {
            Some(t) => {
                t.cancel();
                Ok(())
            }
            None => {
                // Verify the invocation exists for this addon — terminal
                // invocations are not in the in-flight map but still exist
                // in DB, so cancelling them is a quiet success.
                let _ = self.status(invocation_id, addon_id).await?;
                Ok(())
            }
        }
    }

    // ----- internals -------------------------------------------------------

    async fn reserve_slot(
        self: &Arc<Self>,
        addon_id: &str,
        invocation_id: &str,
        org_id: Option<&str>,
    ) -> Result<CapGuard, InvokeError> {
        let effective_cap = self.effective_concurrency_cap(addon_id);
        let denied = {
            let mut g = self.by_addon.lock();
            let entry = g.entry(addon_id.to_string()).or_default();
            if entry.len() >= effective_cap {
                true
            } else {
                entry.insert(invocation_id.to_string());
                false
            }
        };
        if denied {
            self.emit_concurrency_cap_audit(addon_id, org_id, effective_cap)
                .await;
            return Err(InvokeError::ConcurrencyCapExceeded {
                addon_id: addon_id.to_string(),
                cap: effective_cap,
            });
        }
        Ok(CapGuard {
            scheduler: self.clone(),
            addon_id: addon_id.to_string(),
            invocation_id: invocation_id.to_string(),
        })
    }

    fn release_slot(&self, addon_id: &str, invocation_id: &str) {
        let mut g = self.by_addon.lock();
        if let Some(set) = g.get_mut(addon_id) {
            set.remove(invocation_id);
            if set.is_empty() {
                g.remove(addon_id);
            }
        }
    }

    async fn emit_concurrency_cap_audit(&self, addon_id: &str, org_id: Option<&str>, cap: usize) {
        let db = self.db.clone();
        let addon = addon_id.to_string();
        let org = org_id
            .unwrap_or(crate::services::org::DEFAULT_ORG_ID)
            .to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = match db.write() {
                Ok(c) => c,
                Err(_) => return,
            };
            let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let details = serde_json::json!({
                "reason": "max_concurrent_invocations",
                "cap": cap,
            })
            .to_string();
            let hash_input = crate::audit::chain::AuditRowHashInput {
                user_id: None,
                addon_id: Some(&addon),
                instance_id: None,
                action: "flow.invoke",
                resource: None,
                resource_type: Some("flow"),
                resource_id: None,
                result: Some("denied"),
                error_message: None,
                details: Some(&details),
                ip_address: None,
                node_id: None,
                severity: Some("warn"),
                risk_class: "C",
                related_claim_id: None,
                request_id: None,
                timestamp: &timestamp,
            };
            let (prev_hash, hash) =
                match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("flow_runtime: audit chain compute failed: {e}");
                        return;
                    }
                };
            let _ = conn.execute(
                "INSERT INTO audit_log \
                    (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                     result, error_message, severity, risk_class, details, prev_hash, hash, org_id) \
                 VALUES (?1, NULL, ?2, 'flow.invoke', 'flow', NULL, \
                         'denied', NULL, 'warn', 'C', ?3, ?4, ?5, ?6)",
                rusqlite::params![timestamp, addon, details, prev_hash, hash, org],
            );
        })
        .await;
    }

    async fn emit_backpressure_audit(
        &self,
        addon_id: &str,
        flow_id: &str,
        invocation_id: &str,
        dropped_total: u64,
        org_id: Option<&str>,
    ) {
        if dropped_total == 0 {
            return;
        }
        let db = self.db.clone();
        let addon = addon_id.to_string();
        let flow = flow_id.to_string();
        let inv = invocation_id.to_string();
        let org = org_id
            .unwrap_or(crate::services::org::DEFAULT_ORG_ID)
            .to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = match db.write() {
                Ok(c) => c,
                Err(_) => return,
            };
            let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let details = serde_json::json!({
                "flow_id": flow,
                "invocation_id": inv,
                "dropped_count": dropped_total,
            })
            .to_string();
            let hash_input = crate::audit::chain::AuditRowHashInput {
                user_id: None,
                addon_id: Some(&addon),
                instance_id: None,
                action: "flow.backpressure_drop",
                resource: None,
                resource_type: Some("flow"),
                resource_id: Some(&flow),
                result: Some("backpressure_drop"),
                error_message: None,
                details: Some(&details),
                ip_address: None,
                node_id: None,
                severity: Some("warn"),
                risk_class: "C",
                related_claim_id: None,
                request_id: None,
                timestamp: &timestamp,
            };
            let (prev_hash, hash) =
                match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("flow_runtime: backpressure audit chain failed: {e}");
                        return;
                    }
                };
            let _ = conn.execute(
                "INSERT INTO audit_log \
                    (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                     result, error_message, severity, risk_class, details, prev_hash, hash, org_id) \
                 VALUES (?1, NULL, ?2, 'flow.backpressure_drop', 'flow', ?3, \
                         'backpressure_drop', NULL, 'warn', 'C', ?4, ?5, ?6, ?7)",
                rusqlite::params![timestamp, addon, flow, details, prev_hash, hash, org],
            );
        })
        .await;
    }

    /// Emits a warning audit row when the finalize UPDATE on `flow_invocations`
    /// fails. The DB row stays in `running` state and `mark_orphaned_invocations`
    /// at next boot will sweep it.
    async fn emit_finalize_db_error_audit(
        &self,
        addon_id: &str,
        flow_id: &str,
        invocation_id: &str,
        error_message: &str,
        org_id: Option<&str>,
    ) {
        let db = self.db.clone();
        let addon = addon_id.to_string();
        let flow = flow_id.to_string();
        let inv = invocation_id.to_string();
        let err = error_message.to_string();
        let org = org_id
            .unwrap_or(crate::services::org::DEFAULT_ORG_ID)
            .to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = match db.write() {
                Ok(c) => c,
                Err(_) => return,
            };
            let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let details = serde_json::json!({
                "flow_id": flow,
                "invocation_id": inv,
                "error": err,
            })
            .to_string();
            let hash_input = crate::audit::chain::AuditRowHashInput {
                user_id: None,
                addon_id: Some(&addon),
                instance_id: None,
                action: "flow.finalize.db_error",
                resource: None,
                resource_type: Some("flow"),
                resource_id: Some(&flow),
                result: Some("error"),
                error_message: Some(&err),
                details: Some(&details),
                ip_address: None,
                node_id: None,
                severity: Some("warn"),
                risk_class: "C",
                related_claim_id: None,
                request_id: None,
                timestamp: &timestamp,
            };
            let (prev_hash, hash) =
                match crate::audit::chain::compute_chain_for_insert(&conn, &hash_input) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("flow_runtime: finalize-db-error audit chain failed: {e}");
                        return;
                    }
                };
            let _ = conn.execute(
                "INSERT INTO audit_log \
                    (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                     result, error_message, severity, risk_class, details, prev_hash, hash, org_id) \
                 VALUES (?1, NULL, ?2, 'flow.finalize.db_error', 'flow', ?3, \
                         'error', ?4, 'warn', 'C', ?5, ?6, ?7, ?8)",
                rusqlite::params![timestamp, addon, flow, err, details, prev_hash, hash, org],
            );
        })
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_invocation(
        self: Arc<Self>,
        invocation_id: String,
        addon_id: String,
        flow: Arc<CompiledFlow>,
        input: toml::Value,
        edges: HashMap<EdgeKey, Arc<BoundedDropOldest<FlowMessage>>>,
        sink_outputs: Arc<AsyncMutex<Vec<toml::Value>>>,
        operators_completed: Arc<AtomicI64>,
        cancel: CancellationToken,
        started_at: String,
        operators_total: i64,
        guard: CapGuard,
        org_id: Option<String>,
    ) -> InvocationStatus {
        // Build the per-invocation PermissionChecker once and share by Arc.
        // The operators that need it (Predict, Sink event/ui_notify) clone
        // the Arc; the rest carry the handle without ever using it.
        let permission_checker = Arc::new(PermissionChecker::new(self.db.clone()));
        let permissions = load_declared_permissions(&self.db, &addon_id).await;

        let mut tasks: JoinSet<OperatorOutcome> = JoinSet::new();
        for op_id in &flow.topo_order {
            let op = flow
                .def
                .operators
                .iter()
                .find(|o| o.id == *op_id)
                .expect("topo_order references known operator");
            let inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>> = edges
                .iter()
                .filter(|(k, _)| k.to == op.id)
                .map(|(_, v)| v.clone())
                .collect();
            let outbound: Vec<OutboundEdge> = edges
                .iter()
                .filter(|(k, _)| k.from == op.id)
                .map(|(k, v)| (k.port.clone(), v.clone()))
                .collect();

            let ctx = OperatorContext {
                addon_id: addon_id.clone(),
                flow_id: flow.def.id.clone(),
                invocation_id: invocation_id.clone(),
                operator_id: op.id.clone(),
                input_toml: input.clone(),
                params: op.params.clone(),
                db: self.db.clone(),
                permissions: permissions.clone(),
                permission_checker: permission_checker.clone(),
                service_manager: self.service_manager(),
                executor: self.executor(),
                event_bus: self.event_bus(),
                sink_outputs: sink_outputs.clone(),
                org_id: org_id.clone(),
            };
            let op_type = op.op_type;
            let token = cancel.clone();
            let completed = operators_completed.clone();
            tasks.spawn(async move {
                let outcome = dispatch_operator(op_type, ctx, inbound, outbound, token).await;
                if matches!(outcome, OperatorOutcome::Completed) {
                    completed.fetch_add(1, Ordering::Relaxed);
                }
                outcome
            });
        }

        let mut failure: Option<String> = None;
        let mut cancelled = false;
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(OperatorOutcome::Completed) => {}
                Ok(OperatorOutcome::Cancelled) => cancelled = true,
                Ok(OperatorOutcome::Failed(err)) => {
                    if failure.is_none() {
                        failure = Some(err);
                    }
                    cancel.cancel();
                }
                Err(join_err) => {
                    if failure.is_none() {
                        failure = Some(format!("operator task panic: {join_err}"));
                    }
                    cancel.cancel();
                }
            }
        }

        let final_status = if let Some(err) = &failure {
            FinalStatus::Failed(err.clone())
        } else if cancelled || cancel.is_cancelled() {
            FinalStatus::Cancelled
        } else {
            FinalStatus::Completed
        };

        let result_toml = if matches!(final_status, FinalStatus::Completed) {
            let outs = sink_outputs.lock().await;
            encode_sink_outputs(&outs)
        } else {
            None
        };

        let dropped_total: u64 = {
            let g = self.in_flight.lock();
            g.get(&invocation_id)
                .map(|inf| inf.edges.iter().map(|e| e.dropped()).sum())
                .unwrap_or(0)
        };
        self.emit_backpressure_audit(
            &addon_id,
            &flow.def.id,
            &invocation_id,
            dropped_total,
            org_id.as_deref(),
        )
        .await;

        let ops_done = operators_completed.load(Ordering::Relaxed);
        let finished_at = Utc::now().to_rfc3339();

        let db = self.db.clone();
        let inv_id_for_db = invocation_id.clone();
        let status_text: &'static str = match &final_status {
            FinalStatus::Completed => "completed",
            FinalStatus::Failed(_) => "failed",
            FinalStatus::Cancelled => "cancelled",
        };
        let error_text = match &final_status {
            FinalStatus::Failed(e) => Some(e.clone()),
            FinalStatus::Cancelled => Some("cancelled".to_string()),
            FinalStatus::Completed => None,
        };
        let result_toml_for_db = result_toml.clone();
        let finished_at_for_db = finished_at.clone();
        let update_join = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = db.write().map_err(|e| anyhow!("pool poisoned: {e}"))?;
            conn.execute(
                "UPDATE flow_invocations \
                 SET status = ?1, finished_at = ?2, operators_completed = ?3, \
                     error = ?4, result_toml = ?5 \
                 WHERE id = ?6",
                rusqlite::params![
                    status_text,
                    finished_at_for_db,
                    ops_done,
                    error_text,
                    result_toml_for_db,
                    inv_id_for_db
                ],
            )?;
            Ok(())
        })
        .await;
        match update_join {
            Ok(Ok(())) => {}
            Ok(Err(db_err)) => {
                error!(
                    "flow_runtime: finalize UPDATE failed for invocation {}: {}",
                    invocation_id, db_err
                );
                self.emit_finalize_db_error_audit(
                    &addon_id,
                    &flow.def.id,
                    &invocation_id,
                    &db_err.to_string(),
                    org_id.as_deref(),
                )
                .await;
            }
            Err(join_err) => {
                error!(
                    "flow_runtime: finalize UPDATE join failed for invocation {}: {}",
                    invocation_id, join_err
                );
                self.emit_finalize_db_error_audit(
                    &addon_id,
                    &flow.def.id,
                    &invocation_id,
                    &format!("join error: {join_err}"),
                    org_id.as_deref(),
                )
                .await;
            }
        }

        // Slot + in-flight entry are released by the CapGuard's Drop impl on
        // function return. This keeps the contract: even if a panic unwinds
        // run_invocation between the JoinSet drain and finalize, the slot is
        // always returned to the addon's pool.
        drop(guard);

        let final_error = match &final_status {
            FinalStatus::Failed(e) => Some(e.clone()),
            FinalStatus::Cancelled => Some("cancelled".to_string()),
            FinalStatus::Completed => None,
        };
        InvocationStatus {
            invocation_id,
            status: status_text.to_string(),
            started_at,
            finished_at: Some(finished_at),
            operators_completed: ops_done,
            operators_total,
            error: final_error,
            result_toml,
        }
    }
}

// -----------------------------------------------------------------------------
// Operator runtime — dispatches to flow_runtime::operators::*.
// -----------------------------------------------------------------------------

pub(crate) enum OperatorOutcome {
    Completed,
    Cancelled,
    Failed(String),
}

enum FinalStatus {
    Completed,
    Failed(String),
    Cancelled,
}

/// Reads the declared permission identifiers for `addon_id` from
/// `addon_declared_permissions`. Returns an empty vector when the addon has
/// no row in the table (e.g. test fixtures that bypass `lifecycle::install`).
async fn load_declared_permissions(db: &DbPool, addon_id: &str) -> Vec<String> {
    let pool = db.clone();
    let addon = addon_id.to_string();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
        let conn = pool.read().map_err(|e| anyhow!("db pool poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT permission_type FROM addon_declared_permissions WHERE addon_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![addon], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(s) = r {
                out.push(s);
            }
        }
        Ok(out)
    })
    .await;
    match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            warn!("flow_runtime: declared_permissions lookup failed: {e}");
            Vec::new()
        }
        Err(e) => {
            warn!("flow_runtime: declared_permissions join error: {e}");
            Vec::new()
        }
    }
}

async fn dispatch_operator(
    op_type: OperatorType,
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> OperatorOutcome {
    let res: Result<(), OperatorError> = match op_type {
        OperatorType::Source => operators::run_source(ctx, inbound, outbound, cancel.clone()).await,
        OperatorType::Predict => {
            operators::run_predict(ctx, inbound, outbound, cancel.clone()).await
        }
        OperatorType::Threshold => {
            operators::run_threshold(ctx, inbound, outbound, cancel.clone()).await
        }
        OperatorType::Branch => operators::run_branch(ctx, inbound, outbound, cancel.clone()).await,
        OperatorType::Aggregate => {
            operators::run_aggregate(ctx, inbound, outbound, cancel.clone()).await
        }
        OperatorType::Sink => operators::run_sink(ctx, inbound, outbound, cancel.clone()).await,
    };
    match res {
        Ok(()) => {
            if cancel.is_cancelled() {
                OperatorOutcome::Cancelled
            } else {
                OperatorOutcome::Completed
            }
        }
        Err(e) => OperatorOutcome::Failed(e.to_string()),
    }
}

// -----------------------------------------------------------------------------
// Edge construction
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct EdgeKey {
    from: String,
    to: String,
    port: Option<String>,
}

fn build_edges(
    flow: &CompiledFlow,
) -> Result<HashMap<EdgeKey, Arc<BoundedDropOldest<FlowMessage>>>, InvokeError> {
    let mut m: HashMap<EdgeKey, Arc<BoundedDropOldest<FlowMessage>>> = HashMap::new();
    for e in &flow.def.edges {
        let key = EdgeKey {
            from: e.from.clone(),
            to: e.to.clone(),
            port: e.port.clone(),
        };
        // Parser already rejects duplicate edges at compile time; this is the
        // belt-and-suspenders check that guarantees the scheduler never
        // silently coalesces a model error into a single buffer.
        if m.contains_key(&key) {
            return Err(InvokeError::Internal(format!(
                "duplicate edge in compiled flow: from='{}' to='{}' port={:?}",
                key.from, key.to, key.port
            )));
        }
        m.insert(key, BoundedDropOldest::new(EDGE_BUFFER_CAPACITY));
    }
    Ok(m)
}

/// Encodes the sink stream as a TOML document with a single `records` array.
/// `toml::Value::Array` is not a valid top-level TOML document, so we wrap
/// it in a table to keep the string round-trippable by the addon SDK.
fn encode_sink_outputs(outs: &[toml::Value]) -> Option<String> {
    let mut table = toml::value::Table::new();
    table.insert("records".to_string(), toml::Value::Array(outs.to_vec()));
    match toml::to_string(&toml::Value::Table(table)) {
        Ok(s) => {
            debug!(
                "flow_runtime: encoded {} sink records ({} bytes)",
                outs.len(),
                s.len()
            );
            Some(s)
        }
        Err(e) => {
            warn!("flow_runtime: TOML encode failed: {e}");
            None
        }
    }
}
