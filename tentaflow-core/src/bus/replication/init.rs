// =============================================================================
// File: bus/replication/init.rs — M2 replication startup/shutdown (agent G)
// =============================================================================
//
// The one function `tentaflow/src/main.rs` (coordinator) calls after mesh
// startup to bring M2 replication up on this node, and its shutdown twin.
// Builds the real production wiring (`glue::GlueLeaderFactory`/
// `GlueFollowerFactory`/`AuditLogReplAudit`, `assignment::
// SqliteLedgerAssignmentStore`, `manager::IrohTransport`) around
// `manager::ReplicationManager`, installs it as the mesh's `ALPN_BUS`
// accept handler, replays this node's existing assignments, and spawns the
// three background loops a running `ReplicationManager` needs (lease
// watchdog, mesh-disconnect forwarding, ledger-materialization poll — see
// each loop's own doc below for why each exists and, for the last one,
// why it is a poll rather than a push).
//
// LEDGER MATERIALIZATION SIGNAL (M2 simplification, PLAN-M2 §3 item 3):
// there is no "core resource materialized" hook anywhere in `sync::
// core_materializer`/`sync::runtime` this file could subscribe to instead
// — `core_materializer::apply_core_operation` is a plain synchronous
// function called from the ledger's own apply path with no observer
// callback, event channel, or `tokio::sync::watch` of any kind threaded
// through it. Adding one is a `sync/*` change outside this task's
// exclusive file list (`bus/replication/**` only). The documented
// fallback below — polling `bus_assignment_list_for_node` every 1 s and
// diffing `(leader_epoch, updated_at_ms)` per partition — is therefore
// the M2 answer; a future wave that adds a real materialization hook can
// replace `spawn_assignment_poll_loop` with a subscriber without changing
// this function's public signature.
//
// WHAT THE POLL IS NO LONGER THE ONLY THING FOR (wave 3, agent G2): a 1 s
// poll is fine for state that can wait a second, and fatal for a leader
// already dialing in on the wrong millisecond of that second — which is
// what made `tests/process_three_node_bus_failover.rs`'s smoke test publish
// into `isr=1, required=2` while every node's own `ROLE` already said
// Leader/Follower. `ReplicationManager::accept_stream` therefore now
// resolves its own registry miss against the same materialized rows
// (`manager::await_local_assignment`, bounded by `manager::ASSIGNMENT_AWAIT`)
// instead of bouncing the Hello, so an inbound stream no longer depends on
// this loop's timing at all. What this loop still owns, and only it owns:
// assignments NOBODY has dialed — a restarted node catching up on
// partitions it leads, whose followers are the ones dialing nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tentaflow_protocol::environment::NodeEnvironment;

use crate::bus::replication::assignment::{PartitionAssignment, SqliteLedgerAssignmentStore};
use crate::bus::replication::follower::FollowerConfig;
use crate::bus::replication::glue::{
    AuditLogReplAudit, GlueFollowerFactory, GlueLeaderFactory, PartitionProvider,
};
use crate::bus::replication::leader::LeaderConfig;
use crate::bus::replication::manager::{
    IrohTransport, ReplicationManager, ReplicationManagerConfig, Transport,
};
use crate::bus::replication::metrics::LeaderMetrics;
use crate::bus::replication::{election, manager as manager_mod};
use crate::bus::ReplicationCoordinator;
use crate::db::DbPool;
use crate::mesh::iroh_manager::{IrohMeshEvent, IrohMeshManager};

/// Every input `init` needs, gathered in one place so `tentaflow/src/
/// main.rs` has a single call site.
pub struct ReplicationInitConfig {
    pub db: DbPool,
    pub mesh: Arc<IrohMeshManager>,
    pub local_node_id: String,
    pub local_env: NodeEnvironment,
    /// The contract with agent S — see `glue::PartitionProvider`'s own
    /// doc. `bus::mod::BusService` is the real implementor.
    pub provider: Arc<dyn PartitionProvider>,
    /// Lease-watchdog poll cadence (PLAN-M2 §3 default: 500 ms). Also
    /// reused as the ledger-materialization poll's own cadence would be
    /// too tight for that purpose (that loop hardcodes 1 s — see its own
    /// doc); this field is deliberately narrow to the one PLAN-M2 pins a
    /// number for.
    pub lease_check_interval: Duration,
}

impl ReplicationInitConfig {
    /// PLAN-M2 §3's own default (500 ms) for `lease_check_interval`.
    pub const DEFAULT_LEASE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
}

/// Ledger-materialization poll cadence (module doc's "M2 simplification").
/// Not `pub`/configurable: a fixed 1 s is the documented tradeoff, not a
/// tunable production knob.
const ASSIGNMENT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Brings M2 replication up on this node: builds the production
/// `ReplicationManager` wiring, installs the `ALPN_BUS` accept handler,
/// replays this node's existing partition assignments, spawns the
/// lease-check/mesh-event/assignment-poll background loops, and installs
/// the manager as `bus::global()`'s `ReplicationCoordinator` (`BusService::
/// set_replication` — a no-op, logged at `warn`, if `bus::global()` has no
/// `BusService` yet, since a caller that races `init` ahead of `bus::
/// init` has a startup-order bug this function cannot fix for it).
pub async fn init(cfg: ReplicationInitConfig) -> anyhow::Result<Arc<ReplicationManager>> {
    let transport: Arc<dyn Transport> = Arc::new(IrohTransport::new(Arc::clone(&cfg.mesh)));

    // `SqliteLedgerAssignmentStore` is the ONLY store wired here (PLAN-M2
    // §3's "+ `FjallLedgerAdmission` fallback" is available in
    // `manager.rs` but deliberately unused by this function): it already
    // implements BOTH `AssignmentStore` and `LedgerAdmission` (`manager.
    // rs`'s "Alternate real AssignmentStore/LedgerAdmission" section) by
    // reaching `sync::runtime` internally, so this function needs no
    // separately-injected `Arc<dyn SyncLedgerStore>` at all — one fewer
    // required `ReplicationInitConfig` field than PLAN-M2 §3's literal
    // sketch, and one fewer thing for a caller to wire correctly.
    let ledger_store = Arc::new(SqliteLedgerAssignmentStore::new(cfg.db.clone()));

    let metrics = Arc::new(LeaderMetrics::new());
    let leader_factory = Arc::new(GlueLeaderFactory::new(
        cfg.local_node_id.clone(),
        cfg.local_env,
        Arc::clone(&cfg.provider),
        Arc::clone(&transport),
        LeaderConfig::default(),
        Arc::clone(&metrics),
    ));
    let follower_factory = Arc::new(GlueFollowerFactory::new(
        cfg.local_node_id.clone(),
        cfg.local_env,
        Arc::clone(&cfg.provider),
        FollowerConfig::default(),
    ));
    let audit = Arc::new(AuditLogReplAudit::new(
        cfg.db.clone(),
        cfg.local_node_id.clone(),
    ));

    let manager = ReplicationManager::new(ReplicationManagerConfig {
        instance_id: cfg.provider.instance_id().to_string(),
        local_node_id: cfg.local_node_id.clone(),
        local_env: cfg.local_env,
        transport,
        ledger: ledger_store.clone(),
        assignments: ledger_store.clone(),
        leader_factory,
        follower_factory,
        audit,
        leo_query_timeout: election::LEO_QUERY_TIMEOUT,
        majority_await_timeout: election::MAJORITY_AWAIT_TIMEOUT,
    });

    manager.install_accept_handler(&cfg.mesh).await;

    let instance_id = cfg.provider.instance_id().to_string();
    let initial = ledger_store.list_for_node(&instance_id, &cfg.local_node_id)?;
    for assignment in initial {
        manager.apply_assignment(assignment).await;
    }

    spawn_lease_check_loop(Arc::clone(&manager), cfg.lease_check_interval);
    spawn_peer_disconnect_loop(Arc::clone(&manager), Arc::clone(&cfg.mesh));
    spawn_assignment_poll_loop(
        Arc::clone(&manager),
        Arc::clone(&ledger_store),
        instance_id,
        cfg.local_node_id.clone(),
    );

    match crate::bus::global() {
        Some(svc) => {
            svc.set_replication(Arc::clone(&manager) as Arc<dyn ReplicationCoordinator>);
            // Without this, `create_topic`'s assignment-proposal block
            // (`bus::mod`'s `self.assignment_store()`) is permanently `None`
            // on every production node — `set_replication` alone wires the
            // coordinator (role/preflight/snapshot), not the store
            // `create_topic` proposes new partition assignments into. A live
            // krytyk pass on M2 found the registry never got its first row
            // on a real cluster even after the `local_node_id` bootstrap fix
            // landed, because this call was simply missing.
            svc.set_assignment_store(ledger_store.clone());
        }
        None => {
            tracing::warn!(
                "replication::init: bus::global() has no BusService yet — \
                 set_replication skipped (startup-order issue upstream)"
            );
        }
    }

    Ok(manager)
}

/// Shuts M2 replication down on this node: cancels every background loop
/// `init` spawned and stops every partition this node currently leads or
/// follows (`ReplicationManager::shutdown`'s own doc — flushes each
/// stopped partition's persisted meta best-effort). Does NOT clear
/// `BusService::set_replication` — a `BusService` observing a coordinator
/// whose partitions have all stopped degrades to `NotLeader`/`NoAssignment`
/// on every call already (`preflight`/`role` both read the now-empty
/// registry), the same effective behavior `set_replication(None)` would
/// produce if that method existed, without this function needing a second
/// handle back into `bus::mod` just to call it.
pub fn stop(manager: &Arc<ReplicationManager>) {
    manager.shutdown();
}

/// PLAN-M2 §3 (`lease_check_interval`, default 500 ms): periodically scans
/// every partition this node follows for an expired leader lease and
/// starts an election for it (`ReplicationManager::check_leases`'s own
/// doc). Ends when `manager.shutdown_token()` is cancelled.
fn spawn_lease_check_loop(manager: Arc<ReplicationManager>, interval: Duration) {
    let shutdown = manager.shutdown_token();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => manager.check_leases().await,
            }
        }
    });
}

/// PLAN-M2 §1b/§3: forwards `IrohMeshEvent::PeerDisconnected` into
/// `ReplicationManager::on_peer_disconnected` — the lease-expiry
/// ACCELERATOR (never the only signal; `manager.rs`'s own doc), so a
/// follower whose leader's mesh connection just dropped does not always
/// have to wait out the full lease before starting an election. Ends when
/// `manager.shutdown_token()` is cancelled OR the mesh's own event
/// broadcaster closes (mesh shutdown already implies replication shutdown
/// is imminent).
fn spawn_peer_disconnect_loop(manager: Arc<ReplicationManager>, mesh: Arc<IrohMeshManager>) {
    let shutdown = manager.shutdown_token();
    let mut rx = mesh.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                event = rx.recv() => match event {
                    Ok(IrohMeshEvent::PeerDisconnected { node_id }) => {
                        manager.on_peer_disconnected(&node_id);
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // A missed disconnect event only costs the
                        // ACCELERATION this loop provides — the real
                        // `leader_lease_ms` watchdog (`follower.rs`)
                        // still fires on its own regardless.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    });
}

/// Module doc's "LEDGER MATERIALIZATION SIGNAL" section: polls this node's
/// assignments every `ASSIGNMENT_POLL_INTERVAL` and applies any that
/// changed (new partition, or an existing one whose `(leader_epoch,
/// updated_at_ms)` moved — the cheapest correct "did this row change"
/// check available without a real change-data-capture hook, since
/// `leader_epoch` alone would miss an ISR-only update and `updated_at_ms`
/// alone would miss two updates landing in the same millisecond).
/// `ReplicationManager::apply_assignment` is itself idempotent against a
/// truly-unchanged assignment (its own doc), so an extra reconciliation
/// here on a tied poll is harmless, not just cheap. Each apply it does make
/// also wakes any `accept_stream` parked in
/// `ReplicationManager::await_local_assignment` for that partition, so a
/// row this loop materializes is acted on immediately rather than at the
/// parked stream's next re-read tick.
fn spawn_assignment_poll_loop(
    manager: Arc<ReplicationManager>,
    store: Arc<SqliteLedgerAssignmentStore>,
    instance_id: String,
    local_node_id: String,
) {
    let shutdown = manager.shutdown_token();
    tokio::spawn(async move {
        let mut known: HashMap<manager_mod::PartitionKey, (u32, i64)> = HashMap::new();
        let mut ticker = tokio::time::interval(ASSIGNMENT_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    let rows = tokio::task::block_in_place(|| store.list_for_node(&instance_id, &local_node_id));
                    let rows: Vec<PartitionAssignment> = match rows {
                        Ok(rows) => rows,
                        Err(e) => {
                            tracing::warn!(error = %e, "replication: assignment poll failed");
                            continue;
                        }
                    };
                    for a in rows {
                        let key = (a.org_id.clone(), a.topic.clone(), a.partition);
                        let fingerprint = (a.leader_epoch, a.updated_at_ms);
                        if known.get(&key) == Some(&fingerprint) {
                            continue;
                        }
                        known.insert(key, fingerprint);
                        manager.apply_assignment(a).await;
                    }
                }
            }
        }
    });
}
