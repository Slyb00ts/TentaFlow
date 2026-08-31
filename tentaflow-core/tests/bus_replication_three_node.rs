//! In-process 3-node replication test (PLAN-M2 §1g, "3 węzły in-process").
//!
//! Three independent `BusService`s (own temp `bus_dir`/SQLite DB each,
//! mirroring three separate `TENTAFLOW_HOME`s) wired together with:
//! - a `Transport` (`bus::replication::manager::Transport`) that hands a
//!   fresh `tokio::io::duplex` pair to the dialing side and routes the other
//!   half into the target node's OWN `ReplicationManager::accept_stream`
//!   (the exact same entry point a real `ALPN_BUS` accept handler calls),
//!   plus a pre-Hello environment gate mirroring `mesh/iroh_manager.rs`'s
//!   real trust-check gate (Z12, PLAN-M2 §1b fencing point 2a);
//! - a `SharedLedger` fake implementing BOTH `AssignmentStore` and
//!   `LedgerAdmission` over one in-memory map, shared by all three nodes
//!   (standing in for the real sync ledger's eventual consistency): a
//!   successful `propose` is immediately visible to every node's own
//!   `admitted_by` poll, i.e. majority is "acks from the other two fakes"
//!   as instructed;
//! - the REAL `GlueLeaderFactory`/`GlueFollowerFactory` (agent G) over each
//!   node's own `BusService` as `PartitionProvider` — the same production
//!   wiring `replication::init` uses, just without a real `iroh` mesh
//!   underneath.
//!
//! Each node also runs two short-interval background loops mirroring
//! `bus/replication/init.rs`'s own (`spawn_lease_check_loop`,
//! `spawn_assignment_poll_loop`) but generic over the `AssignmentStore`
//! trait object instead of the concrete `SqliteLedgerAssignmentStore`, so a
//! lease-expiry election and its resulting ledger write reach every node's
//! registry within ~100 ms of wall time instead of requiring the test to
//! hand-drive `apply_assignment`/`check_leases` at every step.
//!
//! Run: `cargo test --test bus_replication_three_node -- --test-threads=1`
//!
//! ## Bug this suite found, now fixed (kept as history of the repro)
//!
//! Both scenarios below (`publish_through_leader_replicates_...`,
//! `graceful_leader_stop_promotes_...`) used to be `#[ignore]`d because
//! `ReplicationManager::accept_stream` read a stream's ONE-AND-ONLY `Hello`
//! frame itself and then handed the already-consumed stream to a follower
//! runner that read `Hello` again — so no follower accepted through the
//! production accept path could ever finish its handshake. Fixed in wave 3
//! (`FollowerRunnerFactory::spawn` now takes the `ReplHello` `accept_stream`
//! already read, and the runner continues via
//! `follower::run_follower_stream_with_hello`); the same wave fixed
//! `reassign`/`evict_node_from_replica_sets` never bumping `leader_epoch`,
//! which the ledger's own admission rule (mirrored by `SharedLedger::propose`)
//! used to silently drop. With both fixes in, `wait_for_hello_handshake` is
//! the gate that proves the accept path is live rather than assuming it — and
//! it does pass: every partition's Hello/HelloAck completes on B and C. The
//! two scenarios were then re-`#[ignore]`d for the NEXT defect, the one the
//! next section describes, and not for anything in the handshake path — and
//! they run unignored again now that it is closed.
//!
//! ## Feed-path defect this suite found, and the fix that closed it
//!
//! Until the last edit of this wave, **a leader fed a follower nothing on any
//! topic whose configured `acks` is not `leader`.** The loop closed over
//! itself:
//!
//! - `leader::feed()` read batches with
//!   `PartitionReader::fetch_raw_from_offset`, which the engine bounds at
//!   `high_watermark` (its own doc: "same bounds (`high_watermark`)"), so
//!   offsets at or above `hw` were simply not there;
//! - `GlueLeaderFactory::spawn` puts every leader partition in
//!   `HwTracking::Manual`, so the engine no longer auto-bumps `hw` to `leo`;
//! - `PartitionLeader::recompute_hw` is what moves `hw` now, and it computes
//!   `hw = nth_largest(isr_leos, required)` — i.e. only once enough replicas
//!   have ACKed up to that offset, which needs the data they were never sent.
//!
//! With `acks=leader`, `required == 1` and the leader's own `leo` satisfies it,
//! so `hw == leo` and replication worked — which is why every scenario here was
//! green on `Acks::Leader` while both quorum scenarios died on their first
//! publish, and why the unit tests (whose fake providers left partitions in the
//! `FollowLeo` default) could not see it either.
//!
//! Measured, not inferred — state dumped at the failure point on an idle-ish
//! host (load 5-7), reproducible 4/4 runs of this suite's quorum-publishing
//! tests:
//!
//! ```text
//! leader A: leo=1 hw=0    followers B/C: leo=0 hw=0
//! roles A=Leader{1} B/C=Follower{A,1}   A's live ISR = [A,B,C]   lagging=[]
//! publish -> PartialPublish{accepted:1} + AckTimeout{acked:1, required:2}
//! ```
//!
//! PLAN §4.2 puts the "never read uncommitted data" rule on the CONSUMER
//! (`high_watermark` gates reads), while §4.1 has the leader push raw batch
//! bytes and followers ack cumulatively — so the feeder was never meant to be
//! bounded by `hw`. FIXED in this wave, in the two places the circularity
//! actually lived: `tentaflow-bus` gained `PartitionReader::
//! fetch_raw_to_end_of_log`, the `log_end_offset`-bounded twin of
//! `fetch_raw_from_offset` (consumer reads stay `hw`-bounded; only a leader
//! feeding its own chain may read past it), and `leader::feed()` reads through
//! it. `a_leader_appended_record_is_fed_before_any_high_watermark_advance` in
//! `leader.rs` is the unit-level twin of scenarios 1 and 5: both batches must
//! arrive at the follower while the leader's `hw` is still 0, and the follower's
//! ACK of that uncommitted data is what commits it.
//!
//! One knob worth naming for whoever reads the history, because it was the
//! first thing reached for and it could never have worked: shrinking the ISR out
//! from under the stuck replica set does NOT unblock `quorum`. `isr_leos()` then
//! returns only the leader, but `required_for(Acks::Quorum, _)` is `min_isr`
//! (computed once from the ASSIGNMENT size, not from the live ISR), so
//! `nth_largest([leader_leo], 2)` is `0` by the `n > values.len()` rule and `hw`
//! stays at 0 forever. `Acks::Leader` (`required == 1`) and `Acks::All`
//! (`required == isr_len`) did fall out of a shrink — which is the whole reason
//! an `acks=leader` cluster looked alive but 5 seconds late while a quorum
//! cluster looked dead.
//!
//! A second, independent feeder bug was found alongside it and fixed here too.
//! The feeder was armed only by `Partition::subscribe_leo`, while `hw` is moved
//! by `recompute_hw` on ACK and ISR bookkeeping and NEVER on a local append — so
//! an `hw` advance with no append around it woke nothing, and the records below
//! it stayed unsent for good. That one bit `acks=leader` topics too: measured as
//! a leader at `hw == leo == 6` with both followers still at `leo == 0`.
//! `PartitionLeader::subscribe_hw` now wakes the feeder on a real `hw` advance —
//! load-bearing under the old bound, redundant under the new one (its doc says
//! what it is worth now and how to delete it).
//!
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use tempfile::TempDir;
use tokio::io::split;

use tentaflow_bus::{BatchBuilder, RecordInput};
use tentaflow_protocol::environment::NodeEnvironment;

use tentaflow_core::bus::replication::assignment::PartitionAssignment;
use tentaflow_core::bus::replication::follower::FollowerConfig;
use tentaflow_core::bus::replication::frames::{self, ReplFrame, ReplHello, ReplReject};
use tentaflow_core::bus::replication::glue::{
    AuditLogReplAudit, GlueFollowerFactory, GlueLeaderFactory, PartitionProvider,
};
use tentaflow_core::bus::replication::leader::LeaderConfig;
use tentaflow_core::bus::replication::manager::{
    AssignmentStore, BusRecv, BusSend, LedgerAdmission, PartitionKey, ReplicationManager,
    ReplicationManagerConfig, Transport,
};
use tentaflow_core::bus::replication::metrics::LeaderMetrics;
use tentaflow_core::bus::topics::{Acks, TopicOptions};
use tentaflow_core::bus::{
    BusAction, BusCallContext, BusInitConfig, BusService, BusServiceError, ConsumerConfig,
    PartitionRole, PublishBatch, PublishRecord, ReplError, ReplicationCoordinator, TopicPartition,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::sync::ledger::OperationId;

const ORG: &str = "org-3node";
const TOPIC: &str = "orders";

// ===== Allow-all authorizer (mirrors tests/bus_demo_seed.rs) ===============

struct AllowAllAuthorizer;

impl tentaflow_core::bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }

    fn generation(&self) -> u64 {
        0
    }
}

/// Diagnostic-only: `RUST_LOG=tentaflow_core::bus::replication=trace cargo
/// test ... -- --nocapture` to see per-frame tracing while debugging a
/// scenario. Safe to call from every test (`try_init` is a once-only no-op
/// on a second call).
fn init_tracing() {
    // Plain stderr, not `with_test_writer`: the replication code logs from
    // tokio-spawned tasks, and the test-writer's thread-local capture buffer
    // silently drops events from non-test threads — the exact threads this
    // suite's diagnostics need. Safe with `--test-threads=1`; with parallel
    // runs, expect interleaved lines.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

fn ctx() -> BusCallContext {
    BusCallContext {
        org_id: ORG.to_string(),
        actor: Some("test".to_string()),
        correlation_id: None,
        origin: "bus_replication_three_node".to_string(),
    }
}

// ===== SharedLedger: one shared AssignmentStore + LedgerAdmission =========

/// Stands in for the real sync ledger (PLAN-M2 §1c): a `propose` that is
/// admitted (monotonic epoch, or same-epoch tie-broken by lower
/// `leader_node_id` — the same rule `core_materializer::
/// apply_bus_partition_assignment` documents) immediately marks every OTHER
/// replica of that assignment as having acknowledged the resulting op, so
/// `admitted_by_majority` sees majority without this test needing to model
/// outbox delivery latency. One instance, shared (`Arc`) across all three
/// nodes' `ReplicationManagerConfig`.
///
/// ACK SEMANTICS, modeled deliberately after the REAL ledger's weakness:
/// an outbox ack means "the peer received and processed the op" — a
/// same-epoch proposal that LOSES the node-id tie-break is applied as a
/// no-op (`Ok(0)`) by the materializer and STILL acknowledged
/// (`sync/runtime.rs` marks the inbox entry applied on `Ok(0)`, and the
/// sender's outbox entry is then acknowledged). So this fake registers
/// acks for every proposal, admitted or not — which is precisely why
/// `admitted_by_majority` alone cannot decide exclusivity and the
/// manager's fences (promotion-time ledger consult + Hello-time
/// step-down) are load-bearing: two concurrent self-elections at the same
/// epoch BOTH see a majority for their own op, exactly like the real
/// 3-process chaos run measured.
struct SharedLedger {
    rows: Mutex<HashMap<PartitionKey, PartitionAssignment>>,
    acked: Mutex<HashMap<OperationId, Vec<String>>>,
    next_op: Mutex<u8>,
}

impl SharedLedger {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(HashMap::new()),
            acked: Mutex::new(HashMap::new()),
            next_op: Mutex::new(1),
        })
    }

    /// Seeds an initial assignment directly (no admission gate) — the
    /// test's stand-in for "the ledger already converged on this baseline
    /// before the test started", used once per topic/partition instead of
    /// driving a real `create_topic` cross-node placement round trip.
    fn seed(&self, a: PartitionAssignment) {
        let key = (a.org_id.clone(), a.topic.clone(), a.partition);
        self.rows.lock().insert(key, a);
    }
}

impl AssignmentStore for SharedLedger {
    fn get(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError> {
        Ok(self
            .rows
            .lock()
            .get(&(org.to_string(), topic.to_string(), partition))
            .cloned())
    }

    fn list_for_topic(
        &self,
        org: &str,
        topic: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        Ok(self
            .rows
            .lock()
            .values()
            .filter(|a| a.org_id == org && a.topic == topic)
            .cloned()
            .collect())
    }

    fn list_for_node(&self, node_id: &str) -> Result<Vec<PartitionAssignment>, ReplError> {
        Ok(self
            .rows
            .lock()
            .values()
            .filter(|a| a.replicas.iter().any(|r| r == node_id))
            .cloned()
            .collect())
    }

    fn propose(&self, assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
        let key = (
            assignment.org_id.clone(),
            assignment.topic.clone(),
            assignment.partition,
        );
        let mut rows = self.rows.lock();
        let admitted = match rows.get(&key) {
            None => true,
            Some(stored) => {
                assignment.leader_epoch > stored.leader_epoch
                    || (assignment.leader_epoch == stored.leader_epoch
                        && assignment.leader_node_id < stored.leader_node_id)
            }
        };
        let mut n = self.next_op.lock();
        let id = OperationId::from_hash([*n; 32]);
        *n = n.wrapping_add(1).max(1);
        if admitted {
            rows.insert(key, assignment.clone());
        }
        drop(rows);
        // "majority = acks from the other two fakes": every replica of the
        // assignment is immediately reported as acknowledged for this op —
        // ADMITTED OR NOT (see the ACK SEMANTICS note on the struct: the
        // real ledger acks a gate-rejected assignment op too, which is why
        // majority alone cannot make a promotion exclusive).
        self.acked.lock().insert(id, assignment.replicas.clone());
        Ok(id)
    }
}

impl LedgerAdmission for SharedLedger {
    fn admitted_by(&self, op_id: OperationId) -> Vec<String> {
        self.acked.lock().get(&op_id).cloned().unwrap_or_default()
    }
}

// ===== DuplexTransport: one per node, sharing a peer registry =============

struct PeerEntry {
    manager: Arc<ReplicationManager>,
    environment: NodeEnvironment,
}

/// Shared across every node's own `DuplexTransport` — a stand-in for the
/// mesh's connection table.
struct TransportRegistry {
    peers: Mutex<HashMap<String, PeerEntry>>,
    /// Z12: streams `open_stream` refused before ever handing bytes to the
    /// target's `accept_stream` (the fake-transport equivalent of
    /// `mesh/iroh_manager.rs`'s real pre-ALPN trust/env check,
    /// PLAN-M2 §1b fencing point 2a) — a metric-like counter this test
    /// asserts on to make the rejection "visible", not just inferred from a
    /// timeout.
    env_gate_rejections: AtomicU32,
}

impl TransportRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            peers: Mutex::new(HashMap::new()),
            env_gate_rejections: AtomicU32::new(0),
        })
    }

    fn register(
        &self,
        node_id: impl Into<String>,
        manager: Arc<ReplicationManager>,
        env: NodeEnvironment,
    ) {
        self.peers.lock().insert(
            node_id.into(),
            PeerEntry {
                manager,
                environment: env,
            },
        );
    }
}

/// One node's `Transport`: dials another registered node by spawning a task
/// that feeds the target's REAL `ReplicationManager::accept_stream` (not a
/// bypass), after a pre-Hello environment gate mirroring the mesh's own
/// trust-check gate.
struct DuplexTransport {
    local_env: NodeEnvironment,
    registry: Arc<TransportRegistry>,
}

#[async_trait::async_trait]
impl Transport for DuplexTransport {
    async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
        let (manager, target_env) = {
            let peers = self.registry.peers.lock();
            let entry = peers
                .get(node_id)
                .ok_or_else(|| ReplError::Internal(format!("unknown peer '{node_id}'")))?;
            (Arc::clone(&entry.manager), entry.environment)
        };
        if target_env != self.local_env {
            self.registry
                .env_gate_rejections
                .fetch_add(1, Ordering::SeqCst);
            return Err(ReplError::Internal(format!(
                "mesh-level trust check: environment mismatch ({:?} dialing {:?})",
                self.local_env, target_env
            )));
        }
        let (a, b) = tokio::io::duplex(4 * 1024 * 1024);
        let (our_r, our_w) = split(a);
        let (their_r, their_w) = split(b);
        tokio::spawn(async move {
            manager
                .accept_stream(
                    "test-peer".to_string(),
                    Box::new(their_r),
                    Box::new(their_w),
                )
                .await;
        });
        Ok((Box::new(our_r), Box::new(our_w)))
    }
}

// ===== TestNode ==============================================================

struct TestNode {
    id: String,
    svc: Arc<BusService>,
    manager: Arc<ReplicationManager>,
    ledger: Arc<SharedLedger>,
    _bus_dir: TempDir,
    _db_dir: TempDir,
}

/// Fast-but-not-instant config so a 3-node scenario (Hello, ISR warm-up,
/// lease expiry, election, majority) fits comfortably inside 60 s while
/// still exercising every real timer, not a stubbed-out one.
fn leader_config() -> LeaderConfig {
    LeaderConfig {
        heartbeat_interval: Duration::from_millis(50),
        offsets_coalesce_interval: Duration::from_millis(50),
        replica_lag_max_bytes: 64 * 1024 * 1024,
        replica_lag_max_ms: 5_000,
        batch_fetch_max_bytes: 1024 * 1024,
    }
}

fn follower_config() -> FollowerConfig {
    FollowerConfig {
        ack_every_n_batches: 1,
        ack_interval: Duration::from_millis(30),
        leader_lease: Duration::from_millis(400),
    }
}

const LEASE_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const ASSIGNMENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn build_node(
    id: &str,
    env: NodeEnvironment,
    ledger: Arc<SharedLedger>,
    registry: Arc<TransportRegistry>,
) -> TestNode {
    let bus_dir = tempfile::tempdir().expect("bus_dir");
    let db_dir = tempfile::tempdir().expect("db_dir");
    let db_path = db_dir.path().join("tentaflow.db");
    let db: DbPool = tentaflow_core::db::init(&db_path).expect("db init");
    tentaflow_core::services::environment::set_node_environment(&db, env)
        .expect("set_node_environment");

    let svc = Arc::new(
        BusService::new(BusInitConfig {
            bus_dir: bus_dir.path().to_path_buf(),
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: Duration::from_secs(5),
        })
        .expect("BusService::new"),
    );

    let provider: Arc<dyn PartitionProvider> = Arc::clone(&svc) as Arc<dyn PartitionProvider>;
    let transport: Arc<dyn Transport> = Arc::new(DuplexTransport {
        local_env: env,
        registry: Arc::clone(&registry),
    });
    let metrics = Arc::new(LeaderMetrics::new());
    let leader_factory = Arc::new(GlueLeaderFactory::new(
        id,
        env,
        Arc::clone(&provider),
        Arc::clone(&transport),
        leader_config(),
        metrics,
    ));
    let follower_factory = Arc::new(GlueFollowerFactory::new(
        id,
        env,
        Arc::clone(&provider),
        follower_config(),
    ));
    let audit = Arc::new(AuditLogReplAudit::new(db.clone(), id));

    let manager = ReplicationManager::new(ReplicationManagerConfig {
        local_node_id: id.to_string(),
        local_env: env,
        transport,
        ledger: Arc::clone(&ledger) as Arc<dyn LedgerAdmission>,
        assignments: Arc::clone(&ledger) as Arc<dyn AssignmentStore>,
        leader_factory,
        follower_factory,
        audit,
        leo_query_timeout: Duration::from_millis(150),
        majority_await_timeout: Duration::from_millis(500),
    });

    svc.set_replication(Arc::clone(&manager) as Arc<dyn ReplicationCoordinator>);
    registry.register(id, Arc::clone(&manager), env);

    TestNode {
        id: id.to_string(),
        svc,
        manager,
        ledger,
        _bus_dir: bus_dir,
        _db_dir: db_dir,
    }
}

/// Mirrors `bus/replication/init.rs`'s `spawn_lease_check_loop` +
/// `spawn_assignment_poll_loop`, generic over `Arc<dyn AssignmentStore>`
/// instead of the concrete `SqliteLedgerAssignmentStore` (this test has no
/// SQLite-backed ledger to poll) — short intervals so a real election
/// reaches every node within a couple hundred ms of wall time.
fn spawn_background_loops(node: &TestNode) {
    let manager = Arc::clone(&node.manager);
    let shutdown = manager.shutdown_token();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(LEASE_CHECK_INTERVAL);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => manager.check_leases().await,
            }
        }
    });

    let manager = Arc::clone(&node.manager);
    let ledger = Arc::clone(&node.ledger);
    let local_node_id = node.id.clone();
    let shutdown = manager.shutdown_token();
    tokio::spawn(async move {
        let mut known: HashMap<PartitionKey, (u32, i64)> = HashMap::new();
        let mut ticker = tokio::time::interval(ASSIGNMENT_POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    let Ok(rows) = ledger.list_for_node(&local_node_id) else { continue };
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

fn assignment(replicas: &[&str], leader: &str, epoch: u32, partition: u32) -> PartitionAssignment {
    PartitionAssignment {
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition,
        leader_node_id: leader.to_string(),
        replicas: replicas.iter().map(|s| s.to_string()).collect(),
        isr: replicas.iter().map(|s| s.to_string()).collect(),
        leader_epoch: epoch,
        updated_at_ms: 0,
    }
}

/// Builds a 3-node cluster (A/B/C, all `Prod`), a topic `t` with
/// `partitions` partitions, RF=3, `acks`, leader=A for every partition —
/// applied directly to each node's registry (bypassing the ledger's own
/// `propose` admission for this INITIAL assignment, then seeding it into the
/// shared ledger so a LATER election's `propose` has the right baseline
/// epoch to beat). Does NOT wait for the B/C Hello handshake to finish —
/// scenarios that need replication to be live (anything publishing with
/// `acks=quorum`, or that depends on `preflight`'s live-ISR gate) must call
/// `wait_for_hello_handshake` first.
async fn build_cluster(
    partitions: u32,
    acks: Acks,
) -> (Vec<TestNode>, Arc<SharedLedger>, Arc<TransportRegistry>) {
    init_tracing();
    let ledger = SharedLedger::new();
    let registry = TransportRegistry::new();
    let a = build_node(
        "A",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    let b = build_node(
        "B",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    let c = build_node(
        "C",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    let nodes = vec![a, b, c];

    for node in &nodes {
        node.svc
            .create_topic(
                &ctx(),
                TOPIC,
                TopicOptions {
                    partitions: Some(partitions),
                    replication_factor: Some(3),
                    acks: Some(acks),
                    ..Default::default()
                },
            )
            .expect("create_topic");
    }

    for p in 0..partitions {
        let a0 = assignment(&["A", "B", "C"], "A", 1, p);
        ledger.seed(a0.clone());
        for node in &nodes {
            node.manager.apply_assignment(a0.clone()).await;
        }
    }

    for node in &nodes {
        spawn_background_loops(node);
    }

    (nodes, ledger, registry)
}

/// Waits for the initial Hello handshake to be complete on BOTH ends of
/// every partition — the suite's gate for "replication is genuinely live
/// through the production accept path", which every scenario that publishes
/// must wait on first.
///
/// Two independent halves, both required:
/// - `Partition::leader_epoch() == 1` on B and C: written by the FOLLOWER's
///   own `Hello` handling, so it proves the real
///   `ReplicationManager::accept_stream` -> `FollowerRunnerFactory::spawn`
///   round trip (the one `glue.rs`'s unit tests bypass by calling `spawn`
///   directly) completed, and a timeout here means it never did;
/// - `snapshot()`'s per-partition `isr` on A containing B and C: the
///   LEADER's live ISR (`LeaderHandle::isr` via `PartitionLeader::
///   isr_members`), which only gains a replica once A itself has processed
///   that replica's `HelloAck`. Without this half a scenario could publish
///   while `preflight` still sees `{A}` alone and refuse with
///   `NotEnoughReplicas` — a race, not a bug.
async fn wait_for_hello_handshake(nodes: &[TestNode], partitions: u32, timeout: Duration) -> bool {
    let a = find_node(nodes, "A");
    let b = find_node(nodes, "B");
    let c = find_node(nodes, "C");
    let describe_state = |partitions: u32| -> String {
        let mut state = Vec::new();
        let leader_isr = a.manager.snapshot(ORG, Some(TOPIC)).partitions;
        for p in 0..partitions {
            let b_epoch = PartitionProvider::partition(b.svc.as_ref(), ORG, TOPIC, p)
                .map(|part| part.leader_epoch());
            let c_epoch = PartitionProvider::partition(c.svc.as_ref(), ORG, TOPIC, p)
                .map(|part| part.leader_epoch());
            let isr = leader_isr
                .iter()
                .find(|info| info.partition == p)
                .map(|info| format!("{:?}", info.isr))
                .unwrap_or_else(|| "<no entry>".to_string());
            state.push(format!(
                "p{p}: b_epoch={b_epoch:?} c_epoch={c_epoch:?} a_isr={isr}"
            ));
        }
        state.join("; ")
    };
    let ok = wait_until(timeout, || {
        let leader_isr = a.manager.snapshot(ORG, Some(TOPIC)).partitions;
        (0..partitions).all(|p| {
            PartitionProvider::partition(b.svc.as_ref(), ORG, TOPIC, p)
                .map(|part| part.leader_epoch() == 1)
                .unwrap_or(false)
                && PartitionProvider::partition(c.svc.as_ref(), ORG, TOPIC, p)
                    .map(|part| part.leader_epoch() == 1)
                    .unwrap_or(false)
                && leader_isr
                    .iter()
                    .any(|info| info.partition == p && info.isr.iter().any(|m| m == "B"))
                && leader_isr
                    .iter()
                    .any(|info| info.partition == p && info.isr.iter().any(|m| m == "C"))
        })
    })
    .await;
    if !ok {
        eprintln!(
            "hello handshake incomplete after {timeout:?}: {} | roles: a={:?} b={:?} c={:?}",
            describe_state(partitions),
            a.manager.role(ORG, TOPIC, 0),
            b.manager.role(ORG, TOPIC, 0),
            c.manager.role(ORG, TOPIC, 0),
        );
    }
    ok
}

async fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn find_node<'a>(nodes: &'a [TestNode], id: &str) -> &'a TestNode {
    nodes.iter().find(|n| n.id == id).expect("node exists")
}

async fn publish_text(
    node: &TestNode,
    partition: Option<u32>,
    payload: &str,
) -> Result<tentaflow_core::bus::PublishResult, BusServiceError> {
    node.svc
        .publish_async(
            &ctx(),
            TOPIC,
            PublishBatch {
                partition,
                producer: None,
                records: vec![PublishRecord {
                    key: None,
                    headers: vec![],
                    payload: Bytes::from(payload.to_string()),
                    timestamp_ms: 0,
                    schema_id: 0,
                }],
            },
        )
        .await
}

/// Reads every record's raw payload directly off a node's own engine
/// partition (`PartitionProvider::partition`, not `BusService::peek` —
/// `peek` is leader-only, and this must also work on a follower to prove
/// byte-identical replication).
fn read_all_payloads(node: &TestNode, partition: u32) -> Vec<Vec<u8>> {
    let part = PartitionProvider::partition(node.svc.as_ref(), ORG, TOPIC, partition)
        .expect("partition handle");
    part.open_reader()
        .fetch_from_offset(0, 16 * 1024 * 1024)
        .expect("fetch_from_offset")
        .into_iter()
        .flat_map(|b| {
            b.records()
                .map(|r| r.unwrap().payload.to_vec())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The group's committed offset in THIS node's own local offset store. Read
/// through `PartitionProvider::follower_stores` rather than any service call:
/// on a follower the `open_consumer`/`peek`/`commit` paths are all
/// leader-gated, so the store itself is the only way to see what replication
/// actually landed there.
fn committed_group_offset(node: &TestNode, group: &str, partition: u32) -> u64 {
    PartitionProvider::follower_stores(node.svc.as_ref())
        .offsets
        .committed_offset(ORG, group, TOPIC, partition)
        .expect("committed_offset")
}

fn log_end_offset(node: &TestNode, partition: u32) -> u64 {
    PartitionProvider::partition(node.svc.as_ref(), ORG, TOPIC, partition)
        .expect("partition handle")
        .log_end_offset()
}

fn high_watermark(node: &TestNode, partition: u32) -> u64 {
    PartitionProvider::partition(node.svc.as_ref(), ORG, TOPIC, partition)
        .expect("partition handle")
        .high_watermark()
}

fn shutdown_all(nodes: &[TestNode]) {
    for n in nodes {
        n.manager.shutdown();
    }
}

// ===== Scenario 1: quorum publish replicates byte-identical, hw follows ===

/// The suite's end-to-end proof that the production accept path replicates:
/// B and C handshakes complete through the REAL
/// `ReplicationManager::accept_stream` (not `glue.rs`'s `spawn`-direct
/// bypass), a `acks=quorum` publish through A reaches both followers
/// byte-identically, and `hw` follows once a majority has acked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_through_leader_replicates_byte_identical_and_hw_follows() {
    let (nodes, _ledger, _registry) = build_cluster(2, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 2, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake for every partition"
    );

    for i in 0..20u32 {
        let res = publish_text(a, Some(0), &format!("msg-{i}"))
            .await
            .unwrap_or_else(|e| panic!("publish {i} failed: {e}"));
        assert_eq!(res.accepted, 1);
    }

    assert!(
        wait_until(Duration::from_secs(10), || log_end_offset(b, 0) >= 20
            && log_end_offset(c, 0) >= 20)
        .await,
        "followers never caught up: b_leo={} c_leo={}",
        log_end_offset(b, 0),
        log_end_offset(c, 0)
    );

    let leader_payloads = read_all_payloads(a, 0);
    let b_payloads = read_all_payloads(b, 0);
    let c_payloads = read_all_payloads(c, 0);
    assert_eq!(leader_payloads.len(), 20);
    assert_eq!(leader_payloads, b_payloads, "B must be byte-identical to A");
    assert_eq!(leader_payloads, c_payloads, "C must be byte-identical to A");

    // hw follows: with acks=quorum and 3-way ISR, hw must reach leo on the
    // leader once at least a majority (2 of 3) has acked every record.
    assert!(
        wait_until(Duration::from_secs(5), || high_watermark(a, 0) >= 20).await,
        "leader hw never caught up to leo: hw={}",
        high_watermark(a, 0)
    );

    shutdown_all(&nodes);
}

// ===== Scenario 2: NotLeader on publish/open_consumer at a follower =======

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_and_open_consumer_on_a_follower_return_not_leader() {
    // `Acks::Leader` — this scenario only needs B's ROLE to be `Follower`
    // (set synchronously by `apply_assignment`), so no Hello handshake is
    // required here and no replication has to be live.
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let b = find_node(&nodes, "B");

    // `publish` on a follower and the consume side below must report the
    // SAME error for the SAME condition: `map_repl_error` re-queries
    // `coordinator.role(...)` for the `NoAssignment` that
    // `ReplicationCoordinator::preflight` returns for every non-leader role
    // and turns it into `NotLeader` with the current leader's node id/epoch,
    // exactly like `check_leader_role` does for
    // `open_consumer`/`fetch`/`commit`/`peek`.
    let err = publish_text(b, Some(0), "should-fail").await.unwrap_err();
    assert!(
        matches!(err, BusServiceError::NotLeader { leader_node_id: Some(ref leader), leader_epoch: 1 } if leader == "A"),
        "expected NotLeader naming leader A/epoch 1, got {err:?}"
    );

    let result = b.svc.open_consumer(
        &ctx(),
        "group-1",
        &[TOPIC.to_string()],
        ConsumerConfig {
            commit_mode: tentaflow_core::bus::groups::CommitMode::Explicit,
        },
    );
    let err = match result {
        Ok(_) => panic!("expected NotLeader from open_consumer, got Ok(ConsumerHandle)"),
        Err(e) => e,
    };
    assert!(
        matches!(err, BusServiceError::NotLeader { .. }),
        "expected NotLeader from open_consumer, got {err:?}"
    );

    shutdown_all(&nodes);
}

// ===== Scenario 3: NotEnoughReplicas when both followers are stopped =====

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_refuses_with_not_enough_replicas_once_both_followers_are_down() {
    // `Acks::Leader` — `preflight`'s `NotEnoughReplicas` gate below is
    // driven entirely by `assignment.isr`/`replicas.len()`, never by the
    // topic's own `acks` value, so this scenario does not need (and does
    // not depend on) the reported Hello-handshake bug either.
    let (nodes, ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    b.manager.shutdown();
    c.manager.shutdown();

    // This test reaches `NotEnoughReplicas` by seeding the ledger directly
    // rather than by shrinking the replica set through the admin `reassign`
    // action, so the below-min_isr state is landed in ONE assignment round
    // trip instead of also depending on `replica_lag_max_ms` ack-staleness
    // (or on A observing B's and C's streams close) to drain the live ISR:
    // replicas `{A, D, E}` (D/E never seen before), `isr := old_isr({A,B,C})
    // ∩ new_replicas({A,D,E}) = {A}`, below `min_isr_required(3) = 2`.
    // `SharedLedger::propose`'s admission gate (the materializer's own rule:
    // strictly higher epoch, or same epoch with a lower `leader_node_id`) is
    // what makes the bumped epoch 2 below mandatory — a same-epoch
    // reassignment would be silently dropped.
    ledger.seed(PartitionAssignment {
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "A".to_string(),
        replicas: vec!["A".to_string(), "D".to_string(), "E".to_string()],
        isr: vec!["A".to_string()],
        leader_epoch: 2,
        updated_at_ms: 0,
    });

    assert!(
        wait_until(Duration::from_secs(5), || {
            let snap = a.manager.snapshot(ORG, Some(TOPIC));
            snap.partitions
                .first()
                .map(|p| p.isr.len() < 2)
                .unwrap_or(false)
        })
        .await,
        "leader never materialized the reassigned (under-replicated) assignment"
    );

    let err = publish_text(a, Some(0), "no-quorum").await.unwrap_err();
    assert!(
        matches!(err, BusServiceError::NotEnoughReplicas { .. }),
        "expected NotEnoughReplicas, got {err:?}"
    );

    a.manager.shutdown();
}

// ===== Scenario 4: Z12 — environment mismatch rejected both ways =========

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn z12_environment_mismatch_is_rejected_by_the_transport_gate_and_by_hello() {
    // ---- (a) fake-transport-level gate (mesh trust-check equivalent) ----
    let ledger = SharedLedger::new();
    let registry = TransportRegistry::new();
    let prod = build_node(
        "PROD",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    let test_node = build_node(
        "TEST",
        NodeEnvironment::Test,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );

    let transport = DuplexTransport {
        local_env: NodeEnvironment::Prod,
        registry: Arc::clone(&registry),
    };
    let before = registry.env_gate_rejections.load(Ordering::SeqCst);
    let result = transport.open_stream("TEST").await;
    assert!(
        result.is_err(),
        "a Prod node dialing a Test node must be rejected before any bytes are exchanged"
    );
    assert_eq!(
        registry.env_gate_rejections.load(Ordering::SeqCst),
        before + 1,
        "the pre-Hello environment gate rejection must be visible/countable (metric-equivalent)"
    );

    // ---- (b) Hello-level check inside `accept_stream` itself ----
    // Independent gate, exercised directly (bypassing this test's own
    // transport-level gate above) so it is proven on its own merits, not
    // merely "unreachable because (a) already blocked it" — PLAN-M2 §1b:
    // "Dwie bramki, żadna nie polega na drugiej".
    let (a_side, b_side) = tokio::io::duplex(64 * 1024);
    let (mut leader_recv, mut leader_send) = split(a_side);
    let (follower_recv, follower_send) = split(b_side);

    let accept_task = tokio::spawn({
        let manager = Arc::clone(&test_node.manager);
        async move {
            manager
                .accept_stream(
                    "prod-peer".to_string(),
                    Box::new(follower_recv),
                    Box::new(follower_send),
                )
                .await;
        }
    });

    frames::write_frame(
        &mut leader_send,
        &ReplFrame::Hello(ReplHello {
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: 0,
            leader_node_id: "PROD".to_string(),
            leader_epoch: 1,
            replicas: vec!["PROD".to_string(), "TEST".to_string()],
            environment: NodeEnvironment::Prod, // mismatches TEST's own local_env
        }),
    )
    .await
    .expect("write Hello");

    match frames::read_frame(&mut leader_recv)
        .await
        .expect("read HelloAck")
    {
        ReplFrame::HelloAck(ack) => {
            assert!(
                !ack.accepted,
                "HelloAck must reject an environment mismatch"
            );
            assert_eq!(ack.environment, NodeEnvironment::Test);
            assert_eq!(
                ack.reject,
                Some(ReplReject::EnvironmentMismatch {
                    theirs: NodeEnvironment::Prod,
                    ours: NodeEnvironment::Test,
                }),
                "reject reason must name both sides' environments (UI/log visibility)"
            );
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }
    accept_task
        .await
        .expect("accept_stream task must not panic");

    prod.manager.shutdown();
    test_node.manager.shutdown();
}

// ===== Scenario 5: graceful leader stop -> lease expiry -> election ->====
// ===== promotion with majority -> publish through the new leader =========

/// The module doc's graceful-stop case (PLAN-M2 §1g): A's leader lease stops
/// being refreshed, B or C's `check_leases` watchdog starts a real election,
/// the ledger majority admits it at a bumped epoch, and the promoted node is
/// again able to accept `acks=quorum` writes as the new leader — i.e. a
/// failover that leaves the partition write-refusing is a failure here, not a
/// documented gap.
///
/// Both halves are asserted here. This scenario used to stop at its first
/// publish (the module doc's feed-path defect), which left its election half
/// covered only indirectly — by
/// `committed_consumer_offsets_propagate_to_followers_and_survive_promotion`
/// driving the same lease-expiry -> `LeoQuery` -> majority-propose ->
/// epoch-bump promotion on an `acks=leader` cluster. With the feeder reading
/// past `hw`, the quorum version is testable, and it is the harder claim: a
/// failover must leave the partition able to ACCEPT `acks=quorum` writes, not
/// merely able to read what an `acks=leader` chain already committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_leader_stop_promotes_a_follower_with_majority_and_bumps_epoch() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Quorum).await;
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );
    for i in 0..5u32 {
        publish_text(find_node(&nodes, "A"), Some(0), &format!("pre-{i}"))
            .await
            .unwrap_or_else(|e| panic!("pre-failover publish {i} failed: {e}"));
    }
    assert!(
        wait_until(Duration::from_secs(5), || {
            log_end_offset(find_node(&nodes, "B"), 0) >= 5
                && log_end_offset(find_node(&nodes, "C"), 0) >= 5
        })
        .await,
        "followers never caught up before the graceful stop"
    );

    // Graceful stop of A (module doc's own distinction from `kill -9`,
    // unreachable in-process — PLAN-M2 §1g).
    find_node(&nodes, "A").manager.shutdown();

    // B and/or C's `leader_lease` (400 ms) expires with no more heartbeats
    // from A; `check_leases` (100 ms tick) starts an election on each.
    assert!(
        wait_until(Duration::from_secs(10), || {
            matches!(
                find_node(&nodes, "B").manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { epoch } if epoch > 1
            ) || matches!(
                find_node(&nodes, "C").manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { epoch } if epoch > 1
            )
        })
        .await,
        "no follower was promoted to leader with a bumped epoch within budget: B={:?} C={:?}",
        find_node(&nodes, "B").manager.role(ORG, TOPIC, 0),
        find_node(&nodes, "C").manager.role(ORG, TOPIC, 0),
    );

    let new_leader_id = if matches!(
        find_node(&nodes, "B").manager.role(ORG, TOPIC, 0),
        PartitionRole::Leader { .. }
    ) {
        "B"
    } else {
        "C"
    };
    let new_leader = find_node(&nodes, new_leader_id);

    let peer_id = if new_leader_id == "B" { "C" } else { "B" };

    // A promotion is only over when the NEW leader has a live ISR again, not
    // just a role: `preflight` gates an `acks=quorum` publish on
    // `LeaderHandle::isr()` (K-M2-2), and that set gains the surviving replica
    // only once the new leader's OWN replica stream to it finishes its
    // handshake — `execute_promotion_actions` dials every replica but itself,
    // so this is the promotion's work, not the peer's poll loop.
    assert!(
        wait_until(Duration::from_secs(10), || {
            new_leader
                .manager
                .snapshot(ORG, Some(TOPIC))
                .partitions
                .first()
                .map(|p| p.isr.iter().any(|m| m == peer_id))
                .unwrap_or(false)
        })
        .await,
        "new leader {new_leader_id} never regained a quorum-capable ISR after promotion: {:?}",
        new_leader
            .manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .first()
            .map(|p| p.isr.clone()),
    );

    // Publish through the NEW leader at the topic's own `acks=quorum`.
    let publish_ctx = ctx();
    let publish_res = tokio::time::timeout(
        Duration::from_secs(5),
        new_leader.svc.publish_async(
            &publish_ctx,
            TOPIC,
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![PublishRecord {
                    key: None,
                    headers: vec![],
                    payload: Bytes::from_static(b"post-failover"),
                    timestamp_ms: 0,
                    schema_id: 0,
                }],
            },
        ),
    )
    .await
    .expect("publish through the new leader must not hang")
    .unwrap_or_else(|e| panic!("publish through the new leader {new_leader_id} must succeed: {e}"));
    assert_eq!(publish_res.accepted, 1);
    assert!(
        wait_until(Duration::from_secs(5), || log_end_offset(
            find_node(&nodes, peer_id),
            0
        ) >= 6)
        .await,
        "the post-failover record never reached the surviving replica {peer_id}: peer_leo={}",
        log_end_offset(find_node(&nodes, peer_id), 0),
    );

    shutdown_all(&nodes);
}

// ===== Scenario 6: PartitionDetached during active replication ===========

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_topic_on_the_leader_terminates_replication_streams_in_bounded_time() {
    // `Acks::Leader`, because this scenario's own property (bounded
    // termination on `delete_topic`, PLAN §4.1 A5) does not require a
    // caught-up follower. The handshake wait is still required: `preflight`
    // gates on the LIVE ISR (`min_isr_required(3) = 2`), which a partition
    // with no replica streams yet answers as `{A}` alone.
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let a = find_node(&nodes, "A");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );

    publish_text(a, Some(0), "before-delete")
        .await
        .expect("publish before delete");
    assert!(
        wait_until(Duration::from_secs(2), || log_end_offset(a, 0) >= 1).await,
        "leader's own append never landed"
    );

    a.svc.delete_topic(&ctx(), TOPIC).expect("delete_topic");

    // The leader's own feeder tasks must tear down (module doc A5: a
    // `PartitionDetached` engine error is a terminal exit, not a retry) in
    // bounded time — proven by the fact that a FRESH publish attempt (which
    // would try to reopen the now-deleted partition) fails promptly rather
    // than hanging.
    let publish_after = tokio::time::timeout(
        Duration::from_secs(5),
        publish_text(a, Some(0), "after-delete"),
    )
    .await
    .expect("publish after delete_topic must not hang");
    assert!(
        publish_after.is_err(),
        "publish to a deleted topic must fail, not silently succeed"
    );

    shutdown_all(&nodes);
}

// ===== Scenario 7: K-M2-5 consumer offsets propagate and survive a =======
// ===== promotion (a promoted follower inherits the group's commit) ======

/// The one property `ReplOffsets` exists for (PLAN-M2 §1b, K-M2-5): a group
/// that committed on the leader must NOT start over after a failover. Every
/// single hop is covered by unit tests elsewhere (`leader.rs`'s coalescing,
/// `follower.rs`'s apply + monotonicity) — none of them proves the hops are
/// connected to each other, to a real `BusService::commit` call site, or to
/// what a consumer then sees on the promoted node. That wiring is what this
/// scenario pins.
///
/// `Acks::Leader`, not `quorum`: nothing in this property depends on the ack
/// level — offset frames ride the same streams as batches either way — and
/// staying on `leader` keeps the scenario about the wiring it pins rather than
/// about quorum commit timing, which scenarios 1 and 5 already cover.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_consumer_offsets_propagate_to_followers_and_survive_promotion() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );

    for i in 0..6u32 {
        publish_text(a, Some(0), &format!("rec-{i}"))
            .await
            .expect("pre-failover publish");
    }
    // `hw` too, not just `leo`: a consumer only ever reads committed
    // records, so the fetch below needs all six committed on the leader —
    // and the promoted node's `hw` is what bounds redelivery after failover.
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(b, 0) >= 6
            && high_watermark(c, 0) >= 6
            && high_watermark(a, 0) >= 6)
        .await,
        "replication never caught up: a_hw={} b_leo={} b_hw={} c_leo={} c_hw={}",
        high_watermark(a, 0),
        log_end_offset(b, 0),
        high_watermark(b, 0),
        log_end_offset(c, 0),
        high_watermark(c, 0)
    );

    // The commit goes in through the real consumer path on the leader:
    // `ConsumerHandle::commit` is leader-gated, so nothing in this scenario
    // can write a follower's offset store locally — any offset that shows up
    // on B/C below arrived on the wire, and only there.
    const GROUP: &str = "g-offsets";
    let consumer = a
        .svc
        .open_consumer(
            &ctx(),
            GROUP,
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: tentaflow_core::bus::groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer on the leader");
    let fetched = consumer.fetch(1024 * 1024, 2_000).expect("fetch");
    assert_eq!(
        fetched.records.len(),
        6,
        "a fresh group must read from offset 0"
    );
    consumer
        .commit(&[(
            TopicPartition {
                topic: TOPIC.to_string(),
                partition: 0,
            },
            4,
        )])
        .expect("commit offset 4");

    // Bounded, never early (`offsets_coalesce_interval`, 50 ms here): both
    // followers must land the leader's commit without anyone calling them.
    for node in [b, c] {
        assert!(
            wait_until(Duration::from_secs(5), || committed_group_offset(
                node, GROUP, 0
            ) == 4)
            .await,
            "{group} commit never replicated to {node}: got {got}",
            group = GROUP,
            node = node.id,
            got = committed_group_offset(node, GROUP, 0)
        );
    }

    // Graceful stop of A, then a real election — same promotion path as
    // scenario 5.
    a.manager.shutdown();
    assert!(
        wait_until(Duration::from_secs(10), || {
            matches!(
                b.manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { epoch } if epoch > 1
            ) || matches!(
                c.manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { epoch } if epoch > 1
            )
        })
        .await,
        "no follower was promoted within budget: B={:?} C={:?}",
        b.manager.role(ORG, TOPIC, 0),
        c.manager.role(ORG, TOPIC, 0),
    );
    let new_leader = if matches!(b.manager.role(ORG, TOPIC, 0), PartitionRole::Leader { .. }) {
        b
    } else {
        c
    };

    assert_eq!(
        committed_group_offset(new_leader, GROUP, 0),
        4,
        "the promoted node must still hold the commit it inherited as a follower"
    );

    // The user-visible half: a consumer on the NEW leader resumes at the
    // inherited commit instead of replaying the log from offset 0.
    let resumed = new_leader
        .svc
        .open_consumer(
            &ctx(),
            GROUP,
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: tentaflow_core::bus::groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer on the promoted leader");
    let after_failover = resumed.fetch(1024 * 1024, 2_000).expect("fetch");
    assert_eq!(
        after_failover.records.len(),
        2,
        "exactly the two uncommitted records may be redelivered after promotion"
    );
    assert_eq!(
        after_failover.records[0].offset, 4,
        "redelivery must start at the inherited committed offset"
    );

    shutdown_all(&nodes);
}

// ===== Scenario 8: a divergent replica is truncated back to the =========
// ===== leader's authority when its stream reopens ========================

/// One single-record batch, appended straight into a node's own engine
/// partition — this test's stand-in for "records this replica has that no
/// one else ever saw", which is exactly the shape of an old leader's tail
/// after a failover it did not participate in.
fn one_text_batch(payload: &str) -> Bytes {
    let mut b = BatchBuilder::new(0, 0);
    b.push(RecordInput::new(Bytes::from(payload.to_string()), 0))
        .unwrap();
    b.build().unwrap()
}

/// K-M2-1 end to end, over the real `accept_stream` path. A leader reopening
/// a replica's stream must cut that replica back to its own `leo` when the
/// replica reports having grown past it — the case `execute_promotion_actions`
/// cannot cover, because a promotion can only `SendTruncate` down a stream it
/// had already opened (and a replica down during the election answered no
/// `LeoQuery`, so nothing was derived for it either).
///
/// Divergence is created by appending two records directly to C's partition:
/// the state under test ("a replica whose log is ahead of the chain") is what
/// a real failover produces, and constructing it through a real failover
/// would need a second promotion for every run of this test.
///
/// `Acks::Leader` for the same reason as scenario 7 — the ack level is not what
/// this property is about, and quorum replication is already pinned by
/// scenarios 1 and 5. `acks=leader` also still leaves the replica's own `hw`
/// behind the divergent tail (a direct local append does not bump `hw` on a
/// `HwTracking::Manual` partition), which is what makes the truncate legal
/// under K-M2-1 here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replica_ahead_of_the_leader_is_truncated_back_when_its_stream_reopens() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );
    for i in 0..5u32 {
        publish_text(a, Some(0), &format!("chain-{i}"))
            .await
            .expect("publish to the chain");
    }
    // `hw`, not just `leo`: the truncate below is only legal at or above the
    // replica's own `hw` (K-M2-1), so the chain has to be fully committed on
    // C before this test starts adding an un-committed tail to it.
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(b, 0) >= 5
            && high_watermark(c, 0) >= 5)
        .await,
        "followers never caught up and committed: b_leo={} b_hw={} c_leo={} c_hw={}",
        log_end_offset(b, 0),
        high_watermark(b, 0),
        log_end_offset(c, 0),
        high_watermark(c, 0)
    );
    let chain = read_all_payloads(a, 0);

    // C grows past the chain; B and A stay at 5.
    let c_part = PartitionProvider::partition(c.svc.as_ref(), ORG, TOPIC, 0).expect("C partition");
    c_part
        .append_batch_async(one_text_batch("ghost-1"))
        .await
        .expect("append divergent record 1");
    c_part
        .append_batch_async(one_text_batch("ghost-2"))
        .await
        .expect("append divergent record 2");
    assert_eq!(log_end_offset(c, 0), 7, "C must be ahead of the chain");
    assert_eq!(high_watermark(c, 0), 5, "the divergent tail is uncommitted");

    // A reopened stream — a new epoch is what a `reassign`/promotion lands in
    // the ledger, and `apply_assignment` rebuilds the leader's replica
    // streams for it (the same path `apply_assignment_leader_dials_...`
    // covers).
    a.manager
        .apply_assignment(assignment(&["A", "B", "C"], "A", 2, 0))
        .await;

    assert!(
        wait_until(Duration::from_secs(10), || log_end_offset(c, 0) == 5).await,
        "C was never truncated back to the leader's authority: c_leo={} c_hw={}",
        log_end_offset(c, 0),
        high_watermark(c, 0),
    );
    assert_eq!(
        read_all_payloads(c, 0),
        chain,
        "the divergent tail must be gone and the chain records kept"
    );
    assert_eq!(
        high_watermark(c, 0),
        5,
        "hw is monotonic (K-M2-1) — the truncate must not have moved it"
    );
    assert_eq!(
        log_end_offset(b, 0),
        5,
        "an in-sync replica must not be truncated at all"
    );

    // The resolved replica keeps replicating from the offset it was cut back
    // to, rather than diverging forever.
    for i in 0..2u32 {
        publish_text(a, Some(0), &format!("after-{i}"))
            .await
            .expect("publish after the truncate");
    }
    assert!(
        wait_until(Duration::from_secs(10), || log_end_offset(c, 0) >= 7).await,
        "C never resumed replicating past the truncated point: c_leo={}",
        log_end_offset(c, 0),
    );
    let mut expected = chain.clone();
    expected.push(b"after-0".to_vec());
    expected.push(b"after-1".to_vec());
    assert_eq!(read_all_payloads(c, 0), expected, "C must match the chain");
    assert_eq!(read_all_payloads(a, 0), expected);

    shutdown_all(&nodes);
}

// ===== Scenario 9: leader dropped WITHOUT a graceful stop — exactly one ===
// ===== winner, the loser fenced, quorum ISR re-formed, publishes resume ===

/// The in-process crash-path gate for the P8 defects the 3-process chaos run
/// measured (`tests/process_three_node_bus_failover.rs`, M2-WYNIKI "promocja
/// nie jest wyłączna"): the leader vanishes WITHOUT a graceful handoff, both
/// survivors' leases expire within the same tick, BOTH self-elect at the
/// same next epoch — and the cluster must still converge to EXACTLY ONE
/// serving leader with a quorum ISR, instead of the measured ~48 s
/// mutual-`NotAReplica` livelock (`isr=1, required=2` on both sides).
///
/// What "dropped without a graceful stop" means in-process, honestly
/// stated: a real `kill -9` closes the process's sockets and aborts every
/// task it owns in one step. The in-process equivalent is cutting the node
/// out of the transport (survivors' re-dials fail like a dead host), so its
/// live duplex streams die from the survivors' side, and dropping the
/// node's own background loops so a "crashed" node cannot keep running
/// elections. The manager-level teardown (`ReplicationManager::shutdown`)
/// aborts exactly those stream tasks — the survivors observe transport EOF
/// either way (that is the equivalence `glue.rs`'s EOF→`lease_expired` fix
/// established) — and the meta flush `shutdown` performs on the way out is
/// not observable at the replication-protocol level. The genuinely SIGKILL-
/// specific parts (no fsync of in-flight records, process-level state) are
/// the run-only process test's subject; what THIS scenario pins is the
/// concurrent-double-election resolution, which no graceful-stop scenario
/// exercises deterministically.
///
/// Asserted, in order:
/// 1. exactly one of B/C ends up `Leader` at an epoch > 1, and it is the
///    LOWEST node id ("B") — the LeoQuery pre-vote + node-id tie-break
///    (K-M2-3) must decide the winner, not proposal arrival order;
/// 2. the loser's role is `Follower` of the winner at the winner's epoch
///    (it stepped down / never stayed up, rather than the mutual-refusal
///    split);
/// 3. the winner's LIVE ISR covers the loser (the ISR re-formed — the
///    second measured failure stage: a promoted leader that never
///    regains quorum);
/// 4. a publish through the winner at the topic's `acks=quorum` succeeds,
///    reaches the loser byte-identically, and the pre-crash chain is
///    intact on both.
///
/// What each layer of the fix is pinned by: the election's deterministic
/// single-candidate path (LeoQuery answered on the accept path + the
/// `choose_candidate` node-id tie-break) is what THIS scenario observes
/// end to end; the two belt-and-braces fences (promotion-time consult of
/// the materialized ledger row; Hello-time step-down of a facing leader)
/// have their own deterministic unit tests in `manager.rs`
/// (`promotion_yields_when_the_ledger_already_settled_on_a_lower_id_
/// leader`, `a_leader_receiving_an_equal_epoch_lower_id_peers_hello_
/// fences_itself` and siblings) because the in-process harness's shared
/// ledger converges rows instantly — unlike the real per-node
/// materialization whose convergence delay is exactly what made the
/// measured failure last ~48 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crashed_leader_without_graceful_stop_leaves_exactly_one_winner_with_quorum_isr() {
    let (mut nodes, _ledger, registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );
    for i in 0..5u32 {
        publish_text(a, Some(0), &format!("pre-{i}"))
            .await
            .unwrap_or_else(|e| panic!("pre-crash publish {i} failed: {e}"));
    }
    assert!(
        wait_until(Duration::from_secs(5), || {
            log_end_offset(b, 0) >= 5 && log_end_offset(c, 0) >= 5
        })
        .await,
        "followers never caught up before the crash"
    );

    // CRASH: cut A out of the transport (survivors' re-dials fail like they
    // would against a dead host — no accept handler answers, not even with
    // a rejection) and tear the node down. A real `kill -9` aborts every
    // task the process owns and lets the OS close its sockets; the
    // in-process equivalent must do the same to the leader's stream tasks
    // (they hold the duplex halves — without this the survivors would keep
    // receiving heartbeats from a "dead" node forever), which is what the
    // manager teardown does. What is deliberately absent is everything a
    // GRACEFUL handoff would involve: no `transfer_leader`, no leader
    // resignation through the ledger, no farewell to the followers — from
    // B's and C's point of view A simply stops answering mid-stream, which
    // is the observable this scenario's election race is built on. The
    // SIGKILL-specific disk-level parts are the run-only process test's
    // subject; the concurrent-double-election resolution is this test's.
    registry.peers.lock().remove("A");
    let a_pos = nodes.iter().position(|n| n.id == "A").expect("node A");
    let crashed = nodes.remove(a_pos);
    let crash_instant = tokio::time::Instant::now();
    crashed.manager.shutdown();
    drop(crashed);
    // Give the surviving streams a beat to observe the EOF before the
    // assertions start (the lease-expiry path is timing-driven, not
    // instantaneous); everything after this point waits on real state.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");

    // (0) IMMEDIATE SERVING BUDGET — the converge-locally gate (P8, the
    // last measured blocker: the 3-process chaos run's winner spent ~40 s
    // answering `not the leader (leader is <loser>)` while its registry
    // lagged its own settled ledger row, because the author's own
    // assignment op never materialized locally and a peer's loser op had
    // won the local slot). The first publish that gets past `preflight`
    // must be quorum-ACKED within this budget of the kill. The number:
    // the deterministic bound of the in-process chain is lease expiry
    // (400 ms) + lease-check tick (100 ms) + LeoQuery round (150 ms
    // budget) + majority (instant on this ledger) + ISR handshake
    // (milliseconds) ≈ 0.7 s worst case; 2 s is ~3x that bound (host
    // scheduling bursts) and still 4x tighter than PLAN §5.2's own 8 s P8
    // gate. A run that misses this budget reproduces the measured ~40 s
    // registry-lag window, scaled down.
    //
    // Retry shape: every attempt that fails FAST (`NotLeader` /
    // `NotEnoughReplicas` — preflight refuses before any append) retries
    // immediately. The FIRST attempt that gets past preflight runs to
    // completion under the remaining budget — its quorum ack must land
    // inside it, which is exactly the property under test. No retry ever
    // follows an in-flight append, so there are no duplicate-tail
    // ambiguities in the byte-identity assertions below.
    {
        let budget = Duration::from_secs(2);
        let deadline = crash_instant + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "no quorum-acked publish within {budget:?} of the crash — the winner's \
                 serving capability did not converge immediately"
            );
            match tokio::time::timeout(remaining, publish_text(b, Some(0), "resume-0")).await {
                Ok(Ok(res)) if res.accepted == 1 => break,
                Ok(Ok(res)) => panic!("unexpected publish result: {res:?}"),
                // Failed fast before any append (not leader yet, or ISR
                // still below min_isr) — retry immediately.
                Ok(Err(_)) => tokio::time::sleep(Duration::from_millis(10)).await,
                // The budget burned while this attempt was in flight: the
                // ack did not land in time. This is the failure the budget
                // exists to catch.
                Err(_) => panic!(
                    "the first publish that passed preflight was not quorum-acked \
                     within {budget:?} of the crash — serving capability did not \
                     converge immediately"
                ),
            }
        }
    }

    // (1) Exactly one winner, and (2) the loser is a Follower of it. The
    // winner is DETERMINISTIC, not first-come: both survivors' leases
    // expire within the same tick and both run elections, the LeoQuery
    // pre-vote (K-M2-3, answered on the accept path) shows equal leos, and
    // the node-id tie-break hands the candidacy to "B" ("B" < "C") on BOTH
    // sides — so C abandons without ever proposing. A run where C ends up
    // the leader means the pre-vote/tie-break chain broke.
    let winner_id;
    let loser_id;
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let b_role = b.manager.role(ORG, TOPIC, 0);
            let c_role = c.manager.role(ORG, TOPIC, 0);
            let b_leads = matches!(b_role, PartitionRole::Leader { epoch } if epoch > 1);
            let c_leads = matches!(c_role, PartitionRole::Leader { epoch } if epoch > 1);
            if b_leads && !c_leads {
                winner_id = "B";
                loser_id = "C";
                break;
            }
            if c_leads && !b_leads {
                winner_id = "C";
                loser_id = "B";
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no exclusive winner settled within budget: B={b_role:?} C={c_role:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(
        winner_id, "B",
        "the equal-leo tie-break must elect the LOWEST node id survivor, not whoever \
         proposed first"
    );
    let (winner, loser) = (b, c);

    // (2) The loser converged onto the winner: follower of `winner_id` at
    // an epoch >= the winner's own (never a rival leader). This is a
    // WAITED-FOR state, not an instant one: the loser may still be running
    // (or about to run) its own election when the winner's leadership is
    // first observed, and its adoption of the winner's assignment rides
    // the poll loop; the convergence itself is what the fences guarantee.
    let winner_epoch = match winner.manager.role(ORG, TOPIC, 0) {
        PartitionRole::Leader { epoch } => epoch,
        other => panic!("winner {winner_id} lost leadership during settle: {other:?}"),
    };
    assert!(
        wait_until(Duration::from_secs(10), || matches!(
            loser.manager.role(ORG, TOPIC, 0),
            PartitionRole::Follower {
                ref leader_node_id,
                epoch,
            } if leader_node_id == winner_id && epoch >= winner_epoch
        ))
        .await,
        "loser {loser_id} never became a follower of {winner_id} at epoch >= {winner_epoch}: \
         final roles: winner={:?} loser={:?}",
        winner.manager.role(ORG, TOPIC, 0),
        loser.manager.role(ORG, TOPIC, 0),
    );

    // (3) The winner's LIVE ISR re-formed to include the loser (K-M2-2:
    // `preflight` gates `acks=quorum` on this set, not on the static
    // assignment). This is the assertion the chaos run's second failure
    // stage failed forever (`isr=1, required=2` on both survivors).
    assert!(
        wait_until(Duration::from_secs(10), || winner
            .manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .first()
            .map(|p| p.isr.iter().any(|m| m == loser_id))
            .unwrap_or(false))
        .await,
        "winner {winner_id} never re-formed a quorum ISR with {loser_id}: {:?}",
        winner
            .manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .first()
            .map(|p| p.isr.clone()),
    );

    // (4) Publishes resume through the winner and replicate byte-identically.
    // The record from step (0) ("resume-0") plus this one ("post-crash")
    // ride on top of the 5 pre-crash records: 7 total.
    let res = publish_text(winner, Some(0), "post-crash")
        .await
        .unwrap_or_else(|e| panic!("publish through the new leader {winner_id} failed: {e}"));
    assert_eq!(res.accepted, 1);
    assert!(
        wait_until(Duration::from_secs(5), || log_end_offset(loser, 0) >= 7).await,
        "the post-crash records never reached the fenced loser {loser_id}"
    );
    let winner_payloads = read_all_payloads(winner, 0);
    assert_eq!(winner_payloads.len(), 7);
    assert_eq!(
        read_all_payloads(loser, 0),
        winner_payloads,
        "the fenced loser must hold the winner's log byte-identically (truncated \
         to the winner's authority if it ever grew past it)"
    );

    b.manager.shutdown();
    c.manager.shutdown();
}
