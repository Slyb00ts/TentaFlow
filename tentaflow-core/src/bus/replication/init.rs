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

use crate::bus::instance::BusInstanceId;
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
use crate::bus::replication::router;
use crate::bus::replication::{election, manager as manager_mod};
use crate::bus::ReplicationCoordinator;
use crate::db::DbPool;
use crate::mesh::iroh_manager::{IrohMeshEvent, IrohMeshManager};

/// Every input `init` needs, gathered in one place so `tentaflow/src/
/// main.rs` has a single call site.
pub struct ReplicationInitConfig {
    pub db: DbPool,
    pub mesh: Arc<IrohMeshManager>,
    /// plan-app-platform §1.6/W5 review finding D1/D7: the caller must
    /// STATE which instance it is starting replication for, not leave this
    /// function to guess. Before this field existed, `init` bound the new
    /// manager to `bus::global()` — "whichever single engine happens to be
    /// running" — which is exactly wrong once a second instance exists:
    /// racing `native_on_enable` calls resolve `global()` to instance A
    /// while wiring up instance B's manager (A's `publish` then reads B's
    /// registry — cross-instance data inside one process), and once both
    /// engines are up `global()` returns `None` and the newer manager
    /// silently gets no coordinator at all (RF=1 semantics on a replicated
    /// topic, no error surfaced). `init` now resolves `bus::instance(&id)`
    /// with this field and fails outright if that engine is not running —
    /// see `init`'s own doc.
    pub instance_id: BusInstanceId,
    pub local_node_id: String,
    pub local_env: NodeEnvironment,
    /// The contract with agent S — see `glue::PartitionProvider`'s own
    /// doc. `bus::mod::BusService` is the real implementor. `init` checks
    /// this provider's own `instance_id()` against `instance_id` above and
    /// fails if they disagree — two sources of truth for the same fact
    /// must never be allowed to drift silently.
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
/// the manager as `cfg.instance_id`'s OWN `BusService` engine's
/// `ReplicationCoordinator`/assignment store (`bus::instance(&cfg.
/// instance_id)`, never `bus::global()` — `ReplicationInitConfig::
/// instance_id`'s own doc explains why `global()` is unsafe here: it names
/// "whichever single engine is running", not "this manager's engine").
///
/// The engine lookup happens FIRST, before anything else this function
/// builds (transport, ledger store, manager, router registration,
/// background loops): `bus::instance` returning `None` means the engine
/// this manager is meant to serve is not running yet — a startup-order bug
/// in the caller (W6's `native_on_enable` must run `bus::init_instance`
/// before `replication::init` for the same instance), surfaced as a hard
/// error here rather than a `warn` log and a manager left running with no
/// coordinator. Failing before any allocation also means an error return
/// leaks nothing: no router registration, no background loop, no ledger
/// handle to clean up.
pub async fn init(cfg: ReplicationInitConfig) -> anyhow::Result<Arc<ReplicationManager>> {
    if cfg.provider.instance_id() != cfg.instance_id.as_str() {
        anyhow::bail!(
            "replication::init: cfg.instance_id ({}) does not match \
             cfg.provider.instance_id() ({}) — this ReplicationInitConfig was built \
             for the wrong engine",
            cfg.instance_id,
            cfg.provider.instance_id()
        );
    }
    let svc = crate::bus::instance(&cfg.instance_id).ok_or_else(|| {
        anyhow::anyhow!(
            "replication::init: no BusService engine running for instance '{}' — \
             `bus::init_instance` must run before `replication::init` for this instance \
             (startup-order bug upstream, not a degraded mode to warn through)",
            cfg.instance_id
        )
    })?;

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

    // `cfg.instance_id` (checked against `cfg.provider.instance_id()`
    // above) is the ONE source of instance identity from here down —
    // `ReplicationManagerConfig::instance_id`, the `router::register` key,
    // and every `ledger_store` call below all key off the exact same
    // typed value, never re-derived from the provider a second time.
    let instance_id = cfg.instance_id.as_str().to_string();

    let manager = ReplicationManager::new(ReplicationManagerConfig {
        instance_id: instance_id.clone(),
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

    // W5 review round 2 finding 1: this fallible read MUST run before
    // `router::register` below, not after — the doc two paragraphs up
    // promises "an error return leaks nothing: no router registration, no
    // background loop", and that was false while this `?` sat after
    // `register`: a transient SQLite busy/locked here returned `Err` with
    // the manager already installed in `router`'s global `MANAGERS` table
    // and reachable from inbound `Hello`/`LeoQuery` streams, while the
    // caller (seeing `Err`) never holds the `Arc` and can never call
    // `replication::stop` to unregister it — a half-live participant with
    // no lease watchdog, no mesh-disconnect loop, no assignment poll, and
    // `set_replication`/`set_assignment_store` never called, that the
    // ledger's lease machinery has no way to supervise or reap.
    let initial = ledger_store.list_for_node(&instance_id, &cfg.local_node_id)?;

    router::register(&cfg.mesh, cfg.instance_id.clone(), Arc::clone(&manager)).await;

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

    // `svc` was resolved by `cfg.instance_id` at the very top of this
    // function — THIS manager's own engine, never `bus::global()`
    // (`ReplicationInitConfig::instance_id`'s doc explains why that would
    // be wrong here). Without `set_assignment_store`, `create_topic`'s
    // assignment-proposal block (`bus::mod`'s `self.assignment_store()`)
    // is permanently `None` on every production node — `set_replication`
    // alone wires the coordinator (role/preflight/snapshot), not the store
    // `create_topic` proposes new partition assignments into. A live
    // krytyk pass on M2 found the registry never got its first row on a
    // real cluster even after the `local_node_id` bootstrap fix landed,
    // because this call was simply missing.
    svc.set_replication(Arc::clone(&manager) as Arc<dyn ReplicationCoordinator>);
    svc.set_assignment_store(ledger_store.clone());

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
///
/// Also removes this instance from `router`'s demux table (§7 W5): a
/// stopped manager must not keep answering `Hello`/`LeoQuery` frames from
/// its now-empty registry. Parse failure here would mean this manager's
/// own `instance_id` was never a valid `BusInstanceId` to begin with — it
/// could not have been `router::register`ed in the first place, so there
/// is nothing to unregister; logged rather than panicking, since shutdown
/// must not fail partway through on a manager that was never routable.
///
/// W5 review round 2 finding 4: `router::unregister` runs BEFORE
/// `manager.shutdown()`, not after. A frame arriving for `id` after
/// removal falls into `route_stream`'s existing unknown-instance arm
/// (immediate `UnknownInstance`/zeroed reply, no different from an id that
/// was never registered).
///
/// Do NOT reorder these two. Round 3 review corrected the reason this
/// ordering matters, and the corrected reason is much stronger than the
/// one first written here: `shutdown` (`manager.rs:505-515`) does NOT
/// empty the registry — it only `take()`s `leader`/`follower` out of each
/// surviving `PartitionEntry`. So with shutdown first, a `Hello` arriving
/// in the window still finds its entry, still matches on epoch, still
/// reaches `Verdict::Accept` (`manager.rs:777`), and `follower_factory
/// .spawn` (`manager.rs:794`) starts a BRAND-NEW `FollowerRunner` on a
/// manager that was just stopped: replication feeding a disabled
/// instance's partitions, with the shutdown token already cancelled and no
/// lease watchdog to supervise it. That is follower resurrection, not the
/// mere `ASSIGNMENT_AWAIT` latency an earlier version of this comment
/// claimed — and a reader who believes the latency story will happily
/// reorder these lines while wiring `native_on_disable` in W6.
pub fn stop(manager: &Arc<ReplicationManager>) {
    match BusInstanceId::parse(manager.instance_id()) {
        // plan-app-platform §7 W6 carried-over finding #1: identity-checked
        // removal (`Arc::ptr_eq`), not a plain by-id `unregister` — see
        // `router::unregister_if_current`'s own doc for why an enable→
        // disable→enable cycle needs this.
        Ok(id) => router::unregister_if_current(&id, manager),
        Err(e) => tracing::warn!(
            error = %e,
            "replication::stop: this manager's own instance_id does not parse as a \
             BusInstanceId — it cannot have been router::register()ed, skipping unregister"
        ),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::replication::follower::FollowerStores;
    use crate::bus::replication::frames::ReplProducerMark;
    use crate::bus::topics::Acks;
    use crate::bus::ReplError;
    use crate::db::Db;
    use crate::mesh::iroh_manager::IrohMeshConfig;
    use crate::mesh::security::MeshSecurity;
    use tentaflow_bus::Partition;

    // W5 review round 2 (the reviewer's own flagged gap): `init`'s two
    // fail-hard guards — id-mismatch, missing-engine — were exercised only
    // through `tests/process_three_node_bus_failover.rs`, the one binary
    // with two deterministic PRE-EXISTING failures unrelated to either
    // guard (confirmed by re-running the same two tests against an
    // unmodified pre-W5 `4689cee55` worktree — identical failure, zero
    // code from this file present). The fail-hard path itself had never
    // executed inside a PASSING test. These two do that directly, with no
    // dependency on mesh networking actually completing a handshake or on
    // any convergence timing at all.

    /// A `PartitionProvider` whose only real behaviour is reporting a fixed
    /// `instance_id()`. Both tests below only ever reach `init`'s two
    /// guards, both of which run before `cfg.provider.partition()`/
    /// `follower_stores()`/`producer_mark_for()`/`topic_acks()` could ever
    /// be called — a fake this thin is sufficient, and means neither test
    /// needs a real `BusService`/on-disk engine to prove either guard.
    struct FakeProvider(String);
    impl PartitionProvider for FakeProvider {
        fn instance_id(&self) -> &str {
            &self.0
        }
        fn partition(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
        ) -> Result<Partition, ReplError> {
            unimplemented!("fixture: init must bail before this is ever called")
        }
        fn follower_stores(&self) -> FollowerStores {
            unimplemented!("fixture: init must bail before this is ever called")
        }
        fn producer_mark_for(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _base_offset: u64,
        ) -> Option<ReplProducerMark> {
            unimplemented!("fixture: init must bail before this is ever called")
        }
        fn topic_acks(&self, _org: &str, _topic: &str) -> Option<Acks> {
            unimplemented!("fixture: init must bail before this is ever called")
        }
    }

    /// Loopback, discovery-disabled `IrohMeshManager` — `cfg.mesh` is a
    /// required `Arc<IrohMeshManager>` field, but neither test below ever
    /// reaches the code that dials or accepts on it (both guards fire
    /// before `router::register` even sees `cfg.mesh`). Same pattern as
    /// `router.rs`'s own `make_test_mesh_manager` (private to that file's
    /// test module, so duplicated here rather than shared — same
    /// reasoning `router.rs`'s own copy documents for its source,
    /// `mesh::iroh_manager::tie_break_tests::make_manager`).
    async fn make_test_mesh_manager() -> Arc<IrohMeshManager> {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT NOT NULL DEFAULT '',
                environment TEXT
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_by TEXT,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("create tables");
        let db: DbPool = Arc::new(Db::from_connection(conn));
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let security = Arc::new(MeshSecurity::new(db, cipher).expect("security new"));
        let cfg = IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            ..Default::default()
        };
        IrohMeshManager::new(cfg, security)
            .await
            .expect("manager new")
    }

    fn empty_db() -> DbPool {
        Arc::new(Db::from_connection(
            rusqlite::Connection::open_in_memory().expect("open in-memory db"),
        ))
    }

    #[tokio::test]
    async fn init_bails_when_the_configured_instance_id_disagrees_with_the_providers_own() {
        let mesh = make_test_mesh_manager().await;
        let cfg = ReplicationInitConfig {
            db: empty_db(),
            mesh,
            instance_id: BusInstanceId::parse("tentabus-aaaaaaaa").expect("valid id"),
            local_node_id: "n1".to_string(),
            local_env: NodeEnvironment::Prod,
            // Deliberately a DIFFERENT id than `cfg.instance_id` above —
            // this is the exact drift the check exists to catch.
            provider: Arc::new(FakeProvider("tentabus-bbbbbbbb".to_string())),
            lease_check_interval: ReplicationInitConfig::DEFAULT_LEASE_CHECK_INTERVAL,
        };
        // `ReplicationManager` (the `Ok` payload) has no `Debug` impl, so
        // `Result::expect_err`/`unwrap_err` (both require `T: Debug`) do
        // not typecheck here — match instead.
        let err = match init(cfg).await {
            Err(e) => e,
            Ok(_) => panic!(
                "init must refuse to start when cfg.instance_id and cfg.provider.instance_id() disagree"
            ),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("tentabus-aaaaaaaa") && msg.contains("tentabus-bbbbbbbb"),
            "error must name BOTH disagreeing ids so the caller can tell which config built \
             this mismatch: {msg}"
        );
    }

    #[tokio::test]
    async fn init_bails_when_no_engine_is_running_for_the_configured_instance() {
        // A valid, freshly-invented id that no other test in this binary
        // ever calls `bus::init_instance`/`bus::init` for (every other
        // fixture in this crate's `bus::`/`bus::replication::` tests uses
        // "tentabus-00000001" — chosen here specifically to avoid that
        // shared id and any risk of colliding with a real registration a
        // concurrently running test made).
        const ID: &str = "tentabus-de000001";
        let mesh = make_test_mesh_manager().await;
        let cfg = ReplicationInitConfig {
            db: empty_db(),
            mesh,
            instance_id: BusInstanceId::parse(ID).expect("valid id"),
            local_node_id: "n1".to_string(),
            local_env: NodeEnvironment::Prod,
            provider: Arc::new(FakeProvider(ID.to_string())),
            lease_check_interval: ReplicationInitConfig::DEFAULT_LEASE_CHECK_INTERVAL,
        };
        // Defensive: this id must genuinely be unregistered before the
        // assertion below means anything.
        assert!(
            crate::bus::instance(&BusInstanceId::parse(ID).expect("valid id")).is_none(),
            "test setup bug: '{ID}' must not already be a running engine"
        );
        let err = match init(cfg).await {
            Err(e) => e,
            Ok(_) => {
                panic!("init must refuse to start when bus::instance() has no engine for this id")
            }
        };
        assert!(
            err.to_string().contains(ID),
            "error must name the missing instance id: {err}"
        );
    }
}
