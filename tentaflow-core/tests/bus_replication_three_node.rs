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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use tempfile::TempDir;
use tokio::io::split;

use tentaflow_bus::{BatchBuilder, RecordInput};
use tentaflow_protocol::environment::NodeEnvironment;

use tentaflow_core::bus::leader_balance::{
    LeaderBalanceError, MoveStatus, RequestedMove, RunContext,
};
use tentaflow_core::bus::replication::assignment::PartitionAssignment;
use tentaflow_core::bus::replication::election::LogPosition;
use tentaflow_core::bus::replication::follower::FollowerConfig;
use tentaflow_core::bus::replication::forward::ForwardedPublishHandler;
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
    UnavailableReason,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::sync::ledger::OperationId;

const ORG: &str = "org-3node";
const TOPIC: &str = "orders";
/// plan-app-platform §7 W4: a real (non-`LEGACY_SINGLE_INSTANCE`)
/// shape-valid instance id — every node's `BusService`/`SharedLedger` row
/// in this suite belongs to the same one TentaBus instance.
const TEST_INSTANCE_ID: &str = "tentabus-00000001";

// ===== Allow-all authorizer (mirrors tests/bus_demo_seed.rs) ===============

/// Allows everything, except `Produce` while `deny_produce` is set: the
/// switch a scenario flips on the node that LEADS a partition to see its
/// refusal reach a publisher on another node.
struct AllowAllAuthorizer {
    deny_produce: Arc<AtomicBool>,
    /// While set, `Consume` is refused the way a per-topic ACL row would.
    deny_consume: Arc<AtomicBool>,
}

impl tentaflow_core::bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
    ) -> Result<(), BusServiceError> {
        if action == BusAction::Produce && self.deny_produce.load(Ordering::SeqCst) {
            return Err(BusServiceError::PermissionDenied {
                action: action.as_str(),
                topic: topic.to_string(),
            });
        }
        Ok(())
    }

    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        if action == BusAction::Consume && self.deny_consume.load(Ordering::SeqCst) {
            return Err(BusServiceError::PermissionDenied {
                action: action.as_str(),
                topic: topic.to_string(),
            });
        }
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
        instance_id: tentaflow_core::bus::instance::BusInstanceId::parse(TEST_INSTANCE_ID)
            .expect("valid instance id"),
        org_id: ORG.to_string(),
        actor: Some("test".to_string()),
        actor_kind: tentaflow_core::auth::actor::ActorKind::User,
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
/// nodes, each reaching it through its own `NodeLedger`.
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
    /// `Some` while the ledger is split (`split`): every node sees and
    /// writes only its own copy, taken from `rows` when the split began,
    /// and nobody else acknowledges its ops — the sync ledger across a
    /// network partition. `heal` merges the copies back by the admission
    /// rule.
    views: Mutex<Option<HashMap<String, HashMap<PartitionKey, PartitionAssignment>>>>,
    /// A candidate whose next `propose` blocks until released
    /// (`hold_propose`) — an election paused between its `LeoQuery` round
    /// and its proposal, while another one runs.
    hold: Mutex<Option<ProposeHold>>,
    /// `Some(node)`: everything `node` proposes is lost on the way to the
    /// ledger (`drop_proposals_from`) -- no row, no acknowledgement.
    dropping: Mutex<Option<String>>,
}

struct ProposeHold {
    node: String,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// The materializer's admission rule (`core_materializer::
/// apply_bus_partition_assignment`): a strictly higher epoch, or the same
/// epoch with a lower leader node id.
fn admits(stored: Option<&PartitionAssignment>, incoming: &PartitionAssignment) -> bool {
    match stored {
        None => true,
        Some(stored) => {
            incoming.leader_epoch > stored.leader_epoch
                || (incoming.leader_epoch == stored.leader_epoch
                    && incoming.leader_node_id < stored.leader_node_id)
        }
    }
}

impl SharedLedger {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(HashMap::new()),
            acked: Mutex::new(HashMap::new()),
            next_op: Mutex::new(1),
            views: Mutex::new(None),
            hold: Mutex::new(None),
            dropping: Mutex::new(None),
        })
    }

    /// Loses every proposal `node` makes until `drop_proposals_from(None)`.
    fn drop_proposals_from(&self, node: Option<&str>) {
        *self.dropping.lock() = node.map(str::to_string);
    }

    /// Seeds an initial assignment directly (no admission gate) — the
    /// test's stand-in for "the ledger already converged on this baseline
    /// before the test started", used once per topic/partition instead of
    /// driving a real `create_topic` cross-node placement round trip.
    fn seed(&self, a: PartitionAssignment) {
        let key = (a.org_id.clone(), a.topic.clone(), a.partition);
        self.rows.lock().insert(key, a);
    }

    fn split(&self) {
        *self.views.lock() = Some(HashMap::new());
    }

    fn heal(&self) {
        let Some(views) = self.views.lock().take() else {
            return;
        };
        let mut rows = self.rows.lock();
        let mut nodes: Vec<_> = views.into_iter().collect();
        nodes.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, view) in nodes {
            for (key, row) in view {
                if admits(rows.get(&key), &row) {
                    rows.insert(key, row);
                }
            }
        }
    }

    /// Blocks `node`'s next `propose` until the returned sender fires (or is
    /// dropped); the returned receiver fires once that `propose` is waiting.
    fn hold_propose(
        &self,
        node: &str,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self.hold.lock() = Some(ProposeHold {
            node: node.to_string(),
            entered: entered_tx,
            release: release_rx,
        });
        (entered_rx, release_tx)
    }

    /// The rows `node` sees: its own copy while split, the shared map
    /// otherwise.
    fn rows_seen_by(&self, node: &str) -> HashMap<PartitionKey, PartitionAssignment> {
        let mut views = self.views.lock();
        match views.as_mut() {
            Some(views) => views
                .entry(node.to_string())
                .or_insert_with(|| self.rows.lock().clone())
                .clone(),
            None => self.rows.lock().clone(),
        }
    }

    fn propose_by(&self, node: &str, assignment: PartitionAssignment) -> OperationId {
        let hold = {
            let mut hold = self.hold.lock();
            if hold.as_ref().is_some_and(|h| h.node == node) {
                hold.take()
            } else {
                None
            }
        };
        if let Some(hold) = hold {
            let _ = hold.entered.send(());
            let _ = hold.release.recv();
        }
        let key = (
            assignment.org_id.clone(),
            assignment.topic.clone(),
            assignment.partition,
        );
        let mut n = self.next_op.lock();
        let id = OperationId::from_hash([*n; 32]);
        *n = n.wrapping_add(1).max(1);
        drop(n);
        if self.dropping.lock().as_deref() == Some(node) {
            return id;
        }
        let mut views = self.views.lock();
        let acked = match views.as_mut() {
            Some(views) => {
                let view = views
                    .entry(node.to_string())
                    .or_insert_with(|| self.rows.lock().clone());
                if admits(view.get(&key), &assignment) {
                    view.insert(key, assignment);
                }
                // Nobody is reachable across the split.
                Vec::new()
            }
            None => {
                let mut rows = self.rows.lock();
                if admits(rows.get(&key), &assignment) {
                    rows.insert(key, assignment.clone());
                }
                // "majority = acks from the other two fakes": every replica
                // of the assignment is immediately reported as acknowledged
                // for this op — ADMITTED OR NOT (see the ACK SEMANTICS note
                // on the struct: the real ledger acks a gate-rejected
                // assignment op too, which is why majority alone cannot make
                // a promotion exclusive).
                assignment.replicas.clone()
            }
        };
        drop(views);
        self.acked.lock().insert(id, acked);
        id
    }
}

/// One node's handle on the `SharedLedger`: what that node's manager and
/// assignment poll read and write through.
struct NodeLedger {
    node: String,
    shared: Arc<SharedLedger>,
}

impl AssignmentStore for NodeLedger {
    fn get(
        &self,
        instance_id: &str,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError> {
        Ok(self
            .shared
            .rows_seen_by(&self.node)
            .remove(&(org.to_string(), topic.to_string(), partition))
            .filter(|a| a.instance_id == instance_id))
    }

    fn list_for_topic(
        &self,
        instance_id: &str,
        org: &str,
        topic: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        Ok(self
            .shared
            .rows_seen_by(&self.node)
            .into_values()
            .filter(|a| a.instance_id == instance_id && a.org_id == org && a.topic == topic)
            .collect())
    }

    fn list_for_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        Ok(self
            .shared
            .rows_seen_by(&self.node)
            .into_values()
            .filter(|a| a.instance_id == instance_id && a.replicas.iter().any(|r| r == node_id))
            .collect())
    }

    fn list_for_instance(&self, instance_id: &str) -> Result<Vec<PartitionAssignment>, ReplError> {
        let mut rows: Vec<_> = self
            .shared
            .rows_seen_by(&self.node)
            .into_values()
            .filter(|a| a.instance_id == instance_id)
            .collect();
        rows.sort_by(|a, b| {
            (&a.org_id, &a.topic, a.partition).cmp(&(&b.org_id, &b.topic, b.partition))
        });
        Ok(rows)
    }

    fn preferred_leader_counts(
        &self,
        instance_id: &str,
    ) -> Result<HashMap<String, u32>, ReplError> {
        let mut counts = HashMap::new();
        for row in self.shared.rows_seen_by(&self.node).into_values() {
            if row.instance_id == instance_id {
                if let Some(preferred) = row.replicas.first() {
                    *counts.entry(preferred.clone()).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }

    fn propose(&self, assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
        Ok(self.shared.propose_by(&self.node, assignment))
    }
}

impl LedgerAdmission for NodeLedger {
    fn admitted_by(&self, op_id: OperationId) -> Vec<String> {
        self.shared
            .acked
            .lock()
            .get(&op_id)
            .cloned()
            .unwrap_or_default()
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
    /// Node pairs that cannot reach each other (`cut`): a dial between them
    /// fails the way a dial across a network partition does. Streams
    /// already open are not severed — the scenarios cut links before any
    /// stream between the pair exists.
    cut: Mutex<Vec<(String, String)>>,
    /// Directed `(from, to)` pairs: `from` cannot dial `to`, while `to` can
    /// still dial `from`.
    blocked: Mutex<Vec<(String, String)>>,
    /// Every stream opened for a forwarded publish, as (from, to), in order.
    forward_streams: Mutex<Vec<(String, String)>>,
}

impl TransportRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            peers: Mutex::new(HashMap::new()),
            env_gate_rejections: AtomicU32::new(0),
            cut: Mutex::new(Vec::new()),
            blocked: Mutex::new(Vec::new()),
            forward_streams: Mutex::new(Vec::new()),
        })
    }

    fn forwards(&self) -> Vec<(String, String)> {
        self.forward_streams.lock().clone()
    }

    fn cut(&self, a: &str, b: &str) {
        self.cut.lock().push((a.to_string(), b.to_string()));
    }

    fn block_dials(&self, from: &str, to: &str) {
        self.blocked.lock().push((from.to_string(), to.to_string()));
    }

    fn heal(&self) {
        self.cut.lock().clear();
        self.blocked.lock().clear();
    }

    fn is_cut(&self, a: &str, b: &str) -> bool {
        self.cut
            .lock()
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
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
    /// The dialing node's own id — what the accepting side sees as the
    /// stream's authenticated peer, as iroh reports the remote endpoint id.
    local_node_id: String,
    local_env: NodeEnvironment,
    registry: Arc<TransportRegistry>,
}

#[async_trait::async_trait]
impl Transport for DuplexTransport {
    /// The same path a replication stream takes — the target's
    /// `accept_stream` demuxes `ProduceForward` — recorded so a scenario can
    /// say how many forwarded publishes went where.
    async fn open_forward_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
        self.registry
            .forward_streams
            .lock()
            .push((self.local_node_id.clone(), node_id.to_string()));
        self.open_stream(node_id).await
    }

    async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
        if self.registry.is_cut(&self.local_node_id, node_id)
            || self
                .registry
                .blocked
                .lock()
                .iter()
                .any(|(from, to)| from == &self.local_node_id && to == node_id)
        {
            return Err(ReplError::Internal(format!(
                "network partition between '{}' and '{node_id}'",
                self.local_node_id
            )));
        }
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
        let peer = self.local_node_id.clone();
        tokio::spawn(async move {
            manager
                .accept_stream(peer, Box::new(their_r), Box::new(their_w))
                .await;
        });
        Ok((Box::new(our_r), Box::new(our_w)))
    }
}

// ===== TestNode ==============================================================

struct TestNode {
    id: String,
    db: DbPool,
    svc: Arc<BusService>,
    manager: Arc<ReplicationManager>,
    ledger: Arc<NodeLedger>,
    /// While set, this node's authorizer refuses `Produce`.
    deny_produce: Arc<AtomicBool>,
    /// While set, this node's authorizer refuses `Consume`.
    deny_consume: Arc<AtomicBool>,
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
        ..LeaderConfig::default()
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

/// Makes `user` a member of the suite's organisation on one node's own
/// database, as the sync of memberships does on a real cluster: a node that
/// serves a forwarded read of a user checks it there.
fn enroll_user(db: &DbPool, user: &str) {
    let conn = db.write().expect("db write");
    conn.execute(
        "INSERT OR IGNORE INTO organizations \
            (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
         VALUES (?1, 'three node', 'three-node', NULL, NULL, NULL, 'active', \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [ORG],
    )
    .expect("organization row");
    conn.execute(
        "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
         VALUES (?1, ?2, 'role-org-admin', '2026-01-01T00:00:00Z', 'test')",
        [ORG, user],
    )
    .expect("membership row");
}

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

    let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
    tentaflow_core::bus::db::migrate(&local_conn).expect("migrate local db");
    let local_db: DbPool = Arc::new(tentaflow_core::db::Db::from_connection(local_conn));
    let deny_produce = Arc::new(AtomicBool::new(false));
    let deny_consume = Arc::new(AtomicBool::new(false));
    enroll_user(&db, ctx().actor.as_deref().expect("the test actor"));
    let svc = Arc::new(
        BusService::new(BusInitConfig {
            instance_id: tentaflow_core::bus::instance::BusInstanceId::parse(TEST_INSTANCE_ID)
                .expect("valid instance id"),
            local_db,
            bus_dir: bus_dir.path().to_path_buf(),
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer {
                deny_produce: Arc::clone(&deny_produce),
                deny_consume: Arc::clone(&deny_consume),
            }),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: Duration::from_secs(5),
            settings_cipher: Some(Arc::new(tentaflow_core::crypto::SettingsCipher::new(
                &[7u8; 32],
            ))),
        })
        .expect("BusService::new"),
    );

    let provider: Arc<dyn PartitionProvider> = Arc::clone(&svc) as Arc<dyn PartitionProvider>;
    let transport: Arc<dyn Transport> = Arc::new(DuplexTransport {
        local_node_id: id.to_string(),
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
    let ledger = Arc::new(NodeLedger {
        node: id.to_string(),
        shared: ledger,
    });

    let manager = ReplicationManager::new(ReplicationManagerConfig {
        instance_id: TEST_INSTANCE_ID.to_string(),
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
    manager.set_forward_handler(Arc::downgrade(&svc) as Weak<dyn ForwardedPublishHandler>);
    registry.register(id, Arc::clone(&manager), env);

    TestNode {
        id: id.to_string(),
        db,
        svc,
        manager,
        ledger,
        deny_produce,
        deny_consume,
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
    spawn_lease_check_loop(node);
    spawn_assignment_poll_loop(node);
}

fn spawn_lease_check_loop(node: &TestNode) {
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
}

fn spawn_assignment_poll_loop(node: &TestNode) {
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
                    let Ok(rows) = ledger.list_for_node(TEST_INSTANCE_ID, &local_node_id) else {
                        continue;
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

/// The incarnation of the test topic `node` holds — what every placement of
/// it must name (`PartitionAssignment::topic_generation`).
fn topic_generation(node: &TestNode) -> u64 {
    tentaflow_core::db::repository::bus_topic_get(&node.db, TEST_INSTANCE_ID, ORG, TOPIC)
        .expect("read topic row")
        .expect("topic row")
        .generation
}

fn assignment(
    generation: u64,
    replicas: &[&str],
    leader: &str,
    epoch: u32,
    partition: u32,
) -> PartitionAssignment {
    PartitionAssignment {
        instance_id: TEST_INSTANCE_ID.to_string(),
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition,
        leader_node_id: leader.to_string(),
        replicas: replicas.iter().map(|s| s.to_string()).collect(),
        isr: replicas.iter().map(|s| s.to_string()).collect(),
        leader_epoch: epoch,
        updated_at_ms: 0,
        topic_generation: generation,
        epoch_slots: Default::default(),
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
    let cluster = build_cluster_placed(partitions, acks, &["A", "B", "C"], "A").await;
    for node in &cluster.0 {
        spawn_background_loops(node);
    }
    cluster
}

/// `build_cluster` with the epoch-1 placement given: `replicas` (a subset of
/// A/B/C) led by `leader`, which may name a node that is not there — a
/// leader already gone when the scenario starts. Spawns no background loop;
/// the scenario picks the ones it runs.
async fn build_cluster_placed(
    partitions: u32,
    acks: Acks,
    replicas: &[&str],
    leader: &str,
) -> (Vec<TestNode>, Arc<SharedLedger>, Arc<TransportRegistry>) {
    build_cluster_led_by(partitions, acks, replicas, |_| leader.to_string()).await
}

/// `build_cluster_placed` with the epoch-1 leader chosen per partition.
async fn build_cluster_led_by(
    partitions: u32,
    acks: Acks,
    replicas: &[&str],
    leader_of: impl Fn(u32) -> String,
) -> (Vec<TestNode>, Arc<SharedLedger>, Arc<TransportRegistry>) {
    let (nodes, ledger, registry) = build_unplaced_cluster(partitions, acks, replicas.len()).await;
    let generation = topic_generation(&nodes[0]);
    for p in 0..partitions {
        let a0 = assignment(generation, replicas, &leader_of(p), 1, p);
        ledger.seed(a0.clone());
        for node in &nodes {
            node.manager.apply_assignment(a0.clone()).await;
        }
    }

    (nodes, ledger, registry)
}

/// A 3-node cluster (A/B/C, all `Prod`) with the topic `t` created on A and
/// its row copied to B and C the way the ledger replicates it: one
/// incarnation, the one every placement names. Nothing is placed yet.
async fn build_unplaced_cluster(
    partitions: u32,
    acks: Acks,
    replication_factor: usize,
) -> (Vec<TestNode>, Arc<SharedLedger>, Arc<TransportRegistry>) {
    build_unplaced_cluster_withholding(partitions, acks, replication_factor, &[]).await
}

/// `build_unplaced_cluster`, but the topic row is not copied to the nodes
/// named in `withhold_from` (the test copies it later).
async fn build_unplaced_cluster_withholding(
    partitions: u32,
    acks: Acks,
    replication_factor: usize,
    withhold_from: &[&str],
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

    nodes[0]
        .svc
        .create_topic(
            &ctx(),
            TOPIC,
            TopicOptions {
                partitions: Some(partitions),
                replication_factor: Some(replication_factor as u32),
                acks: Some(acks),
                ..Default::default()
            },
        )
        .expect("create_topic");
    let row =
        tentaflow_core::db::repository::bus_topic_get(&nodes[0].db, TEST_INSTANCE_ID, ORG, TOPIC)
            .expect("read topic row")
            .expect("topic row");
    for node in &nodes[1..] {
        if withhold_from.contains(&node.id.as_str()) {
            continue;
        }
        tentaflow_core::db::repository::bus_topic_create(&node.db, &row)
            .expect("replicate topic row");
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
            let b_epoch = b
                .svc
                .local_partition(&ctx(), TOPIC, p)
                .map(|part| part.leader_epoch());
            let c_epoch = c
                .svc
                .local_partition(&ctx(), TOPIC, p)
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
            b.svc
                .local_partition(&ctx(), TOPIC, p)
                .map(|part| part.leader_epoch() == 1)
                .unwrap_or(false)
                && c.svc
                    .local_partition(&ctx(), TOPIC, p)
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
    let part = node
        .svc
        .local_partition(&ctx(), TOPIC, partition)
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
    node.svc
        .local_partition(&ctx(), TOPIC, partition)
        .expect("partition handle")
        .log_end_offset()
}

fn high_watermark(node: &TestNode, partition: u32) -> u64 {
    node.svc
        .local_partition(&ctx(), TOPIC, partition)
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
    let (nodes, _ledger, registry) = build_cluster(1, Acks::Leader).await;
    let b = find_node(&nodes, "B");
    // A publish on a follower is forwarded to the leader; with the leader
    // out of reach the forwarding cannot be done and the caller is left with
    // the `NotLeader` it always got (the forwarded path has its own
    // scenarios below).
    registry.cut("B", "A");

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

// ===== Forwarding a write to the partition leader ==========================

/// The records `node` holds for `partition` — key, payload and the headers
/// the producer set (the broker's own `tf.*` stamps are left out).
fn read_all_records(
    node: &TestNode,
    partition: u32,
) -> Vec<(Option<Vec<u8>>, Vec<u8>, Vec<(String, Vec<u8>)>)> {
    node.svc
        .local_partition(&ctx(), TOPIC, partition)
        .expect("partition handle")
        .open_reader()
        .fetch_from_offset(0, 16 * 1024 * 1024)
        .expect("fetch_from_offset")
        .into_iter()
        .flat_map(|b| {
            b.records()
                .map(|r| {
                    let r = r.unwrap();
                    let headers = r
                        .headers
                        .iter()
                        .filter(|(k, _)| !k.starts_with(b"tf."))
                        .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v.to_vec()))
                        .collect();
                    (r.key.map(|k| k.to_vec()), r.payload.to_vec(), headers)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn binary_record(key: &str, payload: Vec<u8>) -> PublishRecord {
    PublishRecord {
        key: Some(Bytes::from(key.to_string())),
        headers: vec![("trace".to_string(), Bytes::from_static(&[0xFF, 0x00, 0x7F]))],
        payload: Bytes::from(payload),
        timestamp_ms: 1_700_000_000_000,
        schema_id: 0,
    }
}

async fn publish_records(
    node: &TestNode,
    partition: Option<u32>,
    records: Vec<PublishRecord>,
) -> Result<tentaflow_core::bus::PublishResult, BusServiceError> {
    publish_records_as(node, &ctx(), partition, records).await
}

async fn publish_records_as(
    node: &TestNode,
    ctx: &BusCallContext,
    partition: Option<u32>,
    records: Vec<PublishRecord>,
) -> Result<tentaflow_core::bus::PublishResult, BusServiceError> {
    node.svc
        .publish_async(
            ctx,
            TOPIC,
            PublishBatch {
                partition,
                producer: None,
                records,
            },
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publish_on_a_follower_lands_on_the_leader_byte_for_byte() {
    let (nodes, _ledger, registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );

    let every_byte: Vec<u8> = (0..=255u8).collect();
    let result = publish_records(b, Some(0), vec![binary_record("k-1", every_byte.clone())])
        .await
        .unwrap_or_else(|e| panic!("publish on a follower failed: {e:?}"));
    assert_eq!(result.accepted, 1);
    assert_eq!(result.partitions.len(), 1);
    assert_eq!(result.partitions[0].base_offset, 0);

    let expected = vec![(
        Some(b"k-1".to_vec()),
        every_byte,
        vec![("trace".to_string(), vec![0xFF, 0x00, 0x7F])],
    )];
    assert_eq!(
        read_all_records(a, 0),
        expected,
        "the leader holds the record"
    );
    assert!(
        wait_until(Duration::from_secs(10), || log_end_offset(b, 0) >= 1
            && log_end_offset(c, 0) >= 1)
        .await,
        "the record never replicated onward"
    );
    assert_eq!(read_all_records(c, 0), expected);
    assert_eq!(
        read_all_records(b, 0),
        expected,
        "the forwarding node follows the leader like any replica"
    );
    assert_eq!(
        registry.forwards(),
        vec![("B".to_string(), "A".to_string())],
        "one forwarded request"
    );
    shutdown_all(&nodes);
}

/// A node outside the replica set (a one-replica topic written from every
/// node, as `__bus.metrics` is) hands its publish to the leader the
/// assignment names at once, instead of waiting for a role it will never get.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_from_a_node_that_holds_no_replica_lands_on_the_leader_without_a_stall() {
    let (nodes, _ledger, registry) = build_cluster_placed(1, Acks::Leader, &["A", "B"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    let a = find_node(&nodes, "A");
    let c = find_node(&nodes, "C");
    // C shares no replication stream with A, so the mesh is its only sign of
    // life from A; the in-process transport has no mesh to say so.
    c.manager.on_peer_connected("A");

    // Retried only until the first handshake is through; the call that
    // succeeds is the one timed.
    let give_up = std::time::Instant::now() + Duration::from_secs(15);
    let took = loop {
        let started = std::time::Instant::now();
        match publish_text(c, Some(0), "from-c").await {
            Ok(_) => break started.elapsed(),
            Err(e) if std::time::Instant::now() >= give_up => {
                panic!("the publish from the non-replica never landed: {e:?}")
            }
            Err(e) => {
                eprintln!("non-replica publish attempt failed: {e:?}");
                tokio::time::sleep(Duration::from_millis(100)).await
            }
        }
    };
    assert!(
        took < Duration::from_secs(2),
        "a node with no replica must not wait for a leader: {took:?}"
    );
    assert_eq!(log_end_offset(a, 0), 1);
    assert_eq!(
        registry.forwards().last(),
        Some(&("C".to_string(), "A".to_string()))
    );
    shutdown_all(&nodes);
}

/// What `BusService::create_topic` does when the placement picks a node other
/// than the creator: the genesis (epoch 1) row of every partition is proposed
/// by the creator, through its own ledger handle, naming `leader` first in
/// `replicas`, followed by the other nodes up to `replication_factor`. Nothing
/// is applied by hand; the nodes pick the rows up the way a real node does,
/// from their assignment poll.
fn propose_genesis_led_by(
    creator: &TestNode,
    leader: &str,
    partitions: u32,
    replication_factor: usize,
) {
    let generation = topic_generation(creator);
    let replicas: Vec<&str> = std::iter::once(leader)
        .chain(["A", "B", "C"].into_iter().filter(|node| *node != leader))
        .take(replication_factor)
        .collect();
    for p in 0..partitions {
        creator
            .ledger
            .propose(assignment(generation, &replicas, leader, 1, p))
            .expect("propose genesis");
    }
}

/// TB-04 K4: a topic whose genesis (epoch 1) is led by a node that did not
/// create it comes up on its own: the leader materializes the row, takes the
/// role, the followers (the creator among them) answer its Hello, the ISR
/// fills, and a publish by the creator lands on the leader and replicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_genesis_led_by_a_node_other_than_the_creator_comes_up_and_takes_writes() {
    let (nodes, _ledger, _registry) = build_unplaced_cluster(2, Acks::Quorum, 3).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    propose_genesis_led_by(a, "B", 2, 3);
    for node in &nodes {
        spawn_background_loops(node);
    }

    let handshake = wait_until(Duration::from_secs(15), || {
        let leader_isr = b.manager.snapshot(ORG, Some(TOPIC)).partitions;
        (0..2).all(|p| {
            [a, c].iter().all(|follower| {
                follower
                    .svc
                    .local_partition(&ctx(), TOPIC, p)
                    .map(|part| part.leader_epoch() == 1)
                    .unwrap_or(false)
            }) && leader_isr.iter().any(|info| {
                info.partition == p
                    && ["A", "B", "C"]
                        .iter()
                        .all(|member| info.isr.iter().any(|m| m == member))
            })
        })
    })
    .await;
    assert!(
        handshake,
        "Hello and ISR never completed under the remote leader"
    );
    assert_eq!(
        b.manager.role(ORG, TOPIC, 0),
        PartitionRole::Leader { epoch: 1 }
    );
    assert_eq!(
        a.manager.role(ORG, TOPIC, 0),
        PartitionRole::Follower {
            leader_node_id: "B".to_string(),
            epoch: 1
        },
        "the creator follows the leader it placed"
    );

    // The creator is a follower, so this write is forwarded to B.
    let result = publish_records(
        a,
        Some(1),
        vec![binary_record("k", b"from-the-creator".to_vec())],
    )
    .await
    .unwrap_or_else(|e| panic!("the creator's publish failed: {e:?}"));
    assert_eq!(result.accepted, 1);
    assert_eq!(log_end_offset(b, 1), 1, "the record landed on the leader");
    assert!(
        wait_until(Duration::from_secs(10), || log_end_offset(a, 1) >= 1
            && log_end_offset(c, 1) >= 1
            && high_watermark(b, 1) >= 1)
        .await,
        "the record never replicated to the followers"
    );
    assert_eq!(read_all_records(a, 1), read_all_records(b, 1));
    assert_eq!(read_all_records(c, 1), read_all_records(b, 1));
    shutdown_all(&nodes);
}

/// The creator's producer may publish before the leader has materialized the
/// rows (the ledger reaches it within a poll). The write is refused there and
/// asked again until the leader takes the role, within the wait for a leader;
/// it is not lost and does not fail. The topic has one replica, the leader,
/// so the creator holds none and the write has no ISR to wait for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_by_the_creator_before_the_remote_leader_has_the_row_lands_once_it_has() {
    let (nodes, _ledger, registry) = build_unplaced_cluster(1, Acks::Leader, 1).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    propose_genesis_led_by(a, "B", 1, 1);
    // B is not polling yet: it does not know it leads.
    spawn_background_loops(a);
    a.manager.on_peer_connected("B");

    let a_svc = Arc::clone(&a.svc);
    let publish = tokio::spawn(async move {
        a_svc
            .publish_async(
                &ctx(),
                TOPIC,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![binary_record("early", b"early-write".to_vec())],
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !publish.is_finished(),
        "the write waits for the leader, it is not refused"
    );
    assert_eq!(
        b.manager.role(ORG, TOPIC, 0),
        PartitionRole::Unavailable {
            reason: UnavailableReason::NoAssignment
        },
        "precondition: the leader is unaware of the topic's placement"
    );
    spawn_background_loops(b);

    let result = publish
        .await
        .expect("publish task")
        .unwrap_or_else(|e| panic!("the early publish was lost: {e:?}"));
    assert_eq!(result.accepted, 1);
    assert_eq!(log_end_offset(b, 0), 1, "exactly one copy on the leader");
    assert_eq!(
        registry.forwards().last(),
        Some(&("A".to_string(), "B".to_string()))
    );
    shutdown_all(&nodes);
}

/// The topic row and the placement reach the leader through the ledger, and
/// can come later than the creator's first publish: the leader answers "not
/// ready" (the manager has no entry for the partition yet, which is also what
/// keeps the request from reaching a bus service that lacks the topic), never
/// `TopicNotFound`, and the write lands once, after both have arrived. (The
/// service-side wait for a topic row that is behind the sender's is covered
/// by the `a_forwarded_publish_*` unit tests in `bus/mod.rs`: the manager's
/// entry for a partition normally implies the row, so only an update or a
/// re-creation leaves the two apart.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_before_the_leader_has_the_topic_row_lands_once_it_arrives() {
    let (nodes, _ledger, _registry) =
        build_unplaced_cluster_withholding(1, Acks::Leader, 1, &["B"]).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let row = tentaflow_core::db::repository::bus_topic_get(&a.db, TEST_INSTANCE_ID, ORG, TOPIC)
        .expect("read topic row")
        .expect("topic row");
    propose_genesis_led_by(a, "B", 1, 1);
    // B has neither the topic row nor the placement yet: both come through
    // the ledger, later than a forwarded write does.
    spawn_background_loops(a);
    a.manager.on_peer_connected("B");

    let a_svc = Arc::clone(&a.svc);
    let publish = tokio::spawn(async move {
        a_svc
            .publish_async(
                &ctx(),
                TOPIC,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![binary_record("early", b"early-write".to_vec())],
                },
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !publish.is_finished(),
        "the write waits for the topic row, it is not refused"
    );
    tentaflow_core::db::repository::bus_topic_create(&b.db, &row).expect("topic row arrives");
    spawn_background_loops(b);

    let result = publish
        .await
        .expect("publish task")
        .unwrap_or_else(|e| panic!("the early publish was lost: {e:?}"));
    assert_eq!(result.accepted, 1);
    assert_eq!(log_end_offset(b, 0), 1, "exactly one copy on the leader");
    shutdown_all(&nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_with_partitions_on_two_leaders_is_split_one_request_per_leader() {
    let (nodes, _ledger, registry) = build_cluster_led_by(2, Acks::Quorum, &["A", "B", "C"], |p| {
        if p == 0 { "A" } else { "B" }.to_string()
    })
    .await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    let leaders_have_their_isr = || {
        let full = |node: &TestNode, partition: u32| {
            node.manager
                .snapshot(ORG, Some(TOPIC))
                .partitions
                .iter()
                .any(|p| p.partition == partition && p.isr.len() == 3)
        };
        full(a, 0) && full(b, 1)
    };
    assert!(
        wait_until(Duration::from_secs(15), leaders_have_their_isr).await,
        "the replicas never joined both leaders' ISR"
    );

    let sent: Vec<Vec<u8>> = (0..40)
        .map(|i| format!("payload-{i}").into_bytes())
        .collect();
    let records = sent
        .iter()
        .enumerate()
        .map(|(i, payload)| binary_record(&format!("key-{i}"), payload.clone()))
        .collect();
    let result = publish_records(c, None, records)
        .await
        .unwrap_or_else(|e| panic!("publish across two leaders failed: {e:?}"));
    assert_eq!(result.accepted, 40);
    let mut acked: Vec<u32> = result.partitions.iter().map(|p| p.partition).collect();
    acked.sort();
    assert_eq!(acked, vec![0, 1]);

    let on_a = read_all_records(a, 0);
    let on_b = read_all_records(b, 1);
    assert!(
        !on_a.is_empty() && !on_b.is_empty(),
        "keys must spread over both partitions"
    );
    let mut landed: Vec<Vec<u8>> = on_a.iter().chain(&on_b).map(|r| r.1.clone()).collect();
    landed.sort();
    let mut want = sent;
    want.sort();
    assert_eq!(
        landed, want,
        "every record landed once, on its own partition's leader"
    );

    let mut forwards = registry.forwards();
    forwards.sort();
    assert_eq!(
        forwards,
        vec![
            ("C".to_string(), "A".to_string()),
            ("C".to_string(), "B".to_string())
        ]
    );
    shutdown_all(&nodes);
}

/// The leader moves while a write is on its way: the publisher's view still
/// names the old leader, which refuses; once the publisher learns the new
/// assignment the write is sent again — once — and lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_that_moved_mid_flight_gets_exactly_one_retry_at_the_new_leader() {
    // No background loops: nothing elects, and each node learns the move
    // only when this test applies it.
    let (nodes, _ledger, registry) =
        build_cluster_placed(1, Acks::Quorum, &["A", "B", "C"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );

    let generation = topic_generation(a);
    let moved = assignment(generation, &["A", "B", "C"], "B", 2, 0);
    // The new leader's Hello would tell C about the move on its own; C is to
    // learn of it only when this test says so.
    registry.block_dials("B", "C");
    a.manager.apply_assignment(moved.clone()).await;
    b.manager.apply_assignment(moved.clone()).await;
    assert!(
        wait_until(Duration::from_secs(10), || {
            b.manager
                .snapshot(ORG, Some(TOPIC))
                .partitions
                .iter()
                .any(|p| p.isr.len() >= 2)
        })
        .await,
        "the new leader never gathered a quorum"
    );

    let publisher = Arc::clone(&c.svc);
    let publish = tokio::spawn(async move {
        publisher
            .publish_async(
                &ctx(),
                TOPIC,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![binary_record("k", b"moved".to_vec())],
                },
            )
            .await
    });
    // C still believes A leads: the request goes to A, which refuses, and C
    // waits for an assignment change.
    assert!(
        wait_until(Duration::from_secs(5), || registry.forwards().len() == 1).await,
        "the first forward never happened"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    if publish.is_finished() {
        panic!(
            "the publish must wait for the new leader, but ended: {:?}",
            publish.await
        );
    }
    c.manager.apply_assignment(moved).await;

    let result = publish
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("the retried publish failed: {e:?}"));
    assert_eq!(result.accepted, 1);
    assert_eq!(
        registry.forwards(),
        vec![
            ("C".to_string(), "A".to_string()),
            ("C".to_string(), "B".to_string())
        ],
        "exactly one retry, at the new leader, and none from A"
    );
    assert_eq!(read_all_records(b, 0).len(), 1);
    shutdown_all(&nodes);
}

/// Two nodes that each believe the other leads: the receiver of a forwarded
/// publish answers `NotLeader` instead of forwarding it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarded_publish_is_never_sent_back_to_its_sender() {
    let (nodes, _ledger, registry) =
        build_cluster_placed(1, Acks::Quorum, &["A", "B", "C"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    let generation = topic_generation(a);
    a.manager
        .apply_assignment(assignment(generation, &["A", "B", "C"], "B", 2, 0))
        .await;
    b.manager
        .apply_assignment(assignment(generation, &["A", "B", "C"], "A", 2, 0))
        .await;

    let err = publish_text(a, Some(0), "loop").await.unwrap_err();
    assert!(
        matches!(err, BusServiceError::NotLeader { .. }),
        "expected NotLeader, got {err:?}"
    );
    assert_eq!(
        registry.forwards(),
        vec![("A".to_string(), "B".to_string())],
        "B must not hand the request back to A, and A asks it only once"
    );
    shutdown_all(&nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_authorization_refusal_on_the_leader_reaches_the_publisher() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    a.deny_produce.store(true, Ordering::SeqCst);

    let err = publish_text(b, Some(0), "forbidden").await.unwrap_err();
    assert!(
        matches!(err, BusServiceError::PermissionDenied { .. }),
        "the leader's refusal must arrive as itself, got {err:?}"
    );
    assert_eq!(log_end_offset(a, 0), 0, "nothing was written");
    shutdown_all(&nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_system_actor_is_refused_on_a_forwarded_publish_to_an_ordinary_topic() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    let mut system = ctx();
    system.actor = Some(tentaflow_core::services::bus_authorizer::SYSTEM_ACTOR.to_string());
    system.actor_kind = tentaflow_core::auth::actor::ActorKind::System;

    let err = publish_records_as(b, &system, Some(0), vec![binary_record("k", b"x".to_vec())])
        .await
        .unwrap_err();
    assert!(
        matches!(err, BusServiceError::PermissionDenied { .. }),
        "got {err:?}"
    );
    assert_eq!(log_end_offset(a, 0), 0, "nothing was written");
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
        instance_id: TEST_INSTANCE_ID.to_string(),
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "A".to_string(),
        replicas: vec!["A".to_string(), "D".to_string(), "E".to_string()],
        isr: vec!["A".to_string()],
        leader_epoch: 2,
        updated_at_ms: 0,
        topic_generation: topic_generation(a),
        epoch_slots: Default::default(),
    });

    // The epoch-2 row keeps A as leader, so A re-stamps its entry in place
    // and swaps leader handles: `apply_assignment` stops the epoch-1 handle
    // (the stop flushes partition meta on the engine's writer thread) and
    // only then attaches the epoch-2 one. In between, the entry already
    // reads `Leader { epoch: 2 }` with the new static ISR `{A}` — which is
    // what `snapshot` falls back to without a handle — while `preflight`
    // refuses every publish, because nothing could replicate or count acks
    // for it, and `BusService` reports that as `NotLeader { None, 2 }`. So
    // neither the role nor the snapshot tells this test the swap is over;
    // only a publish does. A `NotLeader` naming no leader at A's own new
    // epoch is that bounded swap and is retried; anything else, or the
    // swap outlasting the deadline, fails. Measured: 0-26 ms over 20 runs.
    assert!(
        wait_until(Duration::from_secs(5), || {
            a.manager.role(ORG, TOPIC, 0) == PartitionRole::Leader { epoch: 2 }
        })
        .await,
        "leader never materialized the reassigned (under-replicated) assignment"
    );
    let swap_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let err = loop {
        let err = publish_text(a, Some(0), "no-quorum").await.unwrap_err();
        let mid_swap = matches!(
            err,
            BusServiceError::NotLeader {
                leader_node_id: None,
                leader_epoch: 2
            }
        );
        if !mid_swap || tokio::time::Instant::now() >= swap_deadline {
            break err;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        matches!(err, BusServiceError::NotEnoughReplicas { .. }),
        "expected NotEnoughReplicas, got {err:?}"
    );

    a.manager.shutdown();
}

/// A lone leader at RF = 2 with `acks=leader` has to keep making what it
/// appends readable: `hw` follows the leader's own appends once the follower
/// is gone, because nothing else (no ack, no ISR change) moves it any more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lone_rf2_leader_keeps_its_appends_readable_after_the_follower_dies() {
    let (nodes, _ledger, _registry) = build_cluster_placed(1, Acks::Leader, &["A", "B"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    spawn_background_loops(a);
    spawn_background_loops(b);
    let isr_of_a = |node: &TestNode| -> Vec<String> {
        node.manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .iter()
            .find(|info| info.partition == 0)
            .map(|info| info.isr.clone())
            .unwrap_or_default()
    };
    assert!(
        wait_until(Duration::from_secs(10), || isr_of_a(a)
            .iter()
            .any(|m| m == "B"))
        .await,
        "the follower never joined the in-sync set"
    );
    publish_text(a, Some(0), "before-1").await.expect("publish");
    publish_text(a, Some(0), "before-2").await.expect("publish");

    b.manager.shutdown();
    assert!(
        wait_until(Duration::from_secs(10), || !isr_of_a(a)
            .iter()
            .any(|m| m == "B"))
        .await,
        "the leader never dropped the dead follower from its in-sync set"
    );

    for i in 0..5 {
        publish_text(a, Some(0), &format!("alone-{i}"))
            .await
            .unwrap_or_else(|e| panic!("publish {i} with the follower dead: {e:?}"));
    }
    assert_eq!(log_end_offset(a, 0), 7);
    assert_eq!(
        high_watermark(a, 0),
        7,
        "records the leader accepted must be readable"
    );
    let read = consume_bounded(a, ctx(), "fresh-group", 1024 * 1024, usize::MAX)
        .await
        .expect("a new consumer reads on the lone leader");
    assert_eq!(read.len(), 7, "a new group reads every accepted record");
    shutdown_all(&nodes);
}

/// What a node that does not lead shows as the in-sync set of a partition is
/// the leader's live one, within a heartbeat, not the set the replicated row
/// carried since the last placement change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_follower_shows_the_leaders_live_in_sync_set_not_the_one_of_the_row() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Quorum).await;
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    let b = find_node(&nodes, "B");
    // The row B holds names the leader alone as in sync: what a placement
    // change that happened before the followers caught up leaves behind.
    let mut stale = assignment(topic_generation(b), &["A", "B", "C"], "A", 1, 0);
    stale.isr = vec!["A".to_string()];
    b.manager.apply_assignment(stale).await;

    let isr_on_b = || -> Vec<String> {
        let mut isr = b
            .manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .iter()
            .find(|info| info.partition == 0)
            .map(|info| info.isr.clone())
            .unwrap_or_default();
        isr.sort();
        isr
    };
    let everyone = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    assert!(
        wait_until(Duration::from_secs(5), || isr_on_b() == everyone).await,
        "B shows {:?}, the leader's live set is {everyone:?}",
        isr_on_b()
    );

    // And it follows the leader when a member leaves.
    find_node(&nodes, "C").manager.shutdown();
    let without_c = vec!["A".to_string(), "B".to_string()];
    assert!(
        wait_until(Duration::from_secs(15), || isr_on_b() == without_c).await,
        "B shows {:?} after C left, the leader's live set is {without_c:?}",
        isr_on_b()
    );
    shutdown_all(&nodes);
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
        local_node_id: "PROD".to_string(),
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
            instance_id: TEST_INSTANCE_ID.to_string(),
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: 0,
            leader_node_id: "PROD".to_string(),
            leader_epoch: 1,
            replicas: vec!["PROD".to_string(), "TEST".to_string()],
            environment: NodeEnvironment::Prod, // mismatches TEST's own local_env
            topic_generation: Some(0),
            bus_features: 0,
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

// ===== TB-04 K7: an admin on a replica reads a topic another node leads ====

/// The read-only views of the admin screens, asked on B for a topic A leads:
/// the messages are the leader's up to B's high watermark (an uncommitted
/// tail B holds past it stays hidden), the lag is the leader's once the
/// commit has crossed the coalescing window, and every change of state is
/// still refused on B, naming A.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_on_a_follower_reads_a_topic_led_by_another_node() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "B/C never completed the initial Hello handshake"
    );
    for i in 0..6u32 {
        publish_text(a, Some(0), &format!("rec-{i}"))
            .await
            .expect("publish on the leader");
    }
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(b, 0) >= 6).await,
        "B never committed the six records: b_leo={} b_hw={}",
        log_end_offset(b, 0),
        high_watermark(b, 0)
    );

    const GROUP: &str = "g-replica-read";
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
    consumer.fetch(1024 * 1024, 2_000).expect("fetch");
    consumer
        .commit(&[(
            TopicPartition {
                topic: TOPIC.to_string(),
                partition: 0,
            },
            4,
        )])
        .expect("commit offset 4");
    assert!(
        wait_until(Duration::from_secs(5), || committed_group_offset(
            b, GROUP, 0
        ) == 4)
        .await,
        "the commit never reached B"
    );

    // B reads up to what the leader has told it is on a majority, which trails
    // its high watermark by a heartbeat.
    assert!(
        wait_until(Duration::from_secs(5), || readable_up_to(b) == 6).await,
        "B never learned that the six records are committed"
    );

    // An uncommitted tail on B, past its high watermark.
    let b_part = b
        .svc
        .local_partition(&ctx(), TOPIC, 0)
        .expect("B partition");
    b_part
        .append_batch_async(one_text_batch("ghost-1"))
        .await
        .expect("append the uncommitted tail");
    assert_eq!(log_end_offset(b, 0), 7);
    assert_eq!(high_watermark(b, 0), 6, "the tail is past hw");

    let peek = |node: &TestNode| {
        node.svc
            .peek(
                &ctx(),
                TOPIC,
                0,
                0,
                100,
                tentaflow_core::bus::PEEK_MAX_BYTES,
            )
            .expect("peek")
    };
    let (on_a, on_b) = (peek(a), peek(b));
    assert!(!on_a.from_replica && on_b.from_replica);
    let seen = |r: &tentaflow_core::bus::PeekResult| -> Vec<(u64, Vec<u8>)> {
        r.records
            .iter()
            .map(|r| (r.offset, r.payload.to_vec()))
            .collect()
    };
    assert_eq!(
        seen(&on_b),
        seen(&on_a),
        "the replica shows the leader's records"
    );
    assert_eq!(on_b.records.len(), 6, "the ghost past hw is not shown");
    assert_eq!(on_b.high_watermark, 6);

    let runs = b
        .svc
        .peek_runs(&ctx(), TOPIC, 0, &[0..7], 64)
        .expect("peek_runs");
    assert_eq!(runs.records.len(), 6);
    assert!(runs.records.iter().all(|r| r.offset < 6));

    let lag = |node: &TestNode| {
        node.svc
            .group_lag(&ctx(), GROUP, TOPIC)
            .expect("group_lag")
            .into_iter()
            .map(|p| (p.committed_offset, p.high_watermark, p.lag))
            .collect::<Vec<_>>()
    };
    assert_eq!(lag(a), vec![(4, 6, 2)]);
    assert_eq!(
        lag(b),
        lag(a),
        "the lag on B is the leader's once the commit arrived"
    );

    // Nothing that changes state is let through on B; each refusal names A.
    let names_a = |what: &str, outcome: Result<(), BusServiceError>| match outcome {
        Err(BusServiceError::NotLeader {
            leader_node_id: Some(leader),
            leader_epoch: 1,
        }) => assert_eq!(leader, "A", "{what}"),
        other => panic!("{what}: expected NotLeader naming A, got {other:?}"),
    };
    names_a("pause", b.svc.pause_group(&ctx(), GROUP, TOPIC));
    names_a("reset", b.svc.reset_offset(&ctx(), GROUP, TOPIC, 0, 0));
    assert_eq!(committed_group_offset(b, GROUP, 0), 4);
    assert_eq!(committed_group_offset(a, GROUP, 0), 4);

    shutdown_all(&nodes);
}

// ===== TB-04 K7: consumer-group rows and pauses are replicated ============

fn group_row(node: &TestNode, group: &str) -> Option<tentaflow_core::db::repository::DbBusGroup> {
    tentaflow_core::db::repository::bus_group_get(node.svc.local_db(), ORG, group, TOPIC)
        .expect("read the group row")
}

/// How far a read on `node` goes: its high watermark, but never past what the
/// leader has told it is on a majority (`committed_offset`), which trails the
/// high watermark by a heartbeat.
fn readable_up_to(node: &TestNode) -> u64 {
    node.svc
        .read_window(&ctx(), TOPIC, 0)
        .expect("read window")
        .high_watermark
}

fn open_explicit(node: &TestNode, group: &str) -> tentaflow_core::bus::ConsumerHandle {
    node.svc
        .open_consumer(
            &ctx(),
            group,
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: tentaflow_core::bus::groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer")
}

/// A pause set on the leader reaches the replicas, is still there on the
/// node that takes over, keeps the topic where it is (the leader-return loop
/// would otherwise move a paused group's topic and drop its pause), and is
/// lifted on the node that comes back once it is resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_reaches_the_replicas_and_survives_a_failover_and_a_return() {
    const GROUP: &str = "g-paused";
    let (mut nodes, ledger, registry) =
        build_cluster_placed(1, Acks::Leader, &["A", "B", "C"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    assert!(
        wait_isr_full(&nodes, TOPIC, "A", &["A", "B", "C"]).await,
        "the ISR never filled"
    );
    let a = find_node(&nodes, "A");
    for i in 0..4 {
        publish_on(&a.svc, TOPIC, &format!("rec-{i}"))
            .await
            .expect("publish");
    }
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(a, 0) >= 4).await,
        "the records never became visible"
    );
    let consumer = open_explicit(a, GROUP);
    assert_eq!(consumer.fetch(1024 * 1024, 2_000).unwrap().records.len(), 4);
    consumer
        .commit(&[(
            TopicPartition {
                topic: TOPIC.to_string(),
                partition: 0,
            },
            2,
        )])
        .expect("commit");

    // A replica lists the group the moment the leader opened it.
    for id in ["B", "C"] {
        let node = find_node(&nodes, id);
        assert!(
            wait_until(Duration::from_secs(5), || group_row(node, GROUP)
                .is_some_and(|row| !row.paused))
            .await,
            "the group row never reached {id}"
        );
    }
    a.svc.pause_group(&ctx(), GROUP, TOPIC).expect("pause");
    for id in ["B", "C"] {
        let node = find_node(&nodes, id);
        assert!(
            wait_until(Duration::from_secs(5), || group_row(node, GROUP)
                .is_some_and(|row| row.paused))
            .await,
            "the pause never reached {id}"
        );
    }

    // A crashes; a survivor takes over and holds the pause it was sent.
    registry.peers.lock().remove("A");
    let a_pos = nodes.iter().position(|n| n.id == "A").expect("node A");
    let crashed = nodes.remove(a_pos);
    crashed.manager.shutdown();
    drop(crashed);
    assert!(
        wait_until(Duration::from_secs(20), || leader_of(&nodes, TOPIC)
            .is_some())
        .await,
        "no survivor took over"
    );
    let survivor = leader_of(&nodes, TOPIC).expect("one leader");
    let leader = find_node(&nodes, &survivor);
    let resumed = open_explicit(leader, GROUP);
    assert!(
        matches!(
            resumed.fetch(1024 * 1024, 100),
            Err(tentaflow_core::bus::BusServiceError::GroupPaused { .. })
        ),
        "the promoted node served a group that was paused"
    );

    // A comes back with an empty disk. Its topic stays with the survivor
    // while the group is paused, however long A has been in sync.
    for node in &nodes {
        node.svc.set_leader_return_timing(quick_return_timing());
        spawn_leader_return_loop(node);
    }
    let row =
        tentaflow_core::db::repository::bus_topic_get(&leader.db, TEST_INSTANCE_ID, ORG, TOPIC)
            .expect("read topic row")
            .expect("topic row");
    let back = build_node(
        "A",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    tentaflow_core::db::repository::bus_topic_create(&back.db, &row).expect("topic row on A");
    back.svc.set_leader_return_timing(quick_return_timing());
    spawn_background_loops(&back);
    spawn_leader_return_loop(&back);
    nodes.push(back);
    let leader = find_node(&nodes, &survivor);
    assert!(
        wait_until(Duration::from_secs(30), || leader
            .manager
            .snapshot(ORG, Some(TOPIC))
            .partitions
            .iter()
            .any(|p| p.isr.iter().any(|n| n == "A")))
        .await,
        "A never rejoined the in-sync set"
    );
    tokio::time::sleep(RETURN_STABLE_FOR * 2).await;
    assert_eq!(
        leader_of(&nodes, TOPIC).as_deref(),
        Some(survivor.as_str()),
        "a paused group's topic must not be moved"
    );
    let a = find_node(&nodes, "A");
    assert!(
        wait_until(Duration::from_secs(10), || group_row(a, GROUP)
            .is_some_and(|row| row.paused))
        .await,
        "the node that was away never learned the pause"
    );

    // Resumed on the leader, the topic goes back to A, which serves the group.
    leader
        .svc
        .resume_group(&ctx(), GROUP, TOPIC)
        .expect("resume");
    assert!(
        wait_until(Duration::from_secs(40), || leader_of(&nodes, TOPIC)
            .as_deref()
            == Some("A"))
        .await,
        "the topic never went back to A"
    );
    // The resume travels to A as a replicated row, which can trail the move.
    assert!(
        wait_until(Duration::from_secs(10), || group_row(a, GROUP)
            .is_some_and(|row| !row.paused))
        .await,
        "the resume never reached A"
    );
    let on_a = open_explicit(a, GROUP);
    let batch = on_a
        .fetch(1024 * 1024, 2_000)
        .expect("a resumed group is served by the node that came back");
    assert_eq!(
        batch.records.first().map(|r| r.offset),
        Some(2),
        "it reads on from the offset that was committed"
    );
    shutdown_all(&nodes);
}

/// A replica that was down while a group committed learns the commit when
/// its stream opens, even though the group is idle by then and sends nothing
/// more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replica_that_was_away_gets_the_commit_of_an_idle_group_when_it_rejoins() {
    const GROUP: &str = "g-idle";
    let (mut nodes, ledger, registry) =
        build_cluster_placed(1, Acks::Leader, &["A", "B", "C"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    assert!(
        wait_isr_full(&nodes, TOPIC, "A", &["A", "B", "C"]).await,
        "the ISR never filled"
    );

    registry.peers.lock().remove("B");
    let b_pos = nodes.iter().position(|n| n.id == "B").expect("node B");
    let crashed = nodes.remove(b_pos);
    crashed.manager.shutdown();
    drop(crashed);

    let a = find_node(&nodes, "A");
    for i in 0..6 {
        publish_on(&a.svc, TOPIC, &format!("rec-{i}"))
            .await
            .expect("publish with one replica away");
    }
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(a, 0) >= 6).await,
        "the records never became visible"
    );
    let consumer = open_explicit(a, GROUP);
    assert_eq!(consumer.fetch(1024 * 1024, 2_000).unwrap().records.len(), 6);
    consumer
        .commit(&[(
            TopicPartition {
                topic: TOPIC.to_string(),
                partition: 0,
            },
            4,
        )])
        .expect("commit");
    drop(consumer);
    // Long past the coalescing window: the live note is gone for good.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let row = tentaflow_core::db::repository::bus_topic_get(&a.db, TEST_INSTANCE_ID, ORG, TOPIC)
        .expect("read topic row")
        .expect("topic row");
    let back = build_node(
        "B",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    tentaflow_core::db::repository::bus_topic_create(&back.db, &row).expect("topic row on B");
    spawn_background_loops(&back);
    nodes.push(back);
    let b = find_node(&nodes, "B");

    assert!(
        wait_until(Duration::from_secs(30), || {
            high_watermark(b, 0) >= 6 && committed_group_offset(b, GROUP, 0) == 4
        })
        .await,
        "B never learned the commit: hw={} committed={}",
        high_watermark(b, 0),
        committed_group_offset(b, GROUP, 0)
    );
    assert!(
        wait_until(Duration::from_secs(10), || readable_up_to(b) == 6).await,
        "B never learned that the six records are committed"
    );
    let lag = b.svc.group_lag(&ctx(), GROUP, TOPIC).expect("lag on B");
    assert_eq!(
        (lag[0].committed_offset, lag[0].lag),
        (4, 2),
        "the replica's lag is the leader's"
    );
    // The group row travels in its own frame, a coalescing window after the
    // commit that the waits above saw.
    assert!(
        wait_until(Duration::from_secs(10), || group_row(b, GROUP).is_some()).await,
        "and it lists the group"
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
    let c_part = c
        .svc
        .local_partition(&ctx(), TOPIC, 0)
        .expect("C partition");
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
        .apply_assignment(assignment(topic_generation(a), &["A", "B", "C"], "A", 2, 0))
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

// ===== Scenario 6: router demux keeps two instances' registries isolated ==

/// plan-app-platform §1.6/§7 W5: with `singleton = false`, one physical
/// node can run several TentaBus instances, but the mesh still exposes
/// exactly ONE `ALPN_BUS` accept slot (`mesh/iroh_manager.rs`'s single
/// `Option<BusAcceptHandler>`). `replication::router` is what makes that
/// safe: it reads an inbound stream's first frame, resolves the NAMED
/// instance's own `ReplicationManager`, and only then looks anything up by
/// `PartitionKey` — inside THAT manager's own `registry`, never any other
/// instance's (`manager::PartitionKey`'s own doc explains why a 3-tuple is
/// sufficient given exactly this ordering).
///
/// Two bare managers stand in for "one node running two instances" — no
/// `TestNode`/`build_cluster` reuse, deliberately: reworking that harness's
/// `TransportRegistry`/`DuplexTransport` (one manager per node id today) to
/// route by instance too would touch every other scenario in this file for
/// a property the router alone already proves. Both managers are seeded
/// with an assignment for the SAME `(org, topic, partition)` key but a
/// DIFFERENT leader epoch — the exact shape that would silently collide if
/// the demux ever fell through to a `PartitionKey` lookup shared across
/// instances instead of resolving the instance first.
#[tokio::test]
async fn router_demux_never_lets_a_hello_for_one_instance_reach_another_instances_registry() {
    use tentaflow_core::bus::instance::BusInstanceId;
    use tentaflow_core::bus::replication::manager::{
        FollowerRunner, FollowerRunnerFactory, LeaderHandle, LeaderHandleFactory, ReplAudit,
    };
    use tentaflow_core::bus::replication::router;

    // Every fake below is only ever exercised on a `Reject`/`UnknownInstance`
    // verdict, so `transport`/`leader_factory`/`follower_factory`/`audit`
    // must never actually be called — `unimplemented!()` bodies turn a
    // routing bug that reaches them into a loud panic instead of a
    // silently-wrong pass.
    struct NeverCalledTransport;
    #[async_trait::async_trait]
    impl Transport for NeverCalledTransport {
        async fn open_stream(&self, _node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
            unimplemented!("this test never dials out")
        }
    }

    struct NoopLedger;
    impl LedgerAdmission for NoopLedger {
        fn admitted_by(&self, _op_id: OperationId) -> Vec<String> {
            Vec::new()
        }
    }
    impl AssignmentStore for NoopLedger {
        fn get(
            &self,
            _instance_id: &str,
            _org: &str,
            _topic: &str,
            _partition: u32,
        ) -> Result<Option<PartitionAssignment>, ReplError> {
            Ok(None)
        }
        fn list_for_topic(
            &self,
            _instance_id: &str,
            _org: &str,
            _topic: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            Ok(Vec::new())
        }
        fn list_for_node(
            &self,
            _instance_id: &str,
            _node_id: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            Ok(Vec::new())
        }
        fn preferred_leader_counts(
            &self,
            _instance_id: &str,
        ) -> Result<HashMap<String, u32>, ReplError> {
            Ok(HashMap::new())
        }
        fn propose(&self, _assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
            Err(ReplError::Internal(
                "this test applies assignments directly, never proposes".into(),
            ))
        }
    }

    struct NeverCalledLeaderFactory;
    impl LeaderHandleFactory for NeverCalledLeaderFactory {
        fn spawn(
            &self,
            _assignment: &PartitionAssignment,
            _replica_streams: Vec<(String, BusRecv, BusSend)>,
        ) -> Result<Box<dyn LeaderHandle>, ReplError> {
            unimplemented!("both managers' assignments are Follower role")
        }
        fn spawn_deferred(
            &self,
            _assignment: &PartitionAssignment,
            _replica_streams: Vec<(String, BusRecv, BusSend)>,
        ) -> Result<Box<dyn LeaderHandle>, ReplError> {
            unimplemented!("both managers' assignments are Follower role")
        }
    }

    struct NeverCalledFollowerFactory;
    impl FollowerRunnerFactory for NeverCalledFollowerFactory {
        fn spawn(
            &self,
            _assignment: &PartitionAssignment,
            _hello: ReplHello,
            _leader_recv: BusRecv,
            _leader_send: BusSend,
        ) -> Result<Box<dyn FollowerRunner>, ReplError> {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn undialed_lease(&self) -> Duration {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn leader_lease(&self) -> Duration {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn election_stagger(&self) -> Duration {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn fence_to_epoch(
            &self,
            _assignment: &PartitionAssignment,
            _epoch: u32,
        ) -> Result<u32, ReplError> {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn cut_to_committed(
            &self,
            _assignment: &PartitionAssignment,
        ) -> Result<LogPosition, ReplError> {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
        fn local_log_position(
            &self,
            _assignment: &PartitionAssignment,
        ) -> Result<LogPosition, ReplError> {
            unimplemented!("this test only ever reaches Reject/UnknownInstance verdicts")
        }
    }

    struct NoopAudit;
    impl ReplAudit for NoopAudit {
        fn failover(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _from_node: Option<&str>,
            _to_node: &str,
            _from_epoch: u32,
            _to_epoch: u32,
            _duration_ms: u64,
            _reason: &str,
        ) {
        }
        fn transfer(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _from: &str,
            _to: &str,
            _epoch: u32,
        ) {
        }
        fn evicted(&self, _node_id: &str, _reason: &str, _count: u32) {}
    }

    fn bare_manager(instance_id: &str, local_node_id: &str) -> Arc<ReplicationManager> {
        ReplicationManager::new(ReplicationManagerConfig {
            instance_id: instance_id.to_string(),
            local_node_id: local_node_id.to_string(),
            local_env: NodeEnvironment::Prod,
            transport: Arc::new(NeverCalledTransport),
            ledger: Arc::new(NoopLedger),
            assignments: Arc::new(NoopLedger),
            leader_factory: Arc::new(NeverCalledLeaderFactory),
            follower_factory: Arc::new(NeverCalledFollowerFactory),
            audit: Arc::new(NoopAudit),
            leo_query_timeout: Duration::from_millis(60),
            majority_await_timeout: Duration::from_millis(150),
        })
    }

    let instance_a = BusInstanceId::parse("tentabus-0000000a").expect("valid instance id");
    let instance_b = BusInstanceId::parse("tentabus-0000000b").expect("valid instance id");

    let mgr_a = bare_manager(instance_a.as_str(), "A_LOCAL");
    let mgr_b = bare_manager(instance_b.as_str(), "B_LOCAL");

    // SAME `(org, topic, partition)` key, DIFFERENT epoch per instance —
    // see this test's own doc for why that shape matters here.
    mgr_a
        .apply_assignment(PartitionAssignment {
            instance_id: instance_a.as_str().to_string(),
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: 0,
            leader_node_id: "X_LEADER".to_string(),
            replicas: vec!["A_LOCAL".to_string(), "X_LEADER".to_string()],
            isr: vec!["A_LOCAL".to_string(), "X_LEADER".to_string()],
            leader_epoch: 5,
            updated_at_ms: 0,
            topic_generation: 0,
            epoch_slots: Default::default(),
        })
        .await;
    mgr_b
        .apply_assignment(PartitionAssignment {
            instance_id: instance_b.as_str().to_string(),
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: 0,
            leader_node_id: "Y_LEADER".to_string(),
            replicas: vec!["B_LOCAL".to_string(), "Y_LEADER".to_string()],
            isr: vec!["B_LOCAL".to_string(), "Y_LEADER".to_string()],
            leader_epoch: 9,
            updated_at_ms: 0,
            topic_generation: 0,
            epoch_slots: Default::default(),
        })
        .await;

    // W5 review finding T3: `MANAGERS`/the two managers' own background
    // tasks are process-global state shared with every other test in this
    // binary — an assertion panic between `register_manager` and the
    // explicit cleanup that used to sit at the end of this function would
    // leak `tentabus-0000000a`/`tentabus-0000000b` (and their shutdown
    // tokens) into the process forever, silently poisoning any later test
    // that happens to register the same ids. This guard makes cleanup run
    // on EVERY exit path, panic included, exactly like `shutdown_all` does
    // for the harness-based scenarios elsewhere in this file.
    struct DemuxCleanupGuard {
        ids: Vec<BusInstanceId>,
        managers: Vec<Arc<ReplicationManager>>,
    }
    impl Drop for DemuxCleanupGuard {
        fn drop(&mut self) {
            for id in &self.ids {
                router::unregister(id);
            }
            for m in &self.managers {
                m.shutdown();
            }
        }
    }

    // Both instances registered with the SAME process-global router — the
    // "one node, two instances, one accept slot" shape this test exists to
    // prove safe.
    router::register_manager(instance_a.clone(), Arc::clone(&mgr_a));
    router::register_manager(instance_b.clone(), Arc::clone(&mgr_b));
    let _cleanup = DemuxCleanupGuard {
        ids: vec![instance_a.clone(), instance_b.clone()],
        managers: vec![Arc::clone(&mgr_a), Arc::clone(&mgr_b)],
    };

    async fn send_hello_and_read_ack(hello: ReplHello) -> frames::ReplHelloAck {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut client_recv, mut client_send) = split(client);
        let (server_recv, server_send) = split(server);
        // A leader dials its followers itself, so the stream's peer is the
        // leader the Hello names.
        tokio::spawn(router::route_stream(
            hello.leader_node_id.clone(),
            Box::new(server_recv),
            Box::new(server_send),
        ));
        frames::write_frame(&mut client_send, &ReplFrame::Hello(hello))
            .await
            .expect("write Hello");
        match frames::read_frame(&mut client_recv)
            .await
            .expect("read HelloAck")
        {
            ReplFrame::HelloAck(ack) => ack,
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    // (1) A Hello naming instance A, one epoch below A's OWN stored
    // assignment (5). If the router ever resolved this against instance
    // B's registry instead — the leak this whole mechanism exists to rule
    // out — the reject would report `have: 9` (B's epoch), not `have: 5`.
    let ack = send_hello_and_read_ack(ReplHello {
        instance_id: instance_a.as_str().to_string(),
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "X_LEADER".to_string(),
        leader_epoch: 4,
        replicas: vec!["A_LOCAL".to_string(), "X_LEADER".to_string()],
        environment: NodeEnvironment::Prod,
        topic_generation: Some(0),
        bus_features: 0,
    })
    .await;
    assert!(!ack.accepted);
    assert_eq!(
        ack.reject,
        Some(ReplReject::StaleEpoch { have: 5 }),
        "must be evaluated against instance A's own registry (epoch 5), never \
         instance B's (epoch 9) — proves this Hello never reached B's registry"
    );

    // (1b) W5 review finding T3 — the MIRROR of (1): a Hello naming
    // instance B, one epoch below B's OWN stored assignment (9). Without
    // this direction, a router that resolved EVERY Hello to whichever
    // manager it happens to find first (a broken demux that never actually
    // reads `instance_id`) would still pass (1) — A is the only instance
    // ever addressed positively there. Only this half proves the resolved
    // manager is genuinely the ONE THE FRAME NAMED, not just "some manager
    // that was registered".
    let ack = send_hello_and_read_ack(ReplHello {
        instance_id: instance_b.as_str().to_string(),
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "Y_LEADER".to_string(),
        leader_epoch: 8,
        replicas: vec!["B_LOCAL".to_string(), "Y_LEADER".to_string()],
        environment: NodeEnvironment::Prod,
        topic_generation: Some(0),
        bus_features: 0,
    })
    .await;
    assert!(!ack.accepted);
    assert_eq!(
        ack.reject,
        Some(ReplReject::StaleEpoch { have: 9 }),
        "must be evaluated against instance B's own registry (epoch 9), never \
         instance A's (epoch 5) — proves this Hello never reached A's registry"
    );

    // (2) A Hello naming a THIRD instance id that was never registered —
    // proves the presence of A and B does not make the router fall back to
    // answering for an instance neither of them is.
    let ack = send_hello_and_read_ack(ReplHello {
        instance_id: "tentabus-0badc0de".to_string(),
        org_id: ORG.to_string(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "X_LEADER".to_string(),
        leader_epoch: 4,
        replicas: vec!["A_LOCAL".to_string(), "X_LEADER".to_string()],
        environment: NodeEnvironment::Prod,
        topic_generation: Some(0),
        bus_features: 0,
    })
    .await;
    assert!(!ack.accepted);
    assert_eq!(ack.reject, Some(ReplReject::UnknownInstance));

    // Cleanup runs via `_cleanup`'s `Drop` (declared right after
    // `register_manager` above) — on this normal-exit path AND on any
    // panic between here and there, per this test's own T3 finding.
}

// ===== Scenario: an epoch names exactly one leader's chain ================

/// Records every moment two nodes lead the same epoch, for as long as it
/// runs. An epoch is what the protocol ranks and reconciles logs by, so two
/// leaders sharing one leave two chains under one number.
struct SameEpochWatch {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    /// Every (node, epoch) leadership seen, so a scenario can assert which
    /// path it took.
    led: Arc<Mutex<std::collections::BTreeSet<(String, u32)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl SameEpochWatch {
    fn start(nodes: &[TestNode]) -> Self {
        let managers: Vec<(String, Arc<ReplicationManager>)> = nodes
            .iter()
            .map(|n| (n.id.clone(), Arc::clone(&n.manager)))
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let led = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
        let task = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            let led = Arc::clone(&led);
            tokio::spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    let mut leaders: HashMap<u32, Vec<&str>> = HashMap::new();
                    for (id, manager) in &managers {
                        if let PartitionRole::Leader { epoch } = manager.role(ORG, TOPIC, 0) {
                            leaders.entry(epoch).or_default().push(id);
                            led.lock().insert((id.clone(), epoch));
                        }
                    }
                    for (epoch, ids) in leaders {
                        if ids.len() > 1 {
                            let mut seen = seen.lock();
                            let line = format!("epoch {epoch} led by {ids:?}");
                            if !seen.contains(&line) {
                                seen.push(line);
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        };
        Self {
            stop,
            seen,
            led,
            task,
        }
    }

    /// Stops watching: the moments two nodes led one epoch, and every node
    /// that led at all.
    async fn finish(self) -> (Vec<String>, Vec<String>) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.task.await;
        let seen = self.seen.lock().clone();
        let mut leaders: Vec<String> = self.led.lock().iter().map(|(id, _)| id.clone()).collect();
        leaders.dedup();
        (seen, leaders)
    }
}

/// Publishes `payload` through `node` until one attempt is acknowledged.
async fn publish_until_accepted(node: &TestNode, payload: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let attempt =
            tokio::time::timeout(Duration::from_secs(2), publish_text(node, Some(0), payload))
                .await;
        if matches!(attempt, Ok(Ok(ref res)) if res.accepted == 1) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// The node that leads and has a publish of `payload` acknowledged through
/// it, once one does.
async fn leader_accepting<'a>(
    nodes: &'a [TestNode],
    payload: &str,
    timeout: Duration,
) -> Option<&'a TestNode> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        for node in nodes {
            if matches!(
                node.manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { .. }
            ) && publish_until_accepted(node, payload, Duration::from_millis(500)).await
            {
                return Some(node);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

fn text_payloads(node: &TestNode) -> Vec<String> {
    read_all_payloads(node, 0)
        .into_iter()
        .map(|p| String::from_utf8(p).expect("utf-8 payload"))
        .collect()
}

/// The traced wedge. RF=3, the epoch-1 leader is gone and nobody follows
/// anyone. `held` stands first and asks C for its log — A and B cannot
/// reach each other — and its proposal is held back while `other` stands,
/// is elected, and commits records through C. Then `held` proposes.
///
/// Before epochs were unique, both stood for epoch 2: with A held, the
/// ledger admitted it second (equal epoch, lower node id) and it promoted
/// too — two leaders of epoch 2, C switching to A within one epoch, and A's
/// handshake trying to cut the records C had committed under B. Now the two
/// candidacies stand for different epochs (A's slot deals 64, B's 65), and
/// C, having told the first one what its log held, refuses any leader below
/// the term it promised. `other_leads` names which path the variant must
/// take: the second candidate stands above the held one, leads and commits
/// while the held one later yields — or it hears the held one's promise
/// and stands down, and only the held one ever leads. Either way one epoch
/// has one leader, every acknowledged record survives, and the partition
/// converges.
async fn two_candidacies_from_one_term_leave_one_leader_per_epoch(
    held: &str,
    other: &str,
    other_leads: bool,
) {
    let (nodes, ledger, registry) =
        build_cluster_placed(1, Acks::Quorum, &["A", "B", "C"], "D").await;
    for node in &nodes {
        spawn_assignment_poll_loop(node);
    }
    let watch = SameEpochWatch::start(&nodes);
    let key: PartitionKey = (ORG.to_string(), TOPIC.to_string(), 0);
    registry.cut("A", "B");

    let (entered, release) = ledger.hold_propose(held);
    let held_manager = Arc::clone(&find_node(&nodes, held).manager);
    let held_key = key.clone();
    let held_election = tokio::spawn(async move { held_manager.run_election(held_key).await });
    let reached =
        tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(10)).is_ok())
            .await
            .expect("wait for the held proposal");
    assert!(reached, "{held}'s candidacy never reached its proposal");

    let other_node = find_node(&nodes, other);
    other_node.manager.run_election(key.clone()).await;
    let mut acknowledged = Vec::new();
    if matches!(
        other_node.manager.role(ORG, TOPIC, 0),
        PartitionRole::Leader { .. }
    ) {
        for i in 0..3 {
            let payload = format!("{other}-term-{i}");
            assert!(
                publish_until_accepted(other_node, &payload, Duration::from_secs(10)).await,
                "{other} leads but never had {payload} acknowledged"
            );
            acknowledged.push(payload);
        }
    }

    let _ = release.send(());
    held_election.await.expect("held election task");
    registry.heal();
    for node in &nodes {
        spawn_lease_check_loop(node);
    }

    let leader = leader_accepting(&nodes, "after-heal", Duration::from_secs(20)).await;
    let converged = match leader {
        Some(leader) => {
            wait_until(Duration::from_secs(15), || {
                let log = read_all_payloads(leader, 0);
                nodes.iter().all(|n| read_all_payloads(n, 0) == log)
            })
            .await
        }
        None => false,
    };
    let (same_epoch, leaders) = watch.finish().await;
    let mut violated = Vec::new();
    if !same_epoch.is_empty() {
        violated.push(format!("two leaders held one epoch: {same_epoch:?}"));
    }
    // Which path: the second candidate led and had its records
    // acknowledged, or it stood down and only the held one ever led.
    let expected_leaders = if other_leads {
        vec![other.to_string()]
    } else {
        vec![held.to_string()]
    };
    if leaders != expected_leaders {
        violated.push(format!(
            "expected only {expected_leaders:?} to lead, saw {leaders:?}"
        ));
    }
    if acknowledged.is_empty() == other_leads {
        violated.push(format!(
            "acknowledged through {other}: {acknowledged:?} (expected {})",
            if other_leads { "some" } else { "none" }
        ));
    }
    match leader {
        None => violated.push("no leader accepted a quorum write after the heal".to_string()),
        Some(leader) => {
            if !converged {
                violated.push(format!(
                    "the replicas' logs never converged: {:?}",
                    nodes
                        .iter()
                        .map(|n| (n.id.clone(), text_payloads(n)))
                        .collect::<Vec<_>>()
                ));
            }
            let log = text_payloads(leader);
            for payload in &acknowledged {
                if !log.contains(payload) {
                    violated.push(format!("acknowledged {payload} is gone: {log:?}"));
                }
            }
        }
    }
    shutdown_all(&nodes);
    assert!(violated.is_empty(), "{violated:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_candidacies_from_one_term_leave_one_leader_per_epoch_when_a_is_held() {
    // B's slot deals the higher epoch: B stands above held A and leads.
    two_candidacies_from_one_term_leave_one_leader_per_epoch("A", "B", true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_candidacies_from_one_term_leave_one_leader_per_epoch_when_b_is_held() {
    // Held B stands above A: A hears C's promise to B and stands down.
    two_candidacies_from_one_term_leave_one_leader_per_epoch("B", "A", false).await;
}

/// RF=2 across a split: the leader of epoch 1 is gone, A and B can reach
/// neither each other nor each other's ledger ops, and each alone may lead
/// (the owner's availability decision, `election::availability_quorum`).
/// Both elect, both take writes. Before epochs were unique both stood for
/// epoch 2, and after the heal the loser's log was judged a prefix of the
/// winner's because it named the same epoch — it kept its own records at
/// offsets the winner had written different ones to: two replicas, one
/// epoch, two histories, and nothing reporting it. Now the two leaderships
/// carry different epochs; the newer one wins, and the loser's unreplicated
/// writes are dropped (the accepted RF=2 risk) instead of surviving as a
/// silent divergence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_rf2_split_where_both_replicas_lead_heals_onto_one_chain() {
    let (nodes, ledger, registry) = build_cluster_placed(1, Acks::All, &["A", "B"], "C").await;
    for node in &nodes {
        spawn_assignment_poll_loop(node);
    }
    let watch = SameEpochWatch::start(&nodes);
    let key: PartitionKey = (ORG.to_string(), TOPIC.to_string(), 0);
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    registry.cut("A", "B");
    ledger.split();

    tokio::join!(
        a.manager.run_election(key.clone()),
        b.manager.run_election(key.clone())
    );
    for (node, side, count) in [(a, "a", 2u32), (b, "b", 3u32)] {
        assert!(
            matches!(
                node.manager.role(ORG, TOPIC, 0),
                PartitionRole::Leader { .. }
            ),
            "{} did not lead its side of the split: {:?}",
            node.id,
            node.manager.role(ORG, TOPIC, 0)
        );
        for i in 0..count {
            let payload = format!("{side}-side-{i}");
            assert!(
                publish_until_accepted(node, &payload, Duration::from_secs(10)).await,
                "{} never had {payload} acknowledged",
                node.id
            );
        }
    }

    registry.heal();
    ledger.heal();
    let one_chain = wait_until(Duration::from_secs(20), || {
        let a_leads = matches!(a.manager.role(ORG, TOPIC, 0), PartitionRole::Leader { .. });
        let b_leads = matches!(b.manager.role(ORG, TOPIC, 0), PartitionRole::Leader { .. });
        let a_log = read_all_payloads(a, 0);
        a_leads != b_leads && !a_log.is_empty() && a_log == read_all_payloads(b, 0)
    })
    .await;
    let (same_epoch, _) = watch.finish().await;
    let (a_log, b_log) = (text_payloads(a), text_payloads(b));
    shutdown_all(&nodes);
    let mut violated = Vec::new();
    if !same_epoch.is_empty() {
        violated.push(format!("two leaders held one epoch: {same_epoch:?}"));
    }
    if !one_chain {
        violated.push(format!(
            "the replicas never settled on one chain: A={a_log:?} B={b_log:?}"
        ));
    }
    let from_a = a_log.iter().filter(|p| p.starts_with("a-side")).count();
    let from_b = a_log.iter().filter(|p| p.starts_with("b-side")).count();
    if (from_a, from_b) != (2, 0) && (from_a, from_b) != (0, 3) {
        violated.push(format!(
            "the surviving chain must be exactly one side's: {a_log:?}"
        ));
    }
    assert!(violated.is_empty(), "{violated:#?}");
}

// ===== Callers of forwarding (TB-04 K3) ====================================

/// Reads `topic` of `ctx.org_id` the way the REST GET does (`consume_once`),
/// on a blocking thread.
async fn consume_once_on(
    node: &TestNode,
    group: &str,
) -> Result<Vec<tentaflow_core::bus::FetchedRecordMeta>, BusServiceError> {
    let svc = Arc::clone(&node.svc);
    let group = group.to_string();
    tokio::task::spawn_blocking(move || {
        svc.consume_once(&ctx(), &group, TOPIC, 64 * 1024, usize::MAX, 2_000)
    })
    .await
    .expect("consume task")
}

/// The records of a read on a follower are the leader's, and the commit
/// happens on the leader: a second read neither repeats them nor skips any.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_on_a_follower_is_served_and_committed_by_the_leader() {
    let (nodes, _ledger, registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    publish_records(
        a,
        Some(0),
        vec![
            binary_record("k-1", b"first".to_vec()),
            binary_record("k-2", b"second".to_vec()),
        ],
    )
    .await
    .expect("publish on the leader");

    let first = consume_once_on(b, "reader")
        .await
        .unwrap_or_else(|e| panic!("the read on a follower failed: {e:?}"));
    let payloads: Vec<&[u8]> = first.iter().map(|r| r.payload.as_ref()).collect();
    assert_eq!(payloads, vec![&b"first"[..], &b"second"[..]]);
    assert_eq!(
        registry.forwards().last(),
        Some(&("B".to_string(), "A".to_string())),
        "the read went to the leader"
    );
    assert_eq!(
        committed_group_offset(a, "reader", 0),
        2,
        "the leader committed what it returned"
    );

    publish_records(a, Some(0), vec![binary_record("k-3", b"third".to_vec())])
        .await
        .expect("second publish");
    let second = consume_once_on(b, "reader").await.expect("second read");
    let payloads: Vec<&[u8]> = second.iter().map(|r| r.payload.as_ref()).collect();
    assert_eq!(
        payloads,
        vec![&b"third"[..]],
        "nothing repeated, nothing skipped"
    );
    shutdown_all(&nodes);
}

/// Reads like `consume_once_on`, under the given bounds and a short wait.
async fn consume_bounded(
    node: &TestNode,
    ctx: BusCallContext,
    group: &str,
    max_bytes: usize,
    max_records: usize,
) -> Result<Vec<tentaflow_core::bus::FetchedRecordMeta>, BusServiceError> {
    let svc = Arc::clone(&node.svc);
    let group = group.to_string();
    tokio::task::spawn_blocking(move || {
        svc.consume_once(&ctx, &group, TOPIC, max_bytes, max_records, 100)
    })
    .await
    .expect("consume task")
}

/// The `details` of the audit rows of `action` on `node`.
fn audit_details_of(node: &TestNode, action: &str) -> Vec<String> {
    let conn = node.db.read().expect("db read");
    let mut stmt = conn
        .prepare("SELECT COALESCE(details, '') FROM audit_log WHERE action = ?1")
        .expect("prepare");
    let rows = stmt
        .query_map([action], |row| row.get::<_, String>(0))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

/// A backlog of small records over many partitions, read through a follower
/// under a byte bound that cuts every reply short: every record arrives
/// exactly once, in order within its partition, and after each read the
/// leader's committed offset of every partition is exactly one past the
/// highest record returned, never ahead of what the caller was given.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backlog_across_many_partitions_is_read_through_a_follower_exactly_once() {
    const PARTITIONS: u32 = 6;
    const RECORDS: usize = 1_200;
    let (nodes, _ledger, registry) = build_cluster(PARTITIONS, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, PARTITIONS, Duration::from_secs(15)).await,
        "the Hello handshake never completed"
    );
    for chunk in (0..RECORDS).collect::<Vec<_>>().chunks(100) {
        let records = chunk
            .iter()
            .map(|i| {
                let mut payload = format!("{i:08}").into_bytes();
                payload.resize(200, b'.');
                binary_record(&format!("key-{i}"), payload)
            })
            .collect();
        publish_records(a, None, records)
            .await
            .expect("publish on the leader");
    }

    assert!(
        wait_until(Duration::from_secs(20), || (0..PARTITIONS)
            .map(|p| high_watermark(a, p))
            .sum::<u64>()
            == RECORDS as u64)
        .await,
        "the backlog never became readable on the leader"
    );
    let mut delivered: std::collections::BTreeMap<(u32, u64), usize> =
        std::collections::BTreeMap::new();
    let mut highest: HashMap<u32, u64> = HashMap::new();
    let mut reads = 0;
    loop {
        let got = consume_bounded(b, ctx(), "backlog", 16 * 1024, usize::MAX)
            .await
            .unwrap_or_else(|e| panic!("read {reads} failed: {e:?}"));
        if got.is_empty() {
            break;
        }
        reads += 1;
        assert!(reads < 200, "the backlog never drains");
        for r in &got {
            let index: usize = std::str::from_utf8(&r.payload[..8])
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                delivered.insert((r.partition, r.offset), index).is_none(),
                "partition {} offset {} was delivered twice",
                r.partition,
                r.offset
            );
            let top = highest.entry(r.partition).or_insert(0);
            *top = (*top).max(r.offset + 1);
        }
        for partition in 0..PARTITIONS {
            assert_eq!(
                committed_group_offset(a, "backlog", partition),
                highest.get(&partition).copied().unwrap_or(0),
                "after read {reads} the leader's offset of partition {partition} must be one \
                 past the highest record returned"
            );
        }
    }
    assert!(
        reads >= 5,
        "the bound must have cut the replies ({reads} reads)"
    );
    let mut indices: Vec<usize> = delivered.values().copied().collect();
    indices.sort_unstable();
    assert_eq!(
        indices,
        (0..RECORDS).collect::<Vec<_>>(),
        "every record, once"
    );
    for partition in 0..PARTITIONS {
        let offsets: Vec<u64> = delivered
            .keys()
            .filter(|(p, _)| *p == partition)
            .map(|(_, o)| *o)
            .collect();
        assert_eq!(
            offsets,
            (0..offsets.len() as u64).collect::<Vec<_>>(),
            "partition {partition} must have been read as a gapless run"
        );
    }
    assert!(
        registry
            .forwards()
            .iter()
            .any(|f| f == &("B".to_string(), "A".to_string())),
        "the reads went to the leader"
    );
    shutdown_all(&nodes);
}

/// `max_records` bounds a forwarded read: the leader returns, and commits,
/// no more than that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_records_bounds_a_forwarded_read_and_its_commit() {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Quorum).await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    let records = (0..5)
        .map(|i| binary_record(&format!("k-{i}"), vec![b'x'; 8]))
        .collect();
    publish_records(a, Some(0), records).await.expect("publish");
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(a, 0) == 5).await,
        "the records never became readable on the leader"
    );
    let got = consume_bounded(b, ctx(), "bounded", 1024 * 1024, 2)
        .await
        .expect("read");
    assert_eq!(got.len(), 2);
    assert_eq!(committed_group_offset(a, "bounded", 0), 2);
    shutdown_all(&nodes);
}

/// A node outside the replica set was never dialed by the leader, so it has
/// not heard from it what it accepts: it asks (a capability probe) before it
/// hands the read over, and the read is served, not refused as if the leader
/// could not take it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_from_a_node_that_holds_no_replica_is_probed_and_served() {
    let (nodes, _ledger, registry) = build_cluster_placed(1, Acks::Leader, &["A", "B"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    let a = find_node(&nodes, "A");
    let c = find_node(&nodes, "C");
    c.manager.on_peer_connected("A");
    let give_up = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match publish_text(a, Some(0), "from-a").await {
            Ok(_) => break,
            Err(e) if std::time::Instant::now() >= give_up => panic!("publish on A: {e:?}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    let got = loop {
        match consume_bounded(c, ctx(), "outsider-group", 64 * 1024, usize::MAX).await {
            Ok(records) if !records.is_empty() => break records,
            Ok(_) => {}
            Err(e) if std::time::Instant::now() >= give_up => {
                panic!("the read from the non-replica never succeeded: {e:?}")
            }
            Err(e) => eprintln!("non-replica read attempt failed: {e:?}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await
    };
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].payload.as_ref(), b"from-a");
    assert_eq!(
        registry.forwards().last(),
        Some(&("C".to_string(), "A".to_string()))
    );
    assert_eq!(committed_group_offset(a, "outsider-group", 0), 1);
    shutdown_all(&nodes);
}

/// A leader with one record on it and a node that forwards reads to it.
/// (The refusal audit is windowed per organisation, so each refusal below
/// gets a cluster of its own to be the first in its window.)
async fn cluster_with_one_record() -> Vec<TestNode> {
    let (nodes, _ledger, _registry) = build_cluster(1, Acks::Leader).await;
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    publish_records(
        find_node(&nodes, "A"),
        Some(0),
        vec![binary_record("k-1", b"first".to_vec())],
    )
    .await
    .expect("publish");
    let a = find_node(&nodes, "A");
    assert!(
        wait_until(Duration::from_secs(10), || high_watermark(a, 0) == 1).await,
        "the record never became readable on the leader"
    );
    nodes
}

/// The leader authorizes the original actor again, with its own authorizer,
/// and writes the refusal down with the peer that forwarded the read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarded_read_is_authorized_again_by_the_leader() {
    let nodes = cluster_with_one_record().await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    // The topic ACL on the leader refuses what the node asked allowed.
    a.deny_consume.store(true, Ordering::SeqCst);
    let err = consume_bounded(b, ctx(), "reader", 1024, usize::MAX)
        .await
        .unwrap_err();
    assert!(
        matches!(err, BusServiceError::PermissionDenied { .. }),
        "the leader's refusal reaches the caller as itself, got {err:?}"
    );
    assert_eq!(
        committed_group_offset(a, "reader", 0),
        0,
        "nothing was read"
    );
    let denied = audit_details_of(a, "bus.consume.denied");
    assert!(
        denied
            .iter()
            .any(|d| d.contains("forwarded_from=B") && d.contains("reason=acl")),
        "the refusal names the forwarding peer: {denied:?}"
    );
    a.deny_consume.store(false, Ordering::SeqCst);
    let allowed = consume_bounded(b, ctx(), "reader", 1024, usize::MAX)
        .await
        .expect("the same read once the leader allows it");
    assert_eq!(allowed.len(), 1);
    shutdown_all(&nodes);
}

/// A user the leader does not know as a member of the organisation is
/// refused there, whatever the node that forwarded the read checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarded_read_of_a_user_who_is_no_member_on_the_leader_is_refused() {
    let nodes = cluster_with_one_record().await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let mut outsider = ctx();
    outsider.actor = Some("outsider".to_string());
    let err = consume_bounded(b, outsider, "reader", 1024, usize::MAX)
        .await
        .unwrap_err();
    assert!(
        matches!(err, BusServiceError::PermissionDenied { .. }),
        "{err:?}"
    );
    assert_eq!(committed_group_offset(a, "reader", 0), 0);
    let denied = audit_details_of(a, "bus.consume.denied");
    assert!(
        denied
            .iter()
            .any(|d| d.contains("forwarded_from=B") && d.contains("reason=not_an_org_member")),
        "{denied:?}"
    );
    shutdown_all(&nodes);
}

/// The broker's own actor, and the system and addon kinds, are never
/// accepted on a forwarded read, whatever the topic: `__*` topics of any
/// organisation would otherwise be readable by any peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarded_read_never_accepts_the_system_actor() {
    let nodes = cluster_with_one_record().await;
    let a = find_node(&nodes, "A");
    let system = tentaflow_core::services::bus_authorizer::SYSTEM_ACTOR;
    for (actor, kind, topic) in [
        (system, "system", TOPIC),
        (system, "user", TOPIC),
        (system, "user", "__dlq.orders"),
        (system, "api_key", "__bus.metrics"),
        ("test", "system", "__dlq.orders"),
        ("test", "addon", TOPIC),
    ] {
        let reply = a.svc.consume_forwarded(
            "B",
            frames::ConsumeForward {
                instance_id: TEST_INSTANCE_ID.to_string(),
                environment: NodeEnvironment::Prod,
                org_id: ORG.to_string(),
                topic: topic.to_string(),
                group: "reader".to_string(),
                actor: Some(actor.to_string()),
                actor_kind: kind.to_string(),
                correlation_id: None,
                origin: "test".to_string(),
                max_bytes: 1024,
                max_records: None,
                max_wait_ms: 0,
                topic_generation: 0,
                partitions: 0,
            },
        );
        let error = reply.error.unwrap_or_else(|| {
            panic!(
                "{actor} as {kind} read {topic} through a peer: {:?}",
                reply.records
            )
        });
        assert_eq!(
            error.code, "bus.permission_denied",
            "{actor} as {kind} on {topic}"
        );
    }
    assert_eq!(committed_group_offset(a, "reader", 0), 0);
    let denied = audit_details_of(a, "bus.consume.denied");
    assert!(
        denied
            .iter()
            .any(|d| d.contains("forwarded_from=B") && d.contains("reason=actor_not_forwardable")),
        "{denied:?}"
    );
    shutdown_all(&nodes);
}

/// Two nodes that each believe the other leads: the receiver of a forwarded
/// read answers `NotLeader` instead of forwarding it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forwarded_read_is_never_sent_back_to_its_sender() {
    let (nodes, _ledger, registry) =
        build_cluster_placed(1, Acks::Quorum, &["A", "B", "C"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    assert!(
        wait_for_hello_handshake(&nodes, 1, Duration::from_secs(10)).await,
        "the Hello handshake never completed"
    );
    let generation = topic_generation(a);
    a.manager
        .apply_assignment(assignment(generation, &["A", "B", "C"], "B", 2, 0))
        .await;
    b.manager
        .apply_assignment(assignment(generation, &["A", "B", "C"], "A", 2, 0))
        .await;

    let err = consume_once_on(a, "reader").await.unwrap_err();
    assert!(
        matches!(err, BusServiceError::NotLeader { .. }),
        "expected NotLeader, got {err:?}"
    );
    assert_eq!(
        registry.forwards(),
        vec![("A".to_string(), "B".to_string())],
        "B must not hand the read back to A"
    );
    shutdown_all(&nodes);
}

/// The schema every quarantine scenario below validates against: `id` is
/// required.
const ORDERS_SCHEMA: &str = r#"{"type":"object","properties":{"id":{"type":"string"},
    "status":{"type":"string"}},"required":["id"],"additionalProperties":false}"#;

fn json_record(payload: &str) -> PublishRecord {
    PublishRecord {
        key: None,
        headers: vec![],
        payload: Bytes::from(payload.to_string()),
        timestamp_ms: 0,
        schema_id: 0,
    }
}

/// A one-replica topic `topic` of `org`, created on `creator` and copied to
/// the other nodes the way the ledger replicates a row, led by `leader` for
/// every node's registry.
async fn place_internal_topic(
    nodes: &[TestNode],
    ledger: &SharedLedger,
    org: &str,
    topic: &str,
    leader: &str,
) {
    place_internal_topic_on(nodes, ledger, org, topic, leader, &[leader]).await;
}

/// `place_internal_topic` with the replicas given (the first is the preferred
/// leader), all of them in the in-sync set.
async fn place_internal_topic_on(
    nodes: &[TestNode],
    ledger: &SharedLedger,
    org: &str,
    topic: &str,
    leader: &str,
    replicas: &[&str],
) {
    let creator = find_node(nodes, "A");
    let row_cfg = tentaflow_core::bus::topics::create_internal_topic(
        &creator.db,
        TEST_INSTANCE_ID,
        org,
        topic,
        TopicOptions {
            partitions: Some(1),
            replication_factor: Some(replicas.len() as u32),
            acks: Some(Acks::Leader),
            ..Default::default()
        },
        NodeEnvironment::Prod,
        0,
    )
    .expect("create the internal topic");
    let row =
        tentaflow_core::db::repository::bus_topic_get(&creator.db, TEST_INSTANCE_ID, org, topic)
            .expect("read topic row")
            .expect("topic row");
    for node in nodes.iter().filter(|n| n.id != creator.id) {
        tentaflow_core::db::repository::bus_topic_create(&node.db, &row)
            .expect("replicate topic row");
    }
    let placed = PartitionAssignment {
        instance_id: TEST_INSTANCE_ID.to_string(),
        org_id: org.to_string(),
        topic: topic.to_string(),
        partition: 0,
        leader_node_id: leader.to_string(),
        replicas: replicas.iter().map(|r| r.to_string()).collect(),
        isr: replicas.iter().map(|r| r.to_string()).collect(),
        leader_epoch: 1,
        updated_at_ms: 0,
        topic_generation: row_cfg.generation,
        epoch_slots: Default::default(),
    };
    ledger.seed(placed.clone());
    for node in nodes {
        node.manager.apply_assignment(placed.clone()).await;
    }
}

fn dlq_payloads(node: &TestNode, dlq_topic: &str) -> usize {
    node.svc
        .local_partition(&ctx(), dlq_topic, 0)
        .expect("DLQ partition handle")
        .open_reader()
        .fetch_from_offset(0, 16 * 1024 * 1024)
        .expect("fetch_from_offset")
        .into_iter()
        .map(|b| b.records().count())
        .sum()
}

/// `orders` is led by A and its DLQ by B. A violating record that reaches A
/// directly, or that C handed to A, lands in B's DLQ: the quarantine copy of
/// a forwarded write is forwarded in turn instead of dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quarantine_copy_for_a_dlq_led_by_another_node_loses_no_record() {
    let (nodes, ledger, registry) = build_cluster_placed(1, Acks::Leader, &["A"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    let c = find_node(&nodes, "C");
    let dlq_topic = "__dlq.orders";
    tentaflow_core::bus::schema_registry::registry::register(
        &a.db,
        TEST_INSTANCE_ID,
        ORG,
        "orders",
        tentaflow_core::bus::schema_registry::SchemaType::JsonSchema,
        ORDERS_SCHEMA,
        None,
        None,
    )
    .expect("register the schema");
    a.svc
        .update_topic(
            &ctx(),
            TOPIC,
            TopicOptions {
                schema_id: Some("orders".to_string()),
                validation: Some(tentaflow_core::bus::topics::ValidationMode::Dlq),
                ..Default::default()
            },
        )
        .expect("bind the schema to the topic");
    place_internal_topic(&nodes, &ledger, ORG, dlq_topic, "B").await;
    // Neither link has a mesh to say it is up.
    a.manager.on_peer_connected("B");
    c.manager.on_peer_connected("A");

    // The violation arrives on the leader of the source topic itself.
    let direct = publish_records(
        a,
        Some(0),
        vec![
            json_record(r#"{"id":"ok-1"}"#),
            json_record(r#"{"status":"no-id"}"#),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("the publish on the source leader failed: {e:?}"));
    assert_eq!(
        (
            direct.accepted,
            direct.schema_rejected,
            direct.schema_dropped
        ),
        (1, 1, 0)
    );
    assert_eq!(
        dlq_payloads(b, dlq_topic),
        1,
        "the DLQ leader holds the copy"
    );

    // The violation arrives on a node that leads neither topic.
    let forwarded = publish_records(
        c,
        Some(0),
        vec![
            json_record(r#"{"id":"ok-2"}"#),
            json_record(r#"{"status":"also-no-id"}"#),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("the forwarded publish failed: {e:?}"));
    assert_eq!(
        (
            forwarded.accepted,
            forwarded.schema_rejected,
            forwarded.schema_dropped
        ),
        (1, 1, 0),
        "the quarantine copy of a forwarded write must not be dropped"
    );
    assert_eq!(
        dlq_payloads(b, dlq_topic),
        2,
        "both violations are in the DLQ"
    );
    assert_eq!(
        registry.forwards(),
        vec![
            ("A".to_string(), "B".to_string()),
            ("C".to_string(), "A".to_string()),
            ("A".to_string(), "B".to_string()),
        ]
    );
    assert_eq!(
        log_end_offset(a, 0),
        2,
        "the valid records landed on the source"
    );
    shutdown_all(&nodes);
}

/// A `tracing` writer collecting what a scenario logs on its own thread.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `__bus.metrics` has one partition and one replica, led by A. The rollup
/// of node B (and of C, which holds no replica of anything) reaches that
/// leader keyed by the sender's node id, and the sender logs no warning:
/// before forwarding every non-leader node warned once a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metrics_rollup_of_another_node_lands_on_the_leader_without_a_warning() {
    let org = tentaflow_core::services::org::DEFAULT_ORG_ID;
    let (nodes, ledger, registry) = build_cluster_placed(1, Acks::Leader, &["A"], "A").await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    place_internal_topic(
        &nodes,
        &ledger,
        org,
        tentaflow_core::bus::topics::METRICS_TOPIC_NAME,
        "A",
    )
    .await;
    b.manager.on_peer_connected("A");

    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer({
            let capture = capture.clone();
            move || capture.clone()
        })
        .finish();
    let svc = Arc::clone(&b.svc);
    tokio::task::spawn_blocking(move || {
        tracing::subscriber::with_default(subscriber, || svc.publish_metrics_rollup())
    })
    .await
    .expect("rollup task");

    let logged = String::from_utf8_lossy(&capture.0.lock()).into_owned();
    assert!(
        !logged.contains("WARN"),
        "a rollup from a non-leader must not warn: {logged}"
    );
    assert_eq!(
        registry.forwards(),
        vec![("B".to_string(), "A".to_string())]
    );
    let mut reader = ctx();
    reader.org_id = org.to_string();
    let part = a
        .svc
        .local_partition(&reader, tentaflow_core::bus::topics::METRICS_TOPIC_NAME, 0)
        .expect("metrics partition on the leader");
    let keys: Vec<Option<Vec<u8>>> = part
        .open_reader()
        .fetch_from_offset(0, 16 * 1024 * 1024)
        .expect("fetch_from_offset")
        .into_iter()
        .flat_map(|batch| {
            batch
                .records()
                .map(|r| r.unwrap().key.map(|k| k.to_vec()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        keys,
        vec![Some(b"B".to_vec())],
        "one rollup, keyed by node B"
    );
    shutdown_all(&nodes);
}

// ===== TB-04 K5: spreading topic leaders =====================================

/// Adds a one-partition topic `name`, replicated on `replicas` and led by
/// `leader`, to a cluster built by `build_cluster_placed`: created on A, its
/// row copied to the others, its epoch-1 placement applied everywhere.
async fn add_topic(
    nodes: &[TestNode],
    ledger: &Arc<SharedLedger>,
    name: &str,
    replicas: &[&str],
    leader: &str,
) {
    nodes[0]
        .svc
        .create_topic(
            &ctx(),
            name,
            TopicOptions {
                partitions: Some(1),
                replication_factor: Some(replicas.len() as u32),
                acks: Some(Acks::Leader),
                ..Default::default()
            },
        )
        .expect("create_topic");
    let row =
        tentaflow_core::db::repository::bus_topic_get(&nodes[0].db, TEST_INSTANCE_ID, ORG, name)
            .expect("read topic row")
            .expect("topic row");
    for node in &nodes[1..] {
        tentaflow_core::db::repository::bus_topic_create(&node.db, &row)
            .expect("replicate topic row");
    }
    let mut placed = assignment(row.generation, replicas, leader, 1, 0);
    placed.topic = name.to_string();
    ledger.seed(placed.clone());
    for node in nodes {
        node.manager.apply_assignment(placed.clone()).await;
    }
}

/// The leader of `topic` has every member in its live ISR.
async fn wait_isr_full(nodes: &[TestNode], topic: &str, leader: &str, members: &[&str]) -> bool {
    wait_until(Duration::from_secs(20), || {
        find_node(nodes, leader)
            .manager
            .snapshot(ORG, Some(topic))
            .partitions
            .iter()
            .any(|p| p.partition == 0 && members.iter().all(|m| p.isr.iter().any(|i| i == m)))
    })
    .await
}

async fn publish_on(
    svc: &Arc<BusService>,
    topic: &str,
    payload: &str,
) -> Result<tentaflow_core::bus::PublishResult, BusServiceError> {
    svc.publish_async(
        &ctx(),
        topic,
        PublishBatch {
            partition: Some(0),
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

/// The payloads `node` holds for partition 0 of `topic`, off its own engine.
fn payloads_of(node: &TestNode, topic: &str) -> Vec<String> {
    node.svc
        .local_partition(&ctx(), topic, 0)
        .expect("partition handle")
        .open_reader()
        .fetch_from_offset(0, 16 * 1024 * 1024)
        .expect("fetch_from_offset")
        .into_iter()
        .flat_map(|b| {
            b.records()
                .map(|r| String::from_utf8_lossy(&r.unwrap().payload).into_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn leader_of(nodes: &[TestNode], topic: &str) -> Option<String> {
    let leaders: Vec<&TestNode> = nodes
        .iter()
        .filter(|n| matches!(n.manager.role(ORG, topic, 0), PartitionRole::Leader { .. }))
        .collect();
    (leaders.len() == 1).then(|| leaders[0].id.clone())
}

fn requested_from(plan: &tentaflow_core::bus::leader_balance::LeaderPlan) -> Vec<RequestedMove> {
    plan.moves
        .iter()
        .map(|m| RequestedMove {
            topic: m.topic.clone(),
            partitions: m.partitions.clone(),
            from_node_id: m.from.clone(),
            to_node_id: m.to.clone(),
        })
        .collect()
}

async fn plan_on(node: &TestNode) -> tentaflow_core::bus::leader_balance::LeaderPlan {
    let svc = Arc::clone(&node.svc);
    tokio::task::spawn_blocking(move || svc.leader_balance_plan(Some(ORG), None, &|_, _| true))
        .await
        .expect("plan task")
        .expect("plan")
}

async fn four_topics_led_by_a() -> (
    Vec<TestNode>,
    Arc<SharedLedger>,
    Arc<TransportRegistry>,
    [&'static str; 4],
) {
    let (nodes, ledger, registry) = build_cluster_placed(1, Acks::Leader, &["A", "B"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    for name in ["t2", "t3", "t4"] {
        add_topic(&nodes, &ledger, name, &["A", "B"], "A").await;
    }
    let topics = [TOPIC, "t2", "t3", "t4"];
    for topic in topics {
        assert!(
            wait_isr_full(&nodes, topic, "A", &["A", "B"]).await,
            "the ISR of {topic} never filled"
        );
    }
    (nodes, ledger, registry, topics)
}

/// Four topics led by one of two nodes end two and two, by the plan the admin
/// was shown, while a third node keeps writing to all of them through
/// forwarding: every acknowledged write is in the log of the topic's new
/// leader.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_topics_led_by_one_node_are_spread_two_and_two_while_writes_go_on() {
    let (nodes, _ledger, registry, topics) = four_topics_led_by_a().await;
    let a = find_node(&nodes, "A");
    let c = find_node(&nodes, "C");

    let plan = plan_on(a).await;
    assert_eq!(plan.moves.len(), 2, "plan: {plan:?}");
    let requested = requested_from(&plan);

    let acked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (svc, acked, stop) = (Arc::clone(&c.svc), Arc::clone(&acked), Arc::clone(&stop));
        tokio::spawn(async move {
            let mut i = 0usize;
            while !stop.load(Ordering::SeqCst) {
                let topic = topics[i % topics.len()];
                let payload = format!("w{i}");
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let attempt = tokio::time::timeout(
                        Duration::from_secs(3),
                        publish_on(&svc, topic, &payload),
                    )
                    .await;
                    if matches!(attempt, Ok(Ok(ref r)) if r.accepted == 1) {
                        acked.lock().push((topic.to_string(), payload));
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                i += 1;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let svc = Arc::clone(&a.svc);
    let results = tokio::task::spawn_blocking(move || {
        let run = RunContext {
            actor: Some("test"),
            reason: "rebalance",
        };
        svc.leader_balance_apply(ORG, run, &requested, &|_, _| true)
    })
    .await
    .expect("apply task")
    .expect("apply");
    assert!(
        results.iter().all(|r| r.status == MoveStatus::Moved),
        "results: {results:?}"
    );

    assert!(
        wait_until(Duration::from_secs(15), || {
            let led: Vec<Option<String>> = topics.iter().map(|t| leader_of(&nodes, t)).collect();
            led.iter().all(Option::is_some)
                && led.iter().flatten().filter(|l| *l == "A").count() == 2
                && led.iter().flatten().filter(|l| *l == "B").count() == 2
        })
        .await,
        "the leaders did not settle two and two: {:?}",
        topics
            .iter()
            .map(|t| leader_of(&nodes, t))
            .collect::<Vec<_>>()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, Ordering::SeqCst);
    writer.await.expect("writer");

    let acked = acked.lock().clone();
    for topic in topics {
        assert!(
            acked.iter().any(|(t, _)| t == topic),
            "no write to {topic} was acknowledged"
        );
    }
    for topic in topics {
        let leader = leader_of(&nodes, topic).expect("one leader");
        let held = payloads_of(find_node(&nodes, &leader), topic);
        for (_, payload) in acked.iter().filter(|(t, _)| t == topic) {
            assert!(
                held.contains(payload),
                "{payload} acknowledged on {topic} is not in the log of its leader {leader}"
            );
        }
    }
    assert!(
        registry.forwards().iter().any(|(from, _)| from == "C"),
        "the writes of C never went through forwarding"
    );
    assert_eq!(
        audit_details_of(a, "bus.leader.rebalance").len(),
        1,
        "one aggregate row for the run"
    );
    shutdown_all(&nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebalance_without_the_admin_right_on_a_topic_moves_nothing() {
    let (nodes, _ledger, _registry, topics) = four_topics_led_by_a().await;
    let a = find_node(&nodes, "A");
    let requested = requested_from(&plan_on(a).await);
    assert!(!requested.is_empty());

    let svc = Arc::clone(&a.svc);
    let err = tokio::task::spawn_blocking(move || {
        let run = RunContext {
            actor: Some("test"),
            reason: "rebalance",
        };
        svc.leader_balance_apply(ORG, run, &requested, &|_, _| false)
    })
    .await
    .expect("apply task")
    .expect_err("refused");
    assert!(
        matches!(err, LeaderBalanceError::PermissionDenied(_)),
        "{err:?}"
    );
    for topic in topics {
        assert_eq!(leader_of(&nodes, topic).as_deref(), Some("A"));
    }
    assert!(audit_details_of(a, "bus.leader.transfer").is_empty());
    shutdown_all(&nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plan_that_went_stale_is_refused_with_plan_changed_and_moves_nothing() {
    let (nodes, _ledger, _registry, topics) = four_topics_led_by_a().await;
    let a = find_node(&nodes, "A");
    let requested = requested_from(&plan_on(a).await);

    // The admin moves one topic by hand between the plan and the apply.
    let svc = Arc::clone(&a.svc);
    tokio::task::spawn_blocking(move || {
        let run = RunContext {
            actor: Some("test"),
            reason: "manual",
        };
        svc.move_topic_leadership(run, ORG, "t4", "B")
    })
    .await
    .expect("move task")
    .expect("manual move");
    let transfers = audit_details_of(a, "bus.leader.transfer").len();

    let svc = Arc::clone(&a.svc);
    let err = tokio::task::spawn_blocking(move || {
        let run = RunContext {
            actor: Some("test"),
            reason: "rebalance",
        };
        svc.leader_balance_apply(ORG, run, &requested, &|_, _| true)
    })
    .await
    .expect("apply task")
    .expect_err("stale plan");
    assert!(matches!(err, LeaderBalanceError::PlanChanged), "{err:?}");
    assert_eq!(audit_details_of(a, "bus.leader.transfer").len(), transfers);
    let _ = topics;
    shutdown_all(&nodes);
}

/// A transfer made right after a burst of `acks=leader` writes leaves the new
/// leader with exactly the records the old one acknowledged. This harness
/// cannot hold a follower back, so it does not prove the wait for a lagging
/// target; that is the manager's `a_transfer_closes_writes_first_and_proposes_\
/// only_after_the_final_log_end_is_replicated`, and the glue's close test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_right_after_a_burst_of_acked_writes_leaves_the_new_leader_every_record() {
    let (nodes, _ledger, _registry, _topics) = four_topics_led_by_a().await;
    let a = find_node(&nodes, "A");
    let b = find_node(&nodes, "B");
    for i in 0..300 {
        publish_on(&a.svc, TOPIC, &format!("burst-{i}"))
            .await
            .expect("publish");
    }
    let before = payloads_of(a, TOPIC);
    assert_eq!(before.len(), 300);

    let manager = Arc::clone(&a.manager);
    tokio::task::spawn_blocking(move || manager.transfer_leader(ORG, TOPIC, 0, "B"))
        .await
        .expect("transfer task")
        .expect("transfer");
    assert_eq!(
        payloads_of(b, TOPIC),
        before,
        "the new leader must hold exactly what the old one acknowledged"
    );
    shutdown_all(&nodes);
}

/// A transfer whose proposal is never admitted (lost on its way to the
/// ledger): the leader cannot tell, so it reports the outcome as unknown and
/// stays stopped, and the partition is led again by the election the other
/// replica runs when the old leader falls silent: a new term, from the replica
/// that holds every record the old leader acknowledged. Nothing is lost and
/// the old epoch is never resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_whose_proposal_is_never_admitted_is_unknown_and_the_partition_is_elected_a_leader(
) {
    // Three replicas: the proposer alone is not a quorum, so a proposal that
    // gets no acknowledgement is not admitted (at two replicas the proposer
    // alone admits it).
    let (nodes, ledger, _registry) =
        build_cluster_placed(1, Acks::Leader, &["A", "B", "C"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    assert!(
        wait_isr_full(&nodes, TOPIC, "A", &["A", "B", "C"]).await,
        "the ISR never filled"
    );
    let a = find_node(&nodes, "A");
    for i in 0..50 {
        publish_on(&a.svc, TOPIC, &format!("kept-{i}"))
            .await
            .expect("publish");
    }
    let acknowledged = payloads_of(a, TOPIC);
    assert_eq!(acknowledged.len(), 50);
    let old_epoch = match a.manager.role(ORG, TOPIC, 0) {
        PartitionRole::Leader { epoch } => epoch,
        other => panic!("A must lead, got {other:?}"),
    };

    ledger.drop_proposals_from(Some("A"));
    let manager = Arc::clone(&a.manager);
    let outcome = tokio::task::spawn_blocking(move || manager.transfer_leader(ORG, TOPIC, 0, "B"))
        .await
        .expect("transfer task");
    assert!(
        matches!(outcome, Err(ReplError::TransferOutcomeUnknown { .. })),
        "got {outcome:?}"
    );
    ledger.drop_proposals_from(None);

    // Within a bound, one node leads again, at a newer term.
    let started = std::time::Instant::now();
    assert!(
        wait_until(Duration::from_secs(45), || leader_of(&nodes, TOPIC)
            .is_some())
        .await,
        "nobody leads the partition again: {:?}",
        nodes
            .iter()
            .map(|n| (n.id.clone(), n.manager.role(ORG, TOPIC, 0)))
            .collect::<Vec<_>>()
    );
    let leader_id = leader_of(&nodes, TOPIC).expect("one leader");
    let leader = find_node(&nodes, &leader_id);
    let new_epoch = match leader.manager.role(ORG, TOPIC, 0) {
        PartitionRole::Leader { epoch } => epoch,
        other => panic!("{other:?}"),
    };
    assert!(
        new_epoch > old_epoch,
        "the old term {old_epoch} was resumed ({new_epoch})"
    );
    eprintln!(
        "re-elected {leader_id} at epoch {new_epoch} after {:?}",
        started.elapsed()
    );

    // Nothing acknowledged is lost, and the partition takes writes again.
    let held = payloads_of(leader, TOPIC);
    assert_eq!(&held[..acknowledged.len()], acknowledged.as_slice());
    let mut written = false;
    for _ in 0..50 {
        if publish_on(&leader.svc, TOPIC, "after").await.is_ok() {
            written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(written, "the new leader never accepted a write");
    shutdown_all(&nodes);
}

/// A transfer asked of a node that does not lead the partition runs on the
/// leader, whichever member of the instance asks: a replica, and a node that
/// holds no replica of the topic. What the leader refuses reaches the caller
/// as that refusal, not as a generic internal error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_asked_of_any_member_runs_on_the_leader_and_a_refusal_keeps_its_reason() {
    let (nodes, _ledger, _registry, _topics) = four_topics_led_by_a().await;
    let c = find_node(&nodes, "C");

    // C holds no replica of the topic; the target it names is not one either.
    let manager = Arc::clone(&c.manager);
    let refused = tokio::task::spawn_blocking(move || manager.transfer_leader(ORG, TOPIC, 0, "C"))
        .await
        .expect("transfer task")
        .expect_err("a target outside the replica set is refused");
    assert!(
        matches!(refused, ReplError::NotAReplica { .. }),
        "the leader's reason must reach the caller, got {refused:?}"
    );
    assert_eq!(leader_of(&nodes, TOPIC).as_deref(), Some("A"));

    // The same node, asking for a replica, is carried out by the leader.
    let manager = Arc::clone(&c.manager);
    let epoch = tokio::task::spawn_blocking(move || manager.transfer_leader(ORG, TOPIC, 0, "B"))
        .await
        .expect("transfer task")
        .expect("a member that holds no replica may ask the leader");
    assert!(epoch > 1);
    assert!(
        wait_until(Duration::from_secs(10), || leader_of(&nodes, TOPIC)
            .as_deref()
            == Some("B"))
        .await,
        "B never took over"
    );
    shutdown_all(&nodes);
}

// ===== K6: the topic goes back to its preferred node ======================

/// What the node's background loop (`init::spawn_leader_return_loop`) does,
/// against this suite's node.
fn spawn_leader_return_loop(node: &TestNode) {
    let svc = Arc::clone(&node.svc);
    let shutdown = node.manager.shutdown_token();
    tokio::spawn(async move {
        loop {
            let pause = svc.leader_return_timing().sample_interval;
            let tick = Arc::clone(&svc);
            let stopping = shutdown.clone();
            let _ = tokio::task::spawn_blocking(move || {
                tick.leader_return_tick_at(std::time::Instant::now(), &|| stopping.is_cancelled())
            })
            .await;
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(pause) => {}
            }
        }
    });
}

const RETURN_STABLE_FOR: Duration = Duration::from_secs(2);

fn quick_return_timing() -> tentaflow_core::bus::leader_return::LeaderReturnTiming {
    tentaflow_core::bus::leader_return::LeaderReturnTiming {
        stable_for: RETURN_STABLE_FOR,
        decision_interval: Duration::from_millis(300),
        sample_interval: Duration::from_millis(100),
        idle_interval: Duration::from_millis(100),
    }
}

/// The preferred node leads, crashes, and a survivor takes over; the topic
/// stays with the survivor while the preferred node is away, and goes back to
/// it only after it has been back, in the leader's live in-sync set, for the
/// stable time. A third node writes through forwarding the whole time: every
/// write it was told was accepted is in the log of the preferred node after
/// the return, and the replica order still names the preferred node first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_topic_that_failed_over_goes_back_to_its_preferred_node_once_it_has_stayed_in_sync() {
    let (mut nodes, ledger, registry) =
        build_cluster_placed(1, Acks::Leader, &["A", "B", "C"], "A").await;
    for node in &nodes {
        spawn_background_loops(node);
    }
    assert!(
        wait_isr_full(&nodes, TOPIC, "A", &["A", "B", "C"]).await,
        "the ISR never filled"
    );
    let a = find_node(&nodes, "A");
    for i in 0..20 {
        publish_on(&a.svc, TOPIC, &format!("pre-{i}"))
            .await
            .expect("publish before the crash");
    }
    assert!(
        wait_until(Duration::from_secs(10), || {
            log_end_offset(find_node(&nodes, "B"), 0) >= 20
                && log_end_offset(find_node(&nodes, "C"), 0) >= 20
        })
        .await,
        "the followers never caught up"
    );

    // A crashes (the same way `crashed_leader_without_graceful_stop...` does).
    registry.peers.lock().remove("A");
    let a_pos = nodes.iter().position(|n| n.id == "A").expect("node A");
    let crashed = nodes.remove(a_pos);
    crashed.manager.shutdown();
    drop(crashed);

    assert!(
        wait_until(Duration::from_secs(20), || leader_of(&nodes, TOPIC)
            .is_some())
        .await,
        "no survivor took over"
    );
    let survivor = leader_of(&nodes, TOPIC).expect("one leader");
    assert_ne!(survivor, "A");
    let writer_node = nodes
        .iter()
        .find(|n| n.id != survivor)
        .map(|n| n.id.clone())
        .expect("the other survivor");
    for node in &nodes {
        node.svc.set_leader_return_timing(quick_return_timing());
        spawn_leader_return_loop(node);
    }

    // The writer goes through the node that does not lead.
    let acked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let svc = Arc::clone(&find_node(&nodes, &writer_node).svc);
        let (acked, stop) = (Arc::clone(&acked), Arc::clone(&stop));
        tokio::spawn(async move {
            let mut i = 0usize;
            while !stop.load(Ordering::SeqCst) {
                let payload = format!("w{i}");
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let attempt = tokio::time::timeout(
                        Duration::from_secs(3),
                        publish_on(&svc, TOPIC, &payload),
                    )
                    .await;
                    if matches!(attempt, Ok(Ok(ref r)) if r.accepted == 1) {
                        acked.lock().push(payload);
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                i += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };

    // The preferred node is away: well past the stable time, nothing returns.
    tokio::time::sleep(RETURN_STABLE_FOR * 2).await;
    assert_eq!(
        leader_of(&nodes, TOPIC).as_deref(),
        Some(survivor.as_str()),
        "nothing to return to while the preferred node is away"
    );

    // A comes back with nothing on its disk, and is caught up by the leader.
    let back = build_node(
        "A",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    let row = tentaflow_core::db::repository::bus_topic_get(
        &find_node(&nodes, &survivor).db,
        TEST_INSTANCE_ID,
        ORG,
        TOPIC,
    )
    .expect("read topic row")
    .expect("topic row");
    tentaflow_core::db::repository::bus_topic_create(&back.db, &row).expect("topic row on A");
    back.svc.set_leader_return_timing(quick_return_timing());
    spawn_background_loops(&back);
    spawn_leader_return_loop(&back);
    nodes.push(back);

    let in_sync_since = {
        let leader = find_node(&nodes, &survivor);
        let joined = wait_until(Duration::from_secs(30), || {
            leader
                .manager
                .snapshot(ORG, Some(TOPIC))
                .partitions
                .iter()
                .any(|p| p.isr.iter().any(|n| n == "A"))
        })
        .await;
        assert!(joined, "A never rejoined the in-sync set");
        tokio::time::Instant::now()
    };
    assert_eq!(
        leader_of(&nodes, TOPIC).as_deref(),
        Some(survivor.as_str()),
        "not returned the moment the node is back"
    );

    assert!(
        wait_until(Duration::from_secs(40), || leader_of(&nodes, TOPIC)
            .as_deref()
            == Some("A"))
        .await,
        "the topic never went back to A: {:?}",
        nodes
            .iter()
            .map(|n| (n.id.clone(), n.manager.role(ORG, TOPIC, 0)))
            .collect::<Vec<_>>()
    );
    // The clock starts at the first sample that saw A in sync, which may be
    // one sample period before the poll above did.
    assert!(
        in_sync_since.elapsed() + quick_return_timing().sample_interval * 2 >= RETURN_STABLE_FOR,
        "it went back before A had been in sync for the stable time"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, Ordering::SeqCst);
    writer.await.expect("writer");

    let acked = acked.lock().clone();
    assert!(!acked.is_empty(), "the writer never got a write accepted");
    let a = find_node(&nodes, "A");
    let held = payloads_of(a, TOPIC);
    for i in 0..20 {
        assert!(held.contains(&format!("pre-{i}")), "pre-{i} was lost");
    }
    for payload in &acked {
        assert!(
            held.contains(payload),
            "{payload} was accepted and is not in the log of A"
        );
    }

    let rows = a
        .ledger
        .list_for_node(TEST_INSTANCE_ID, "A")
        .expect("rows of A");
    let row = rows
        .iter()
        .find(|r| r.topic == TOPIC && r.partition == 0)
        .expect("row of the topic");
    assert_eq!(row.replicas, vec!["A", "B", "C"], "the order is as placed");
    assert_eq!(row.leader_node_id, "A");

    let leader_node = find_node(&nodes, &survivor);
    let transfers = audit_details_of(leader_node, "bus.leader.transfer");
    assert!(
        transfers
            .iter()
            .any(|d| d.contains("reason=preferred_restore") && d.contains("target_node_id=A")),
        "{transfers:?}"
    );
    assert_eq!(
        audit_details_of(leader_node, "bus.leader.rebalance").len(),
        1,
        "one aggregate row"
    );
    shutdown_all(&nodes);
}

/// Who leads partition `partition` of `topic`, if exactly one node says so.
fn leader_of_partition(nodes: &[TestNode], topic: &str, partition: u32) -> Option<String> {
    let leaders: Vec<&TestNode> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.manager.role(ORG, topic, partition),
                PartitionRole::Leader { .. }
            )
        })
        .collect();
    (leaders.len() == 1).then(|| leaders[0].id.clone())
}

/// Every partition of the unit (two of `orders` and its DLQ) has one leader
/// and it is `node`.
fn unit_led_by(nodes: &[TestNode], node: &str) -> bool {
    [(TOPIC, 0), (TOPIC, 1), ("__dlq.orders", 0)]
        .iter()
        .all(|(t, p)| leader_of_partition(nodes, t, *p).as_deref() == Some(node))
}

/// The preferred node A is away and the survivors share the unit: B leads
/// partition 0, C leads partition 1 and the DLQ. The driver (B, the lowest
/// leader) gathers the unit at once, with no wait for the stable time, onto
/// C that leads most of it, and does not rewrite the replica order. When A is
/// back and has stayed in sync, C returns the whole unit to A.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_topic_split_by_a_failover_is_gathered_at_once_and_goes_back_to_its_preferred_node() {
    let stable_for = Duration::from_secs(4);
    let timing = tentaflow_core::bus::leader_return::LeaderReturnTiming {
        stable_for,
        ..quick_return_timing()
    };
    let (mut nodes, ledger, registry) =
        build_cluster_led_by(2, Acks::Leader, &["A", "B", "C"], |p| {
            if p == 0 { "B" } else { "C" }.to_string()
        })
        .await;
    place_internal_topic_on(&nodes, &ledger, ORG, "__dlq.orders", "C", &["A", "B", "C"]).await;

    let a_pos = nodes.iter().position(|n| n.id == "A").expect("node A");
    registry.peers.lock().remove("A");
    let away = nodes.remove(a_pos);
    away.manager.shutdown();
    drop(away);
    assert!(
        !unit_led_by(&nodes, "B") && !unit_led_by(&nodes, "C"),
        "the unit starts out split"
    );
    for node in &nodes {
        spawn_background_loops(node);
        node.svc.set_leader_return_timing(timing);
        spawn_leader_return_loop(node);
    }

    let split_since = tokio::time::Instant::now();
    assert!(
        wait_until(Duration::from_secs(20), || unit_led_by(&nodes, "C")).await,
        "the split unit was never gathered: {:?}",
        [(TOPIC, 0), (TOPIC, 1), ("__dlq.orders", 0)]
            .iter()
            .map(|(t, p)| leader_of_partition(&nodes, t, *p))
            .collect::<Vec<_>>()
    );
    assert!(
        split_since.elapsed() < stable_for,
        "a split topic is gathered promptly, not after the stable time"
    );
    // The driver writes its audit row after the move is done, so the unit can
    // already be led by C a moment before the row exists.
    let driver = find_node(&nodes, "B");
    let gathered_by_split_gather = || {
        audit_details_of(driver, "bus.leader.transfer")
            .iter()
            .any(|d| d.contains("reason=split_gather") && d.contains("target_node_id=C"))
    };
    assert!(
        wait_until(Duration::from_secs(5), gathered_by_split_gather).await,
        "{:?}",
        audit_details_of(driver, "bus.leader.transfer")
    );
    for (topic, partition) in [(TOPIC, 0), (TOPIC, 1), ("__dlq.orders", 0)] {
        let rows = find_node(&nodes, "C")
            .ledger
            .list_for_node(TEST_INSTANCE_ID, "C")
            .expect("rows of C");
        let row = rows
            .iter()
            .find(|r| r.topic == topic && r.partition == partition)
            .expect("row");
        assert_eq!(
            row.replicas,
            vec!["A", "B", "C"],
            "the gather leaves the order alone: {topic}/{partition}"
        );
    }

    // A comes back with nothing on its disk.
    let back = build_node(
        "A",
        NodeEnvironment::Prod,
        Arc::clone(&ledger),
        Arc::clone(&registry),
    );
    for topic in [TOPIC, "__dlq.orders"] {
        let row = tentaflow_core::db::repository::bus_topic_get(
            &find_node(&nodes, "C").db,
            TEST_INSTANCE_ID,
            ORG,
            topic,
        )
        .expect("read topic row")
        .expect("topic row");
        tentaflow_core::db::repository::bus_topic_create(&back.db, &row).expect("topic row on A");
    }
    back.svc.set_leader_return_timing(timing);
    spawn_background_loops(&back);
    spawn_leader_return_loop(&back);
    nodes.push(back);

    let in_sync_since = {
        let leader = find_node(&nodes, "C");
        assert!(
            wait_until(Duration::from_secs(30), || {
                leader
                    .manager
                    .snapshot(ORG, None)
                    .partitions
                    .iter()
                    .all(|p| {
                        p.leader_node_id.as_deref() != Some("C") || p.isr.iter().any(|n| n == "A")
                    })
            })
            .await,
            "A never rejoined the in-sync set"
        );
        tokio::time::Instant::now()
    };
    assert!(
        wait_until(Duration::from_secs(40), || unit_led_by(&nodes, "A")).await,
        "the unit never went back to A"
    );
    assert!(
        in_sync_since.elapsed() + timing.sample_interval * 2 >= stable_for,
        "it went back before A had been in sync for the stable time"
    );
    let returned_to_preferred = || {
        audit_details_of(find_node(&nodes, "C"), "bus.leader.transfer")
            .iter()
            .any(|d| d.contains("reason=preferred_restore") && d.contains("target_node_id=A"))
    };
    assert!(
        wait_until(Duration::from_secs(5), returned_to_preferred).await,
        "{:?}",
        audit_details_of(find_node(&nodes, "C"), "bus.leader.transfer")
    );
    shutdown_all(&nodes);
}
