// =============================================================================
// File: bus/replication/manager.rs — ReplicationManager (PLAN-M2 §1b)
// =============================================================================
//
// `ReplicationManager` is the `bus::ReplicationCoordinator` implementor
// (PLAN-M2 §1e): the partition registry, the ALPN_BUS dial/accept
// lifecycle, and the glue that drives `election::PromotionState` from real
// (or, in tests, faked) I/O. Everything this file needs from `leader.rs`
// (agent RL), `follower.rs` (agent RF), and the ledger/assignment stack
// (agent L) is behind a narrow trait defined HERE, not a concrete type —
// none of those files exist yet in this build. Real implementations plug
// in later without this file changing:
//
//   `LeaderHandle` / `LeaderHandleFactory`     -> RL's `leader.rs`
//   `FollowerRunner` / `FollowerRunnerFactory` -> RF's `follower.rs`
//   `AssignmentStore`                          -> agent L (ledger capture,
//                                                  `db/repository.rs` bus_*)
//   `LedgerAdmission`                          -> `FjallLedgerAdmission`
//                                                  below IS the real impl —
//                                                  `SyncLedgerStore::
//                                                  list_outbox_for_operation`
//                                                  (PLAN-M2 §1c) was already
//                                                  a straightforward fit.
//   `ReplAudit`                                -> agent S/L (audit_log rows)
//
// Dial direction (resolved here, since it is not literally spelled out
// anywhere frozen): the LEADER's manager dials every OTHER replica
// (`Transport::open_stream`, matching `IrohMeshManager::connect_bus`'s doc
// — "calls this once per (org, topic, partition, follower) stream it needs
// to establish"); the FOLLOWER side never dials, it only accepts — the
// accept path (`accept_stream`) reads the first frame (`ReplHello`) and
// routes it to that partition's `FollowerRunner`. This is the only
// self-consistent reading of "accept handler ... routes to the right
// partition's follower runner": if the follower dialed instead, an
// accepted connection would belong to the LEADER side, not a follower
// runner. `LeoQuery`/`LeoReply` during an election are the one exception —
// the CANDIDATE dials every other replica directly for those, regardless
// of normal leader/follower roles (K-M2-3).
//
// `LeaderHandle`'s job on `spawn` folds together three of the state
// machine's actions (`SetLeaderEpoch`, `StartFeeders`, and implicitly
// "open the local partition") into one call: opening the local `Partition`
// is an engine concern this file has no business owning (agent E2), so the
// concrete `LeaderHandle` (RL, wave 2) is expected to do it internally
// when `LeaderHandleFactory::spawn` is called with the now-current
// assignment.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tentaflow_protocol::environment::NodeEnvironment;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::bus::replication::assignment::{PartitionAssignment, SqliteLedgerAssignmentStore};
use crate::bus::replication::election::{
    self, LocalRole, PromotionAction, PromotionEvent, PromotionState,
};
use crate::bus::replication::frames::{
    self, ReplFrame, ReplHello, ReplHelloAck, ReplLeoQuery, ReplLeoReply, ReplReject,
};
use crate::bus::topics::Acks;
use crate::bus::{
    AckOutcome, PartitionReplicaInfo, PartitionRole, ReplError, ReplicaLagInfo, ReplicaNodeInfo,
    ReplicationCoordinator, ReplicationSnapshot, UnavailableReason,
};
use crate::mesh::iroh_manager::{BusAcceptHandler, IrohMeshManager};
use crate::sync::ledger::{OperationId, SyncLedgerStore};

/// `(org_id, topic, partition)` — the registry's key everywhere in this
/// file.
pub type PartitionKey = (String, String, u32);

/// One side of a replication stream, already split so callers never touch
/// `iroh::endpoint::{SendStream,RecvStream}` directly — real streams
/// (`IrohTransport`) and test duplexes (`tokio::io::split`) both produce
/// this shape.
pub type BusRecv = Box<dyn AsyncRead + Unpin + Send>;
pub type BusSend = Box<dyn AsyncWrite + Unpin + Send>;

/// How long a `transfer_leader` admin call waits for majority admission
/// before giving up. Not pinned by PLAN-M2; chosen generous (an admin
/// action, not the hot path) so a merely-slow outbox drain still succeeds
/// within one call.
const TRANSFER_MAJORITY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// How long `accept_stream` holds a `ReplHello` for a partition this node's
/// registry does not know yet, waiting for the ledger-materialized
/// assignment row to appear — see `ReplicationManager::await_local_assignment`.
/// Two full `init::ASSIGNMENT_POLL_INTERVAL` ticks plus margin: long enough
/// that a leader's dial never races this node's own materialization poll,
/// short enough that a Hello for a partition nobody ever assigns here still
/// gets a definite answer (and pins the leader's stream for seconds, not
/// indefinitely).
pub const ASSIGNMENT_AWAIT: Duration = Duration::from_millis(2_000);

/// `await_local_assignment`'s own re-read tick, for the case where nothing
/// local bumps `assignments_changed` (the row was materialized by the
/// ledger's own apply path between two poll ticks — exactly the race this
/// exists for).
const ASSIGNMENT_AWAIT_RETRY: Duration = Duration::from_millis(100);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ===== Narrow traits other agents' wave-2 concrete types implement ========

/// Opens one bidi replication stream to `node_id`. The real implementor
/// (`IrohTransport`, below) wraps `IrohMeshManager::connect_bus` + one
/// `open_bi()`; tests use an in-memory duplex fake — this trait is the
/// only thing standing between the two, so nothing else in this file
/// depends on `iroh` types.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError>;
}

/// PLAN-M2 §1c's majority-admission proof, abstracted: "liczba wpisów
/// `acknowledged == true` dla targetów z `replicas`". `admitted_by`
/// returns the node ids the ledger's outbox reports as acknowledged for
/// `op_id` — never including the local node itself (the op is local by
/// definition; `election::admitted_by_majority` adds self back in).
pub trait LedgerAdmission: Send + Sync {
    fn admitted_by(&self, op_id: OperationId) -> Vec<String>;
}

/// Partition assignment read/propose, abstracted over the ledger round
/// trip (K-M2-4: capture -> ledger -> materializer, never a direct write —
/// there is deliberately no local-only write method on this trait). The
/// materialized row reaches this node's registry through READS of this
/// trait, never a callback from `sync/`: `init.rs`'s materialization poll
/// (`list_for_node`) and, for the one caller that cannot afford to wait for
/// that poll's next tick, `ReplicationManager::await_local_assignment`
/// (`get`) on the inbound-Hello path. Both funnel into
/// `ReplicationManager::apply_assignment`, which is the only place local
/// registry state actually changes.
/// Signatures mirror `assignment::SqliteLedgerAssignmentStore`'s inherent
/// methods 1:1 (agent L's real implementation, already landed) so `impl
/// AssignmentStore for SqliteLedgerAssignmentStore` below is a trivial
/// forwarding impl rather than a translation layer.
pub trait AssignmentStore: Send + Sync {
    fn get(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError>;
    fn list_for_topic(&self, org: &str, topic: &str)
        -> Result<Vec<PartitionAssignment>, ReplError>;
    fn list_for_node(&self, node_id: &str) -> Result<Vec<PartitionAssignment>, ReplError>;
    /// Submits `assignment` as a new ledger operation, returning its id for
    /// `LedgerAdmission::admitted_by` polling.
    fn propose(&self, assignment: PartitionAssignment) -> Result<OperationId, ReplError>;
}

/// This node's leader-role state for one partition (RL's `leader.rs`,
/// wave 2). `spawn` (on `LeaderHandleFactory`) is expected to have already
/// opened the local `Partition`, stamped `leader_epoch`, and started
/// feeding `replica_streams` before returning — the promotion state
/// machine's `SetLeaderEpoch`/`StartFeeders` actions both collapse into
/// this one call (see module header).
pub trait LeaderHandle: Send + Sync {
    /// Live ISR membership (this node included) — PLAN-M2 §1e/K-M2-2:
    /// `preflight`'s `min_isr` gate and `snapshot()` both read THIS, not
    /// the static `PartitionAssignment.isr` the ledger last materialized,
    /// so a follower dropping out of (or rejoining) the ISR is visible
    /// immediately rather than only after the next ledger round trip.
    fn isr(&self) -> Vec<String>;
    /// Replicas of this partition currently OUTSIDE the live ISR, each
    /// with a human-readable reason (K-M2-2/PLAN-M2 §1f's
    /// `BusReplicaLagWire` UI surface). `Vec::new()` (the default here) is
    /// the right answer for any `LeaderHandle` with nothing more specific
    /// to report — only the real `GlueLeaderHandle` (backed by
    /// `PartitionLeader`'s own per-follower lag/ack-staleness bookkeeping)
    /// overrides this.
    fn lagging(&self) -> Vec<ReplicaLagInfo> {
        Vec::new()
    }
    fn high_watermark(&self) -> u64;
    fn log_end_offset(&self) -> u64;
    /// Blocks (up to `timeout`) until `next_offset` is acknowledged by
    /// `required` replicas (this node included).
    fn await_acks(&self, next_offset: u64, required: u32, timeout: Duration) -> AckOutcome;
    /// K-M2-5: records a consumer group's offset commit for `ReplOffsets`
    /// coalescing.
    fn note_offset_commit(&self, group: &str, partition: u32, offset: u64, attempts: u32);
    /// K-M2-1: truncates `node`'s tail down to `to_offset` (a replica ahead
    /// of the new leader's own `leo` — see `election.rs`'s header).
    fn send_truncate(&self, node: &str, to_offset: u64);
    fn stop(&self);
}

/// This node's follower-role state for one partition (RF's `follower.rs`,
/// wave 2) — one instance per accepted leader stream.
pub trait FollowerRunner: Send + Sync {
    fn leo(&self) -> u64;
    fn hw(&self) -> u64;
    /// `leader_lease_ms = 3000` watchdog (PLAN §4.3) — `manager.rs` polls
    /// this rather than tracking heartbeat timestamps itself, so the
    /// watchdog stays entirely RF's concern. Also `true` once the stream
    /// itself has died on a transport error: a leader that closed the
    /// connection is not going to refresh anything either, and this flag is
    /// the only thing that makes `check_leases` notice (`GlueFollowerFactory::
    /// spawn`'s exit match says why).
    fn lease_expired(&self) -> bool;
    /// `IrohMeshEvent::PeerDisconnected` accelerator (PLAN-M2 §1b) — a
    /// hint, not a verdict: the real lease timer is still authoritative,
    /// this just lets a runner treat the lease as expired sooner than
    /// 3000 ms when the transport already knows the leader is gone.
    fn mark_leader_disconnected(&self);
    /// The `ReplReject` reason of the most recent refused `Hello` on this
    /// partition's follower side, if any — the accept path's own record of
    /// WHY the last leader dial-in was turned away (P8 diagnosis aid: the
    /// epoch a NEWER leader advertised in that Hello is the epoch this node
    /// must adopt/fence to, and the reason distinguishes a genuine fencing
    /// from a stale probe). `None` (the default) means "nothing was ever
    /// refused on this entry" — the right answer for any runner that never
    /// rejects.
    fn last_hello_reject(&self) -> Option<ReplReject> {
        None
    }
    fn stop(&self);
}

pub trait LeaderHandleFactory: Send + Sync {
    fn spawn(
        &self,
        assignment: &PartitionAssignment,
        replica_streams: Vec<(String, BusRecv, BusSend)>,
    ) -> Result<Box<dyn LeaderHandle>, ReplError>;
    /// Same contract as `spawn`, but the factory MAY defer stamping the
    /// local `leader_epoch` (an engine writer-thread round trip that
    /// persists meta) until after this call returns — the promotion path
    /// uses it because it runs on the manager's async task while peers are
    /// already waiting out an election, and blocking that task on the
    /// partition's writer mutex can hold an inbound stream's accept or the
    /// materialization poll hostage for the whole round trip. The wire
    /// protocol is unaffected either way: the `ReplHello` a feeder sends
    /// carries the LEADER-side epoch (the `PartitionLeader` field), and a
    /// follower's engine epoch is stamped by its own `Hello` handling
    /// before any `Batch` can arrive. Default: identical to `spawn` (the
    /// synchronous stamp), so every existing implementor and test fake
    /// keeps working unchanged.
    fn spawn_deferred(
        &self,
        assignment: &PartitionAssignment,
        replica_streams: Vec<(String, BusRecv, BusSend)>,
    ) -> Result<Box<dyn LeaderHandle>, ReplError> {
        self.spawn(assignment, replica_streams)
    }
}

pub trait FollowerRunnerFactory: Send + Sync {
    /// `hello` is the SAME `ReplHello` `accept_stream` already read off
    /// `leader_recv` to decide routing (wave-3, agent G2's fix for the
    /// double-Hello-read bug T1 found end-to-end: the leader sends exactly
    /// one `Hello` per stream, so a factory that read another one off
    /// `leader_recv` itself would block forever). An implementor drives
    /// the rest of the stream via `follower::run_follower_stream_with_hello`
    /// rather than `run_follower_stream`.
    fn spawn(
        &self,
        assignment: &PartitionAssignment,
        hello: ReplHello,
        leader_recv: BusRecv,
        leader_send: BusSend,
    ) -> Result<Box<dyn FollowerRunner>, ReplError>;
}

/// Audit hooks (PLAN §8.2: `bus.leader.failover`, `bus.leader.transfer`,
/// `bus.replica.evicted_env_change`). Real implementation (agent S/L)
/// writes `repository::log_audit` rows; kept as a trait so this file never
/// depends on `db::repository` directly.
pub trait ReplAudit: Send + Sync {
    /// `reason` is the `bus.leader.failover` audit contract's trailing
    /// `reason=<token>` field (`dispatch/bus.rs`'s `BUS_FAILOVER_AUDIT_
    /// ACTION` doc, agent P) — added here (M2 wave 2, agent G) rather than
    /// hardcoded by the implementor, since this file is the only caller
    /// that knows WHY a promotion happened; today that is always
    /// `"lease_expired"` (`execute_promotion_actions`'s only trigger is
    /// `run_election`, itself only ever driven by `PromotionEvent::
    /// LeaseExpired` — no other promotion trigger exists in this build).
    #[allow(clippy::too_many_arguments)]
    fn failover(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        from_node: Option<&str>,
        to_node: &str,
        from_epoch: u32,
        to_epoch: u32,
        duration_ms: u64,
        reason: &str,
    );
    fn transfer(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        from_node: &str,
        to_node: &str,
        epoch: u32,
    );
    fn evicted(&self, node_id: &str, reason: &str, count: u32);
}

// ===== Registry entry =======================================================

struct PartitionEntry {
    assignment: PartitionAssignment,
    role: LocalRole,
    leader: Option<Box<dyn LeaderHandle>>,
    follower: Option<Box<dyn FollowerRunner>>,
    promotion: PromotionState,
}

// ===== ReplicationManager ===================================================

pub struct ReplicationManagerConfig {
    pub local_node_id: String,
    pub local_env: NodeEnvironment,
    pub transport: Arc<dyn Transport>,
    pub ledger: Arc<dyn LedgerAdmission>,
    pub assignments: Arc<dyn AssignmentStore>,
    pub leader_factory: Arc<dyn LeaderHandleFactory>,
    pub follower_factory: Arc<dyn FollowerRunnerFactory>,
    pub audit: Arc<dyn ReplAudit>,
    /// K-M2-3 default is `election::LEO_QUERY_TIMEOUT` (300 ms); overridable
    /// so tests are not forced to wait the real budget.
    pub leo_query_timeout: Duration,
    /// Default `election::MAJORITY_AWAIT_TIMEOUT` (1.5 s); see above.
    pub majority_await_timeout: Duration,
}

pub struct ReplicationManager {
    local_node_id: String,
    local_env: NodeEnvironment,
    registry: DashMap<PartitionKey, PartitionEntry>,
    transport: Arc<dyn Transport>,
    ledger: Arc<dyn LedgerAdmission>,
    assignments: Arc<dyn AssignmentStore>,
    leader_factory: Arc<dyn LeaderHandleFactory>,
    follower_factory: Arc<dyn FollowerRunnerFactory>,
    audit: Arc<dyn ReplAudit>,
    leo_query_timeout: Duration,
    majority_await_timeout: Duration,
    /// M2 wave 2 (agent G, `init.rs`): every background task `replication::
    /// init` spawns against this manager (lease-check loop, mesh-event
    /// forwarding, ledger-materialization poll) watches this token rather
    /// than owning its own — `shutdown()` cancels it once, and every task
    /// (regardless of which one spawned it or in what order) observes the
    /// cancellation on its next `select!` iteration. Also cheaply cloned
    /// out via `shutdown_token()` for a caller (`init.rs`) that wants to
    /// race its own loop against the same signal without reaching into
    /// this struct's private fields.
    shutdown: CancellationToken,
    /// Bumped by `apply_assignment` on every call that actually changed the
    /// registry. `await_local_assignment` waits on this so a Hello parked on
    /// a not-yet-applied assignment is admitted the instant `init.rs`'s poll
    /// loop (or a local `create_topic`) applies it, rather than up to
    /// `ASSIGNMENT_AWAIT_RETRY` later.
    assignments_changed: watch::Sender<()>,
    /// M2 (PLAN §8.4): running total of times a partition's ISR membership
    /// shrank, across every partition this manager touches — feeds
    /// `tentaflow_bus_isr_shrink_total`. Bumped from `evict_node_from_
    /// replica_sets` (a node dropped entirely) and `reassign` (an admin
    /// replica-set change that narrows the ISR): those are the only two
    /// places this manager itself removes a member from an assignment's
    /// `isr` (as opposed to a `LeaderHandle`'s own live-ISR bookkeeping,
    /// which this registry does not own and has no single choke point to
    /// observe from here). A follower dropping out of the LIVE ISR without
    /// ever being evicted or reassigned out (e.g. falling behind on lag) is
    /// therefore not counted — an honest undercount rather than a
    /// fabricated precise one.
    isr_shrink_total: AtomicU64,
}

fn reject_ack(environment: NodeEnvironment, reject: ReplReject) -> ReplHelloAck {
    ReplHelloAck {
        accepted: false,
        follower_leo: 0,
        follower_hw: 0,
        follower_epoch: 0,
        environment,
        reject: Some(reject),
    }
}

impl ReplicationManager {
    pub fn new(config: ReplicationManagerConfig) -> Arc<Self> {
        Arc::new(Self {
            local_node_id: config.local_node_id,
            local_env: config.local_env,
            registry: DashMap::new(),
            transport: config.transport,
            ledger: config.ledger,
            assignments: config.assignments,
            leader_factory: config.leader_factory,
            follower_factory: config.follower_factory,
            audit: config.audit,
            leo_query_timeout: config.leo_query_timeout,
            majority_await_timeout: config.majority_await_timeout,
            shutdown: CancellationToken::new(),
            assignments_changed: watch::channel(()).0,
            isr_shrink_total: AtomicU64::new(0),
        })
    }

    pub fn local_node_id(&self) -> &str {
        &self.local_node_id
    }

    /// Cloneable cancellation signal for `init.rs`'s background tasks
    /// (lease-check loop, mesh-event forwarding, ledger-materialization
    /// poll) — see `shutdown`'s own field doc.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// `replication::stop`'s implementation: cancels every background task
    /// watching `shutdown_token()` and stops every partition this node
    /// currently leads or follows (each `LeaderHandle`/`FollowerRunner`'s
    /// own `stop()` — the glue's concrete impls, agent G — both abort
    /// their tasks and best-effort flush the partition's persisted meta).
    /// Idempotent: cancelling an already-cancelled `CancellationToken` and
    /// stopping an already-empty registry are both no-ops.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        for mut entry in self.registry.iter_mut() {
            if let Some(leader) = entry.leader.take() {
                leader.stop();
            }
            if let Some(follower) = entry.follower.take() {
                follower.stop();
            }
        }
    }

    /// Installs this manager as the mesh's `ALPN_BUS` accept handler
    /// (PLAN-M2 §1d). Every accepted connection is handed off to
    /// `handle_inbound_connection`, which loops `accept_bi()` — one
    /// `ReplHello`-prefixed bi-stream per (org, topic, partition) the
    /// leader on the other end dials in for.
    pub async fn install_accept_handler(self: &Arc<Self>, mesh: &IrohMeshManager) {
        let manager = Arc::clone(self);
        let handler: BusAcceptHandler = Arc::new(move |remote_hex, connection| {
            let manager = Arc::clone(&manager);
            tokio::spawn(manager.handle_inbound_connection(remote_hex, connection));
        });
        mesh.set_bus_accept_handler(handler).await;
    }

    pub async fn handle_inbound_connection(
        self: Arc<Self>,
        remote_hex: String,
        connection: iroh::endpoint::Connection,
    ) {
        while let Ok((send, recv)) = connection.accept_bi().await {
            let manager = Arc::clone(&self);
            let remote = remote_hex.clone();
            tokio::spawn(async move {
                manager
                    .accept_stream(remote, Box::new(recv), Box::new(send))
                    .await;
            });
        }
    }

    /// Reads the first frame of a newly accepted bi-stream and routes it:
    /// a `ReplHello` goes to the matching partition's `FollowerRunner`
    /// (rejecting with the specific `ReplReject` reason otherwise), and a
    /// `LeoQuery` — the K-M2-3 exception to the Hello-first rule, the
    /// CANDIDATE dialing this node directly for its pre-vote — is answered
    /// from this node's own replication state. Anything else is dropped
    /// silently. Split out from `handle_inbound_connection` so tests can
    /// drive it directly with an in-memory duplex, without a real `iroh`
    /// connection.
    ///
    /// The LeoQuery arm is the fix for the P8 election tie (M2-WYNIKI,
    /// "remis dwóch samoelekcji"): before it existed, a candidate's
    /// `LeoQuery` arrived on a fresh stream whose FIRST frame is not a
    /// `Hello`, hit the old `_ => return` arm, and was never answered — so
    /// every candidate saw an empty reply set, `choose_candidate` fell
    /// back to self on BOTH survivors of a crashed leader, and both
    /// proposed the same next epoch. The node-id tie-break in
    /// `election.rs` (and the materializer's identical gate) never got the
    /// chance to fire because the leo exchange itself never happened.
    ///
    /// The registry lookup is preceded by `await_local_assignment`, so a
    /// `TopicUnknown` here means "the ledger agrees I am not a replica of
    /// this partition (or never will within `ASSIGNMENT_AWAIT`)", not "my
    /// own startup is behind". The environment gate deliberately stays
    /// BEFORE that wait: a peer from another environment gets no DB reads
    /// and no hold at all (PLAN §4.4 Z12).
    pub async fn accept_stream(&self, _remote_hex: String, mut recv: BusRecv, mut send: BusSend) {
        let first = frames::read_frame(&mut recv).await;
        match first {
            Ok(ReplFrame::Hello(hello)) => {
                self.accept_hello(hello, recv, send).await;
            }
            Ok(ReplFrame::LeoQuery(query)) => {
                self.answer_leo_query(query, send).await;
            }
            _ => return,
        }
    }

    /// Answers one inbound `LeoQuery` (K-M2-3) from the registry entry for
    /// the queried partition: a `Leader` reports its own engine offsets
    /// (it is by definition caught up with itself), a `Follower` reports
    /// its follower state (`0,0` when no runner is attached — the offsets
    /// the dialing candidate gets are advisory; the candidate filters
    /// replies against its OWN last-known ISR regardless). `in_isr` is
    /// this node's last-known ISR membership per the assignment — the
    /// same advisory self-report `ReplLeoReply.in_isr` documents — never
    /// consulted by `choose_candidate` (K-M2-3: candidacy safety comes
    /// from the candidate's own ISR view). An unknown partition answers
    /// zeros so the candidate's deadline resolves instead of hanging. The
    /// connection is closed after the reply: one query, one reply, no
    /// follow-up.
    ///
    /// No environment check of its own: the `LeoQuery` frame carries no
    /// environment field, and the mesh's pre-ALPN trust/env gate
    /// (`IrohMeshManager`'s accept arm, PLAN-M2 §1d) has already fenced
    /// cross-environment connections before a stream ever reaches this
    /// function — the same trust level every other frame on this ALPN
    /// assumes.
    async fn answer_leo_query(&self, query: ReplLeoQuery, mut send: BusSend) {
        let key: PartitionKey = (query.org_id, query.topic, query.partition);
        let (leo, hw, leader_epoch, in_isr) = match self.registry.get(&key) {
            None => (0, 0, 0, false),
            Some(entry) => {
                let epoch = entry.assignment.leader_epoch;
                let in_isr = entry
                    .assignment
                    .isr
                    .iter()
                    .any(|m| m == &self.local_node_id);
                match entry.role {
                    LocalRole::Leader => match entry.leader.as_ref() {
                        Some(l) => (l.log_end_offset(), l.high_watermark(), epoch, true),
                        None => (0, 0, epoch, in_isr),
                    },
                    _ => match entry.follower.as_ref() {
                        Some(f) => (f.leo(), f.hw(), epoch, in_isr),
                        None => (0, 0, epoch, in_isr),
                    },
                }
            }
        };
        let reply = ReplFrame::LeoReply(ReplLeoReply {
            leo,
            hw,
            leader_epoch,
            in_isr,
        });
        let _ = frames::write_frame(&mut send, &reply).await;
    }

    /// The `Hello`-first half of `accept_stream` (the original body,
    /// extracted so the `LeoQuery` exception can share the entry point).
    ///
    /// The registry lookup is preceded by `await_local_assignment`, so a
    /// `TopicUnknown` here means "the ledger agrees I am not a replica of
    /// this partition (or never will within `ASSIGNMENT_AWAIT`)", not "my
    /// own startup is behind".
    async fn accept_hello(&self, hello: ReplHello, mut recv: BusRecv, mut send: BusSend) {
        if hello.environment != self.local_env {
            let ack = reject_ack(
                self.local_env,
                ReplReject::EnvironmentMismatch {
                    theirs: hello.environment,
                    ours: self.local_env,
                },
            );
            let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
            return;
        }
        let key: PartitionKey = (hello.org_id.clone(), hello.topic.clone(), hello.partition);

        // A registry miss is not yet an answer: resolve it against the
        // ledger's own row first, so a leader that dialed this node before
        // its materialization poll caught up is held, not bounced.
        self.await_local_assignment(&key).await;

        // P8 exclusive promotion, half 1 — fencing ON the Hello. A node
        // that currently serves this partition as LEADER but is staring at
        // another node's leader Hello is by definition in a two-leader
        // state (the peer would not dial unless it also believes it leads,
        // e.g. two simultaneous self-elections at the same epoch — the
        // exact signature the 3-process chaos run measured). Resolve it
        // deterministically with the SAME rule the ledger's materializer
        // gate applies (`core_materializer::apply_bus_partition_assignment`):
        // the strictly higher epoch wins, and at an equal epoch the
        // lexicographically lower leader node id wins. The LOSER fences
        // itself — stops its own leader handle, adopts the peer's view as
        // its assignment, and continues into the normal follower accept —
        // instead of answering `NotAReplica` forever, which is what turned
        // the transient dual-election into a ~48 s mutual-refusal livelock
        // where neither side ever re-formed an ISR.
        //
        // This is deliberately NOT a widening of Hello acceptance: the
        // incoming leader must still prove it claims THIS node's
        // membership (`self` in `hello.replicas` — checked here and again
        // by the follower stream's own four checks), and a peer that does
        // NOT beat this node by the deterministic rule still gets the
        // plain `NotAReplica`/`StaleEpoch` rejection it always got.
        if let Some(entry) = self.registry.get(&key) {
            let peer_wins = hello.leader_epoch > entry.assignment.leader_epoch
                || (hello.leader_epoch == entry.assignment.leader_epoch
                    && hello.leader_node_id < self.local_node_id);
            let self_assigned = hello.replicas.iter().any(|r| r == &self.local_node_id);
            if entry.role == LocalRole::Leader && peer_wins && self_assigned {
                drop(entry);
                tracing::warn!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    peer = %hello.leader_node_id,
                    peer_epoch = hello.leader_epoch,
                    "replication: fencing own leadership on a newer peer Hello \
                     (equal epochs resolve to the lower node id)"
                );
                if let Some(mut entry) = self.registry.get_mut(&key) {
                    // Stop serving first (`stop` aborts the feeder tasks —
                    // no further bytes leave this node under the old
                    // claim), then adopt the peer's view so the accept
                    // below sees a `Follower` entry at the peer's epoch.
                    if let Some(leader) = entry.leader.take() {
                        leader.stop();
                    }
                    entry.assignment = PartitionAssignment {
                        leader_node_id: hello.leader_node_id.clone(),
                        leader_epoch: hello.leader_epoch,
                        // The Hello carries no ISR of its own; the full
                        // replica set is the honest upper bound until this
                        // node's own poll materializes the ledger row (the
                        // live ISR the leader tracks needs no static
                        // answer from here).
                        isr: hello.replicas.clone(),
                        replicas: hello.replicas.clone(),
                        updated_at_ms: now_ms(),
                        ..entry.assignment.clone()
                    };
                    entry.role = LocalRole::Follower;
                    entry.follower = None;
                    // A promotion in flight on this entry is moot: the
                    // deterministic winner just dialed us.
                    entry.promotion = PromotionState::Idle;
                }
                self.assignments_changed.send_replace(());
            }
        }

        // Snapshot the verdict synchronously so no DashMap guard is ever
        // held across an `.await` below.
        enum Verdict {
            Accept(PartitionAssignment),
            Reject(ReplReject),
        }
        let verdict = match self.registry.get(&key) {
            None => Verdict::Reject(ReplReject::TopicUnknown),
            Some(entry) if entry.role != LocalRole::Follower => {
                Verdict::Reject(ReplReject::NotAReplica)
            }
            Some(entry) if hello.leader_epoch < entry.assignment.leader_epoch => {
                Verdict::Reject(ReplReject::StaleEpoch {
                    have: entry.assignment.leader_epoch,
                })
            }
            Some(entry) => Verdict::Accept(entry.assignment.clone()),
        };

        let assignment = match verdict {
            Verdict::Reject(reject) => {
                let ack = reject_ack(self.local_env, reject);
                let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
                return;
            }
            Verdict::Accept(a) => a,
        };

        if let Some(mut entry) = self.registry.get_mut(&key) {
            if let Some(old) = entry.follower.take() {
                old.stop();
            }
        }
        match self.follower_factory.spawn(&assignment, hello, recv, send) {
            Ok(runner) => {
                if let Some(mut entry) = self.registry.get_mut(&key) {
                    entry.follower = Some(runner);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "replication: follower runner spawn failed");
            }
        }
    }

    /// `accept_stream` only: make sure this node's registry knows `key`
    /// before a `Hello` for it is judged, resolving a miss against the
    /// ledger-materialized row instead of against this node's own startup
    /// lag.
    ///
    /// WHY. A follower never dials (module header) — `apply_assignment`'s
    /// `LocalRole::Follower` arm registers the entry and waits for the
    /// leader to arrive. So a leader that dials BEFORE this node's
    /// materialization poll has run used to be bounced with a flat
    /// `TopicUnknown` for a partition the LEDGER already assigned to this
    /// node, and each bounce cost a full reconnect-backoff round trip
    /// (`glue.rs`'s supervisor: 500 ms, doubling to 5 s). That is the whole
    /// reason `tests/process_three_node_bus_failover.rs`'s smoke test saw
    /// `isr=1, required=2` on a publish issued after every node already
    /// reported its role: the roles had converged locally, the leader's
    /// live ISR had not, because its only dial was already spent.
    ///
    /// HOW, cheapest first — and it never sleeps blindly:
    ///  1. registry already has `key` -> return (every stream that is not
    ///     racing startup, which is all of them in steady state) — re-checked
    ///     on every wake, so a poll apply ends the wait immediately;
    ///  2. `AssignmentStore::get` — the same `bus_partition_assignments`
    ///     table the poll reads, one indexed row here — already has a row
    ///     listing this node -> `apply_assignment` it NOW, zero waiting;
    ///  3. no row yet -> re-check (2) on every `assignments_changed` bump
    ///     (any local apply, including the poll loop's) or every
    ///     `ASSIGNMENT_AWAIT_RETRY`, until `ASSIGNMENT_AWAIT` runs out.
    ///
    /// A row that exists but does not list this node returns immediately:
    /// "not a replica of this" is an answer, not a race to wait out. So
    /// does the timeout — the caller's verdict still rejects with
    /// `TopicUnknown`, just only after the ledger had a chance to say
    /// otherwise. Every exit here is bounded and none of them acks a
    /// partition this node does not have.
    async fn await_local_assignment(&self, key: &PartitionKey) {
        if self.registry.contains_key(key) {
            return;
        }
        // Subscribed BEFORE the first lookup, so an `apply_assignment`
        // landing between the lookup and the wait below is still observed.
        let mut changed = self.assignments_changed.subscribe();
        let deadline = Instant::now() + ASSIGNMENT_AWAIT;
        loop {
            // The registry is what the caller actually reads, so re-check it
            // (not just the store) on every wake: whoever applied the
            // assignment — `init.rs`'s poll, a local `create_topic`, a
            // racing Hello — is what ends this wait, and ending it on the
            // bump rather than at the next tick is the whole point of
            // watching `assignments_changed` at all.
            if self.registry.contains_key(key) {
                return;
            }
            match self.assignments.get(&key.0, &key.1, key.2) {
                Ok(Some(a)) => {
                    // The ledger's answer is in. Whether it names this node
                    // or not, there is nothing left to wait for.
                    if a.replicas.iter().any(|r| r == &self.local_node_id) {
                        self.apply_assignment(a).await;
                    }
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                        "replication: assignment lookup for an inbound Hello failed"
                    );
                    return;
                }
            }
            if Instant::now() >= deadline {
                return;
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = changed.changed() => {}
                _ = tokio::time::sleep(ASSIGNMENT_AWAIT_RETRY) => {}
            }
        }
    }

    /// Applies a `PartitionAssignment` (from ledger materialization or a
    /// local `create_topic`) — decides this node's role and reconciles the
    /// registry entry to match. Tears down and rebuilds whenever anything
    /// about the assignment or the resulting role actually changed (no
    /// incremental reconciliation in this wave — assignment changes are
    /// rare, not a hot path).
    ///
    /// NON-NEGOTIABLE: the registry entry is removed and re-inserted with
    /// NO await in between (the leader's replica streams are opened by the
    /// glue's per-follower supervisor tasks, not here). Measured in the
    /// 3-process chaos run (`/tmp/g3_chaos_final.log`): the previous
    /// version dialed every replica synchronously between the remove and
    /// the insert, and the dial to the KILLED leader ran out iroh's ~40 s
    /// QUIC connect timeout — leaving the partition's registry entry GONE
    /// (every publish refused `leader_node_id=None, leader_epoch=0`,
    /// 249 of them from node b) for exactly as long as the dead peer's
    /// connect took to fail, and gating the winner's own serving
    /// capability behind the dead peer's teardown. The supervisor's
    /// reconnect loop (backoff 500 ms -> 5 s) is the only dialer now: a
    /// first dial to an unreachable peer costs that one supervisor its
    /// backoff cycle, never the registry, never the role, never the
    /// serving path.
    pub async fn apply_assignment(&self, assignment: PartitionAssignment) {
        let key: PartitionKey = (
            assignment.org_id.clone(),
            assignment.topic.clone(),
            assignment.partition,
        );
        let is_replica = assignment.replicas.iter().any(|r| r == &self.local_node_id);
        let new_role = if !is_replica {
            LocalRole::NotReplica
        } else if assignment.leader_node_id == self.local_node_id {
            LocalRole::Leader
        } else {
            LocalRole::Follower
        };

        let unchanged = self
            .registry
            .get(&key)
            .map(|e| e.role == new_role && e.assignment == assignment)
            .unwrap_or(false);
        if unchanged {
            return;
        }

        if let Some((_, mut old)) = self.registry.remove(&key) {
            if let Some(leader) = old.leader.take() {
                leader.stop();
            }
            if let Some(follower) = old.follower.take() {
                follower.stop();
            }
        }

        match new_role {
            LocalRole::NotReplica => {}
            LocalRole::Leader => {
                // No dials here (see the NON-NEGOTIABLE note above): the
                // glue spawns one supervisor per replica, each dialing in
                // its own task. `spawn_deferred` keeps the engine's
                // writer-thread epoch stamp off this call path too.
                match self.leader_factory.spawn_deferred(&assignment, Vec::new()) {
                    Ok(handle) => {
                        self.registry.insert(
                            key,
                            PartitionEntry {
                                assignment,
                                role: LocalRole::Leader,
                                leader: Some(handle),
                                follower: None,
                                promotion: PromotionState::Idle,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "replication: leader handle spawn failed");
                    }
                }
            }
            LocalRole::Follower => {
                // No dial here (see module header): the entry is registered
                // so `accept_stream` has somewhere to attach the
                // `FollowerRunner` once the leader dials in.
                self.registry.insert(
                    key,
                    PartitionEntry {
                        assignment,
                        role: LocalRole::Follower,
                        leader: None,
                        follower: None,
                        promotion: PromotionState::Idle,
                    },
                );
            }
        }
        // Reached only when something actually changed — the `unchanged`
        // path above returns first. Wakes anything parked in
        // `await_local_assignment` without waiting for its own re-read tick.
        self.assignments_changed.send_replace(());
    }

    /// `IrohMeshEvent::PeerDisconnected` handling (PLAN-M2 §1b): an
    /// accelerator, not the only signal — marks every follower runner
    /// whose leader is `node_id` so its own lease watchdog can treat the
    /// lease as expired sooner than the full 3000 ms.
    pub fn on_peer_disconnected(&self, node_id: &str) {
        for entry in self.registry.iter() {
            if entry.assignment.leader_node_id == node_id {
                if let Some(follower) = entry.follower.as_ref() {
                    follower.mark_leader_disconnected();
                }
            }
        }
    }

    /// Scans every `Follower` entry and starts an election for any whose
    /// lease has expired (per `FollowerRunner::lease_expired`) and who is
    /// still in the last known ISR (`election::should_start_election`).
    /// Meant to be called on a periodic tick by whatever owns this
    /// manager's lifecycle (wave 2, `tentaflow/src/main.rs`).
    pub async fn check_leases(&self) {
        let due: Vec<PartitionKey> = self
            .registry
            .iter()
            .filter_map(|entry| {
                if entry.role != LocalRole::Follower {
                    return None;
                }
                let follower = entry.follower.as_ref()?;
                let in_isr = entry
                    .assignment
                    .isr
                    .iter()
                    .any(|n| n == &self.local_node_id);
                let idle = matches!(entry.promotion, PromotionState::Idle);
                if idle
                    && election::should_start_election(
                        follower.lease_expired(),
                        in_isr,
                        LocalRole::Follower,
                    )
                {
                    Some(entry.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for key in due {
            self.run_election(key).await;
        }
    }

    /// Forces an election attempt for one partition regardless of lease
    /// state — used by `check_leases` and directly by tests.
    pub async fn run_election(&self, key: PartitionKey) {
        let Some((assignment, own_leo, own_hw)) = self.registry.get(&key).map(|e| {
            let leo = e.follower.as_ref().map(|f| f.leo()).unwrap_or(0);
            let hw = e.follower.as_ref().map(|f| f.hw()).unwrap_or(0);
            (e.assignment.clone(), leo, hw)
        }) else {
            return;
        };

        // Computed BEFORE stepping so the event's own `leo_query_deadline`
        // and this function's wait loop agree on exactly the same instant
        // — `election.rs` never invents a deadline of its own from a
        // hardcoded constant (see `PromotionEvent::LeaseExpired`'s doc).
        let leo_deadline = Instant::now() + self.leo_query_timeout;
        let event = PromotionEvent::LeaseExpired {
            org_id: key.0.clone(),
            topic: key.1.clone(),
            partition: key.2,
            self_id: self.local_node_id.clone(),
            current_epoch: assignment.leader_epoch,
            own_leo,
            own_hw,
            isr: assignment.isr.clone(),
            replicas: assignment.replicas.clone(),
            leo_query_deadline: leo_deadline,
        };
        let (mut state, actions) = PromotionState::Idle.step(event);
        self.set_promotion(&key, state.clone());
        let Some(PromotionAction::SendLeoQuery { to }) = actions.into_iter().next() else {
            return; // Abandoned{NotInIsr} — nothing to query.
        };

        for peer in to {
            let remaining = leo_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(Some((leo, in_isr))) =
                tokio::time::timeout(remaining, self.query_leo(&key, &peer)).await
            {
                let (next, _) = state.step(PromotionEvent::LeoReply {
                    node_id: peer,
                    leo,
                    in_isr,
                });
                state = next;
            }
        }

        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: Instant::now().max(leo_deadline),
            now_ms: now_ms(),
        });
        self.set_promotion(&key, state.clone());
        let Some(PromotionAction::ProposeAssignment(proposed)) = actions.into_iter().next() else {
            return; // Abandoned{NotInIsr|LostElection}.
        };

        let majority_deadline = Instant::now() + self.majority_await_timeout;
        let state = match self.assignments.propose(proposed.clone()) {
            Ok(op_id) => {
                let (next, _) = state.step(PromotionEvent::Proposed {
                    op_id,
                    deadline: majority_deadline,
                });
                next
            }
            Err(_) => {
                let (next, _) = state.step(PromotionEvent::ProposeFailed);
                self.set_promotion(&key, next);
                return;
            }
        };
        self.set_promotion(&key, state.clone());
        let PromotionState::AwaitingMajority { op_id, .. } = state else {
            return;
        };

        let mut state = state;
        loop {
            let acked = self.ledger.admitted_by(op_id);
            let (next, actions) = state.step(PromotionEvent::AckObserved { acked });
            state = next;
            if !actions.is_empty() {
                self.set_promotion(&key, state.clone());
                self.execute_promotion_actions(&key, &proposed, actions)
                    .await;
                return;
            }
            let now = Instant::now();
            if now >= majority_deadline {
                let (next, _) = state.step(PromotionEvent::Timeout {
                    now,
                    now_ms: now_ms(),
                });
                self.set_promotion(&key, next);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20).min(majority_deadline - now)).await;
        }
    }

    async fn query_leo(&self, key: &PartitionKey, peer: &str) -> Option<(u64, bool)> {
        let (mut recv, mut send) = self.transport.open_stream(peer).await.ok()?;
        let known_epoch = self
            .registry
            .get(key)
            .map(|e| e.assignment.leader_epoch)
            .unwrap_or(0);
        let query = ReplFrame::LeoQuery(ReplLeoQuery {
            org_id: key.0.clone(),
            topic: key.1.clone(),
            partition: key.2,
            known_epoch,
        });
        frames::write_frame(&mut send, &query).await.ok()?;
        match frames::read_frame(&mut recv).await.ok()? {
            ReplFrame::LeoReply(r) => Some((r.leo, r.in_isr)),
            _ => None,
        }
    }

    fn set_promotion(&self, key: &PartitionKey, state: PromotionState) {
        if let Some(mut entry) = self.registry.get_mut(key) {
            entry.promotion = state;
        }
    }

    /// Executes `Promoted`'s actions: re-reads the ledger's materialized
    /// row one last time (exclusive promotion — see below), opens feeders
    /// to every other replica and spawns the `LeaderHandle` via
    /// `spawn_deferred` (which performs `SetLeaderEpoch`+"open partition",
    /// see module header), then sends any pending `Truncate`s and applies
    /// the new assignment locally.
    ///
    /// EXCLUSIVE PROMOTION (P8, M2-WYNIKI "promocja nie jest wyłączna").
    /// Majority admission — `admitted_by_majority` over the candidate's
    /// OWN op's outbox acks — does not by itself settle WHO leads: two
    /// simultaneous self-elections at the same next epoch both mint ops,
    /// and a peer's outbox ack acknowledges DELIVERY, not "the
    /// materializer admitted MY row" (a same-epoch row that loses the
    /// node-id tie-break is applied as a no-op yet still acked). Left
    /// unguarded, both candidates promote and the partition gets two
    /// serving leaders. The deterministic settle is the ledger's
    /// materialized row: it converged (and keeps converging, wherever the
    /// ops race) to the same single winner via the materializer gate —
    /// strictly higher epoch, equal epoch picks the lower node id. So
    /// before inserting the Leader registry entry, the row is consulted:
    /// if it already names a DIFFERENT leader at an epoch that beats this
    /// proposal, this node yields — no leader handle is spawned, and the
    /// entry stays (or becomes) a follower of the stored leader, which the
    /// assignment poll will keep in step with the ledger.
    ///
    /// This closes the promotion side; the Hello-side fence in
    /// `accept_hello` covers the inverse order (both promoted before
    /// either row landed, then the winner dials the loser).
    async fn execute_promotion_actions(
        &self,
        key: &PartitionKey,
        proposed: &PartitionAssignment,
        actions: Vec<PromotionAction>,
    ) {
        let mut epoch = None;
        let mut start_feeders = false;
        let mut truncates = Vec::new();
        for action in actions {
            match action {
                PromotionAction::SetLeaderEpoch(e) => epoch = Some(e),
                PromotionAction::StartFeeders => start_feeders = true,
                PromotionAction::SendTruncate { node, to } => truncates.push((node, to)),
                _ => {}
            }
        }
        let (Some(epoch), true) = (epoch, start_feeders) else {
            return;
        };

        let mut assignment = proposed.clone();
        assignment.leader_epoch = epoch;

        // The settle check (see the EXCLUSIVE PROMOTION doc above). Read
        // through the SAME store the materializer writes, so what this
        // sees is exactly what the admission gate decided — no second
        // source of truth, no extra wire round trip.
        if let Ok(Some(stored)) = self.assignments.get(&key.0, &key.1, key.2) {
            let beats_this_proposal = stored.leader_epoch > assignment.leader_epoch
                || (stored.leader_epoch == assignment.leader_epoch
                    && stored.leader_node_id != self.local_node_id
                    && stored.leader_node_id < self.local_node_id);
            if beats_this_proposal {
                tracing::warn!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    stored_leader = %stored.leader_node_id,
                    stored_epoch = stored.leader_epoch,
                    proposed_epoch = assignment.leader_epoch,
                    "replication: yielding promotion — the ledger already settled \
                     this partition on another leader (equal epochs resolve to \
                     the lower node id)"
                );
                // Become a follower of the stored leader: rebuild the
                // entry through the normal path so role, handles and
                // promotion state all agree, and the stored leader's
                // feeder (or its reconnect supervisor) finds an accepting
                // follower here.
                let stored_epoch = stored.leader_epoch;
                self.apply_assignment(stored).await;
                tracing::debug!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    leader = ?self.role(&key.0, &key.1, key.2),
                    "replication: yielded promotion; now following the settled leader \
                     at epoch {stored_epoch}"
                );
                return;
            }
        }

        let from_node = self
            .registry
            .get(key)
            .map(|e| e.assignment.leader_node_id.clone());
        let started_at = Instant::now();

        // No dials here either (mirrors `apply_assignment`'s NON-NEGOTIABLE
        // note): the promotion's registry insert must not wait on ANY
        // dial — a dead peer's connect timeout must never delay the new
        // leader's own serving capability or leave its registry entry
        // removed. Every replica stream is the glue supervisor's job.
        match self.leader_factory.spawn_deferred(&assignment, Vec::new()) {
            Ok(handle) => {
                for (node, to) in &truncates {
                    handle.send_truncate(node, *to);
                }
                if let Some((_, mut old)) = self.registry.remove(key) {
                    if let Some(follower) = old.follower.take() {
                        follower.stop();
                    }
                }
                self.registry.insert(
                    key.clone(),
                    PartitionEntry {
                        assignment: assignment.clone(),
                        role: LocalRole::Leader,
                        leader: Some(handle),
                        follower: None,
                        promotion: PromotionState::Idle,
                    },
                );
                self.audit.failover(
                    &key.0,
                    &key.1,
                    key.2,
                    from_node.as_deref(),
                    &self.local_node_id,
                    epoch.saturating_sub(1),
                    epoch,
                    started_at.elapsed().as_millis() as u64,
                    "lease_expired",
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "replication: promoted but leader handle spawn failed");
            }
        }
    }
}

impl ReplicationCoordinator for ReplicationManager {
    fn role(&self, org: &str, topic: &str, partition: u32) -> PartitionRole {
        let key: PartitionKey = (org.to_string(), topic.to_string(), partition);
        match self.registry.get(&key) {
            Some(entry) => match entry.role {
                LocalRole::Leader => PartitionRole::Leader {
                    epoch: entry.assignment.leader_epoch,
                },
                LocalRole::Follower => PartitionRole::Follower {
                    leader_node_id: entry.assignment.leader_node_id.clone(),
                    epoch: entry.assignment.leader_epoch,
                },
                LocalRole::NotReplica => PartitionRole::Unavailable {
                    reason: UnavailableReason::NoAssignment,
                },
            },
            None => PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment,
            },
        }
    }

    fn preflight(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        _acks: Acks,
    ) -> Result<u32, ReplError> {
        let key: PartitionKey = (org.to_string(), topic.to_string(), partition);
        let entry = self
            .registry
            .get(&key)
            .ok_or_else(|| ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            })?;
        if entry.role != LocalRole::Leader {
            return Err(ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            });
        }
        // K-M2-2: the gate is against the LIVE ISR (`LeaderHandle::isr`,
        // backed by `PartitionLeader`'s own ack/lag bookkeeping), not the
        // static `PartitionAssignment.isr` the ledger last materialized —
        // a follower shrinking out of (or rejoining) the ISR must be
        // visible to this check immediately, not only after the next
        // ledger round trip. `entry.role == Leader` (just checked above)
        // always has `entry.leader` populated (see `apply_assignment`/
        // `execute_promotion_actions`'s own invariant), so the fallback to
        // the static field is unreachable in practice — kept only so this
        // never panics if that invariant is ever violated.
        let live_isr = entry
            .leader
            .as_ref()
            .map(|l| l.isr())
            .unwrap_or_else(|| entry.assignment.isr.clone());
        let required = election::min_isr_required(entry.assignment.replicas.len()) as u32;
        let isr_len = live_isr.len() as u32;
        if isr_len < required {
            return Err(ReplError::NotEnoughReplicas {
                topic: topic.to_string(),
                partition,
                isr: isr_len,
                required,
            });
        }
        Ok(entry.assignment.leader_epoch)
    }

    fn await_acks(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        next_offset: u64,
        acks: Acks,
        timeout: Duration,
    ) -> Result<AckOutcome, ReplError> {
        let key: PartitionKey = (org.to_string(), topic.to_string(), partition);
        let entry = self
            .registry
            .get(&key)
            .ok_or_else(|| ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            })?;
        let Some(leader) = entry.leader.as_ref() else {
            return Err(ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            });
        };
        let required = match acks {
            Acks::Leader => 1,
            Acks::Quorum => election::min_isr_required(entry.assignment.replicas.len()) as u32,
            Acks::All => entry.assignment.isr.len().max(1) as u32,
        };
        Ok(leader.await_acks(next_offset, required, timeout))
    }

    fn note_offset_commit(
        &self,
        org: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        attempts: u32,
    ) {
        let key: PartitionKey = (org.to_string(), topic.to_string(), partition);
        if let Some(entry) = self.registry.get(&key) {
            if let Some(leader) = entry.leader.as_ref() {
                leader.note_offset_commit(group, partition, offset, attempts);
            }
        }
    }

    fn evict_node_from_replica_sets(
        &self,
        node_id: &str,
        reason: &'static str,
    ) -> Result<u32, ReplError> {
        let keys: Vec<PartitionKey> = self
            .registry
            .iter()
            .filter(|e| e.assignment.replicas.iter().any(|r| r == node_id))
            .map(|e| e.key().clone())
            .collect();
        let mut touched = 0u32;
        for key in keys {
            let Some(mut assignment) = self.registry.get(&key).map(|e| e.assignment.clone()) else {
                continue;
            };
            if !assignment.replicas.iter().any(|r| r == node_id) {
                continue;
            }
            let isr_before = assignment.isr.len();
            assignment.replicas.retain(|r| r != node_id);
            assignment.isr.retain(|r| r != node_id);
            if assignment.isr.len() < isr_before {
                self.isr_shrink_total.fetch_add(1, Ordering::Relaxed);
            }
            // A replica-set change IS an epoch change (fencing semantics,
            // T1's finding (2)): the materializer's own admission gate
            // (`core_materializer::apply_bus_partition_assignment`) admits
            // only a strictly higher epoch (or the same epoch with a
            // lower `leader_node_id`) — proposing this eviction at the
            // SAME epoch it was read at is silently dropped (`Ok(0)`, no
            // error) every time the local leader is unchanged, which is
            // the common case for an eviction. Bumping here also gives
            // every follower a fresh epoch to fence stale writers against,
            // matching `transfer_leader`'s own already-correct behavior.
            assignment.leader_epoch = election::next_epoch(assignment.leader_epoch);
            assignment.updated_at_ms = now_ms();
            self.assignments.propose(assignment)?;
            touched += 1;
        }
        if touched > 0 {
            self.audit.evicted(node_id, reason, touched);
        }
        Ok(touched)
    }

    fn transfer_leader(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        target: &str,
    ) -> Result<u32, ReplError> {
        let key: PartitionKey = (org.to_string(), topic.to_string(), partition);
        let assignment = self
            .registry
            .get(&key)
            .map(|e| e.assignment.clone())
            .ok_or_else(|| ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            })?;
        if !assignment.isr.iter().any(|n| n == target) {
            return Err(ReplError::NotAReplica {
                topic: topic.to_string(),
                partition,
                node_id: target.to_string(),
            });
        }
        let mut proposed = assignment.clone();
        proposed.leader_node_id = target.to_string();
        proposed.leader_epoch = election::next_epoch(assignment.leader_epoch);
        proposed.updated_at_ms = now_ms();
        let op_id = self.assignments.propose(proposed.clone())?;

        let start = Instant::now();
        loop {
            let acked = self.ledger.admitted_by(op_id);
            if election::admitted_by_majority(&acked, &proposed.replicas, &self.local_node_id) {
                if let Some(mut entry) = self.registry.get_mut(&key) {
                    if entry.role == LocalRole::Leader {
                        if let Some(leader) = entry.leader.take() {
                            leader.stop();
                        }
                        // Corrected to `Follower`/`NotReplica` by the next
                        // `apply_assignment` once the op materializes back.
                        entry.role = LocalRole::NotReplica;
                    }
                }
                self.audit.transfer(
                    org,
                    topic,
                    partition,
                    &assignment.leader_node_id,
                    target,
                    proposed.leader_epoch,
                );
                return Ok(proposed.leader_epoch);
            }
            if start.elapsed() >= TRANSFER_MAJORITY_TIMEOUT {
                return Err(ReplError::Internal(format!(
                    "transfer_leader: majority not reached for {org}/{topic}/{partition}"
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn reassign(
        &self,
        org: &str,
        topic: &str,
        partition: Option<u32>,
        replicas: &[String],
    ) -> Result<u32, ReplError> {
        let keys: Vec<PartitionKey> = self
            .registry
            .iter()
            .filter(|e| {
                let k = e.key();
                k.0 == org && k.1 == topic && partition.is_none_or(|p| p == k.2)
            })
            .map(|e| e.key().clone())
            .collect();
        let mut touched = 0u32;
        for key in keys {
            let Some(mut assignment) = self.registry.get(&key).map(|e| e.assignment.clone()) else {
                continue;
            };
            let isr_before = assignment.isr.len();
            assignment.replicas = replicas.to_vec();
            assignment.isr.retain(|n| replicas.iter().any(|r| r == n));
            if assignment.isr.len() < isr_before {
                self.isr_shrink_total.fetch_add(1, Ordering::Relaxed);
            }
            // See `evict_node_from_replica_sets`'s identical comment: a
            // replica-set change is an epoch change, or the materializer's
            // admission gate silently drops a same-leader reassign.
            assignment.leader_epoch = election::next_epoch(assignment.leader_epoch);
            assignment.updated_at_ms = now_ms();
            self.assignments.propose(assignment)?;
            touched += 1;
        }
        Ok(touched)
    }

    fn snapshot(&self, org: &str, topic: Option<&str>) -> ReplicationSnapshot {
        let mut partitions = Vec::new();
        let mut nodes: BTreeMap<String, ReplicaNodeInfo> = BTreeMap::new();
        for entry in self.registry.iter() {
            let a = &entry.assignment;
            if a.org_id != org {
                continue;
            }
            if let Some(t) = topic {
                if a.topic != t {
                    continue;
                }
            }
            // K-M2-2/T1's finding (4): only THIS node's own `LeaderHandle`
            // ever knows the LIVE ISR (`PartitionLeader`'s own ack/lag
            // bookkeeping) — a follower-role entry has no such handle and
            // falls back to the last ledger-materialized `assignment.isr`,
            // which is the best this node can know about a partition it
            // does not lead. Same reasoning `preflight` above uses.
            let (live_isr, lagging): (Vec<String>, Vec<ReplicaLagInfo>) = match entry.role {
                LocalRole::Leader => entry
                    .leader
                    .as_ref()
                    .map(|l| (l.isr(), l.lagging()))
                    .unwrap_or_else(|| (a.isr.clone(), Vec::new())),
                _ => (a.isr.clone(), Vec::new()),
            };
            for node_id in &a.replicas {
                let info = nodes
                    .entry(node_id.clone())
                    .or_insert_with(|| ReplicaNodeInfo {
                        node_id: node_id.clone(),
                        label: node_id.clone(),
                        environment: self.local_env,
                        is_local: node_id == &self.local_node_id,
                        reachable: true,
                        last_heartbeat_ms_ago: None,
                        leader_count: 0,
                        follower_count: 0,
                        isr_count: 0,
                    });
                if &a.leader_node_id == node_id {
                    info.leader_count += 1;
                } else {
                    info.follower_count += 1;
                }
                if live_isr.iter().any(|m| m == node_id) {
                    info.isr_count += 1;
                }
            }
            let (hw, leo) = match entry.role {
                LocalRole::Leader => entry
                    .leader
                    .as_ref()
                    .map(|l| (l.high_watermark(), l.log_end_offset()))
                    .unwrap_or((0, 0)),
                _ => entry
                    .follower
                    .as_ref()
                    .map(|f| (f.hw(), f.leo()))
                    .unwrap_or((0, 0)),
            };
            partitions.push(PartitionReplicaInfo {
                topic: a.topic.clone(),
                partition: a.partition,
                leader_node_id: Some(a.leader_node_id.clone()),
                leader_epoch: a.leader_epoch,
                replicas: a.replicas.clone(),
                isr: live_isr,
                lagging,
                high_watermark: hw,
                log_end_offset: leo,
                unavailable_reason: None,
            });
        }
        ReplicationSnapshot {
            nodes: nodes.into_values().collect(),
            partitions,
            // `audit_log`-sourced (PLAN-M2 §1f, `bus.leader.failover` rows)
            // — belongs to whichever layer owns `repository::log_audit`
            // reads, not this in-memory registry.
            failovers: Vec::new(),
        }
    }

    fn isr_shrink_total(&self) -> u64 {
        self.isr_shrink_total.load(Ordering::Relaxed)
    }
}

// ===== Real production implementations ======================================

/// Real `Transport`: one fresh `iroh` connection (no reuse across calls in
/// this wave — replication streams are long-lived, so the extra dial cost
/// on the rare leader-change/partition-open path is not worth the added
/// connection-cache bookkeeping yet) plus one `open_bi()` per call.
pub struct IrohTransport {
    mesh: Arc<IrohMeshManager>,
}

impl IrohTransport {
    pub fn new(mesh: Arc<IrohMeshManager>) -> Self {
        Self { mesh }
    }
}

#[async_trait::async_trait]
impl Transport for IrohTransport {
    async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
        let connection = self
            .mesh
            .connect_bus(node_id)
            .await
            .map_err(|e| ReplError::Internal(format!("connect_bus({node_id}): {e}")))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| ReplError::Internal(format!("open_bi({node_id}): {e}")))?;
        Ok((Box::new(recv), Box::new(send)))
    }
}

/// Real `LedgerAdmission`: PLAN-M2 §1c's majority-admission proof already
/// exists as `SyncLedgerStore::list_outbox_for_operation` — this is a thin,
/// straightforward wrapper, not a stub.
pub struct FjallLedgerAdmission {
    store: Arc<dyn SyncLedgerStore>,
}

impl FjallLedgerAdmission {
    pub fn new(store: Arc<dyn SyncLedgerStore>) -> Self {
        Self { store }
    }
}

impl LedgerAdmission for FjallLedgerAdmission {
    fn admitted_by(&self, op_id: OperationId) -> Vec<String> {
        match self.store.list_outbox_for_operation(op_id) {
            Ok(entries) => entries
                .into_iter()
                .filter(|e| e.acknowledged)
                .map(|e| e.target.as_str().to_string())
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "replication: list_outbox_for_operation failed");
                Vec::new()
            }
        }
    }
}

/// Alternate real `AssignmentStore`/`LedgerAdmission`: forwards to agent
/// L's already-landed `SqliteLedgerAssignmentStore`, which reaches the
/// ledger through `sync::runtime::{record_core_capture,
/// acknowledged_outbox_targets}` rather than a directly-injected
/// `Arc<dyn SyncLedgerStore>`. Kept alongside `FjallLedgerAdmission`
/// (above) rather than replacing it: `SqliteLedgerAssignmentStore::
/// admitted_by` depends on two `sync/runtime.rs` additions its own doc
/// comment flags as "outside this task's exclusive file list... flagged
/// for coordinator review" — if those do not land as written,
/// `FjallLedgerAdmission` (which needs no `sync/runtime.rs` change at all)
/// is the fallback wiring for wave 2.
///
/// The `self.get(...)`/`self.propose(...)`/`self.admitted_by(...)` calls
/// below resolve to `SqliteLedgerAssignmentStore`'s INHERENT methods, not
/// a recursive trait call: Rust always prefers an inherent method over a
/// trait method of the same name when both are in scope, so this is not
/// infinite recursion.
impl AssignmentStore for SqliteLedgerAssignmentStore {
    fn get(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError> {
        self.get(org, topic, partition)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    fn list_for_topic(
        &self,
        org: &str,
        topic: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        self.list_for_topic(org, topic)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    fn list_for_node(&self, node_id: &str) -> Result<Vec<PartitionAssignment>, ReplError> {
        self.list_for_node(node_id)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    fn propose(&self, assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
        self.propose(&assignment)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }
}

impl LedgerAdmission for SqliteLedgerAssignmentStore {
    fn admitted_by(&self, op_id: OperationId) -> Vec<String> {
        match self.admitted_by(op_id) {
            Ok(targets) => targets,
            Err(e) => {
                tracing::warn!(error = %e, "replication: SqliteLedgerAssignmentStore::admitted_by failed");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::replication::frames::{ReplLeoReply, ReplTruncate};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tokio::io::{split, AsyncReadExt};

    fn assignment(
        org: &str,
        topic: &str,
        partition: u32,
        leader: &str,
        replicas: &[&str],
        isr: &[&str],
        epoch: u32,
    ) -> PartitionAssignment {
        PartitionAssignment {
            org_id: org.to_string(),
            topic: topic.to_string(),
            partition,
            leader_node_id: leader.to_string(),
            replicas: replicas.iter().map(|s| s.to_string()).collect(),
            isr: isr.iter().map(|s| s.to_string()).collect(),
            leader_epoch: epoch,
            updated_at_ms: 0,
        }
    }

    // ---- fakes ---------------------------------------------------------

    #[derive(Clone)]
    enum PeerScript {
        LeoReply { leo: u64, in_isr: bool },
        Unreachable,
    }

    /// In-memory `Transport`: `open_stream(peer)` spawns a task that reads
    /// whatever we write and, if it is a `LeoQuery`, answers per the
    /// peer's registered script. Anything else written (e.g. a leader
    /// feeder's own protocol) is simply never read back — harmless for the
    /// tests in this module, which never assert on feeder-side traffic.
    struct FakeTransport {
        scripts: DashMap<String, PeerScript>,
        dial_count: DashMap<String, u32>,
    }

    impl FakeTransport {
        fn new() -> Self {
            Self {
                scripts: DashMap::new(),
                dial_count: DashMap::new(),
            }
        }

        fn set_script(&self, node_id: &str, script: PeerScript) {
            self.scripts.insert(node_id.to_string(), script);
        }

        fn dials(&self, node_id: &str) -> u32 {
            self.dial_count.get(node_id).map(|v| *v).unwrap_or(0)
        }
    }

    #[async_trait::async_trait]
    impl Transport for FakeTransport {
        async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
            *self.dial_count.entry(node_id.to_string()).or_insert(0) += 1;
            let script = self
                .scripts
                .get(node_id)
                .map(|s| s.clone())
                .unwrap_or(PeerScript::Unreachable);
            if matches!(script, PeerScript::Unreachable) {
                return Err(ReplError::Internal(format!("unreachable: {node_id}")));
            }
            let (ours, theirs) = tokio::io::duplex(16 * 1024);
            tokio::spawn(async move {
                let (mut peer_recv, mut peer_send) = split(theirs);
                if let Ok(ReplFrame::LeoQuery(_)) = frames::read_frame(&mut peer_recv).await {
                    if let PeerScript::LeoReply { leo, in_isr } = script {
                        let reply = ReplFrame::LeoReply(ReplLeoReply {
                            leo,
                            hw: leo,
                            leader_epoch: 0,
                            in_isr,
                        });
                        let _ = frames::write_frame(&mut peer_send, &reply).await;
                    }
                }
            });
            let (our_recv, our_send) = split(ours);
            Ok((Box::new(our_recv), Box::new(our_send)))
        }
    }

    struct FakeLedger {
        acked: Mutex<std::collections::HashMap<OperationId, Vec<String>>>,
    }

    impl FakeLedger {
        fn new() -> Self {
            Self {
                acked: Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn set_acked(&self, op_id: OperationId, acked: Vec<String>) {
            self.acked.lock().insert(op_id, acked);
        }
    }

    impl LedgerAdmission for FakeLedger {
        fn admitted_by(&self, op_id: OperationId) -> Vec<String> {
            self.acked.lock().get(&op_id).cloned().unwrap_or_default()
        }
    }

    /// Mirrors the materializer's extra admission gate documented in
    /// PLAN-M2 §1c (`core_materializer.rs`, agent L, not frozen — this is
    /// a reasonable stand-in, not an assertion about L's real behavior):
    /// `incoming.leader_epoch > stored.leader_epoch`, or (`==` and
    /// `incoming.leader_node_id < stored.leader_node_id`).
    struct FakeAssignmentStore {
        rows: Mutex<std::collections::HashMap<PartitionKey, PartitionAssignment>>,
        next_op: Mutex<u8>,
        fail_next: AtomicBool,
    }

    impl FakeAssignmentStore {
        fn new() -> Self {
            Self {
                rows: Mutex::new(std::collections::HashMap::new()),
                next_op: Mutex::new(1),
                fail_next: AtomicBool::new(false),
            }
        }

        fn seed(&self, a: PartitionAssignment) {
            let key = (a.org_id.clone(), a.topic.clone(), a.partition);
            self.rows.lock().insert(key, a);
        }

        fn fail_next_propose(&self) {
            self.fail_next.store(true, Ordering::SeqCst);
        }

        fn stored(&self, key: &PartitionKey) -> Option<PartitionAssignment> {
            self.rows.lock().get(key).cloned()
        }
    }

    impl AssignmentStore for FakeAssignmentStore {
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
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(ReplError::Internal("forced propose failure".into()));
            }
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
            if admitted {
                rows.insert(key, assignment);
            }
            drop(rows);
            let mut n = self.next_op.lock();
            let id = OperationId::from_hash([*n; 32]);
            *n = n.wrapping_add(1).max(1);
            Ok(id)
        }
    }

    struct FakeLeaderHandle {
        // `Mutex`, not a plain `Vec`: the live-ISR test below (T1's finding
        // (4)) needs to mutate this AFTER `spawn` already handed the
        // `Box<dyn LeaderHandle>` to `ReplicationManager`, simulating a
        // follower dropping out of (and rejoining) the ISR without a new
        // `apply_assignment` round trip — exactly what `preflight`/
        // `snapshot` must observe live now that they read `LeaderHandle::
        // isr()` instead of the static `PartitionAssignment.isr`.
        isr: Mutex<Vec<String>>,
        hw: AtomicU64,
        leo: AtomicU64,
        truncated: Mutex<Vec<(String, u64)>>,
        stopped: AtomicBool,
    }

    impl FakeLeaderHandle {
        fn new(isr: Vec<String>, hw: u64, leo: u64) -> Self {
            Self {
                isr: Mutex::new(isr),
                hw: AtomicU64::new(hw),
                leo: AtomicU64::new(leo),
                truncated: Mutex::new(Vec::new()),
                stopped: AtomicBool::new(false),
            }
        }

        fn set_isr(&self, isr: Vec<String>) {
            *self.isr.lock() = isr;
        }
    }

    impl LeaderHandle for FakeLeaderHandle {
        fn isr(&self) -> Vec<String> {
            self.isr.lock().clone()
        }
        fn high_watermark(&self) -> u64 {
            self.hw.load(Ordering::SeqCst)
        }
        fn log_end_offset(&self) -> u64 {
            self.leo.load(Ordering::SeqCst)
        }
        fn await_acks(&self, _next_offset: u64, required: u32, _timeout: Duration) -> AckOutcome {
            AckOutcome {
                acked_nodes: self.isr.lock().len() as u32,
                required,
                hw: self.high_watermark(),
            }
        }
        fn note_offset_commit(&self, _group: &str, _partition: u32, _offset: u64, _attempts: u32) {}
        fn send_truncate(&self, node: &str, to_offset: u64) {
            self.truncated.lock().push((node.to_string(), to_offset));
        }
        fn stop(&self) {
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    struct FakeLeaderFactory {
        spawned: Mutex<Vec<PartitionAssignment>>,
        fail: AtomicBool,
        // `Arc`, not the `Box<dyn LeaderHandle>` `spawn` hands to the
        // manager: the live-ISR test below needs to keep mutating the SAME
        // handle (`set_isr`) after the manager already owns it.
        handles: Mutex<Vec<Arc<FakeLeaderHandle>>>,
    }

    impl FakeLeaderFactory {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                spawned: Mutex::new(Vec::new()),
                fail: AtomicBool::new(false),
                handles: Mutex::new(Vec::new()),
            })
        }
    }

    /// Thin `LeaderHandle` wrapper over a shared `Arc<FakeLeaderHandle>` so
    /// `FakeLeaderFactory::spawn` can both hand a `Box<dyn LeaderHandle>` to
    /// the manager AND keep its own `Arc` for the test to mutate afterward.
    struct SharedFakeLeaderHandle(Arc<FakeLeaderHandle>);

    impl LeaderHandle for SharedFakeLeaderHandle {
        fn isr(&self) -> Vec<String> {
            self.0.isr()
        }
        fn high_watermark(&self) -> u64 {
            self.0.high_watermark()
        }
        fn log_end_offset(&self) -> u64 {
            self.0.log_end_offset()
        }
        fn await_acks(&self, next_offset: u64, required: u32, timeout: Duration) -> AckOutcome {
            self.0.await_acks(next_offset, required, timeout)
        }
        fn note_offset_commit(&self, group: &str, partition: u32, offset: u64, attempts: u32) {
            self.0
                .note_offset_commit(group, partition, offset, attempts)
        }
        fn send_truncate(&self, node: &str, to_offset: u64) {
            self.0.send_truncate(node, to_offset)
        }
        fn stop(&self) {
            self.0.stop()
        }
    }

    impl LeaderHandleFactory for FakeLeaderFactory {
        fn spawn(
            &self,
            assignment: &PartitionAssignment,
            _replica_streams: Vec<(String, BusRecv, BusSend)>,
        ) -> Result<Box<dyn LeaderHandle>, ReplError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(ReplError::Internal("forced leader spawn failure".into()));
            }
            self.spawned.lock().push(assignment.clone());
            let handle = Arc::new(FakeLeaderHandle::new(assignment.isr.clone(), 0, 0));
            self.handles.lock().push(Arc::clone(&handle));
            Ok(Box::new(SharedFakeLeaderHandle(handle)))
        }
    }

    struct FakeFollowerHandle {
        leo: AtomicU64,
        hw: AtomicU64,
        lease_expired: AtomicBool,
        disconnected: AtomicBool,
        stopped: AtomicBool,
    }

    struct FakeFollowerRunner {
        shared: Arc<FakeFollowerHandle>,
    }

    impl FollowerRunner for FakeFollowerRunner {
        fn leo(&self) -> u64 {
            self.shared.leo.load(Ordering::SeqCst)
        }
        fn hw(&self) -> u64 {
            self.shared.hw.load(Ordering::SeqCst)
        }
        fn lease_expired(&self) -> bool {
            self.shared.lease_expired.load(Ordering::SeqCst)
        }
        fn mark_leader_disconnected(&self) {
            self.shared.disconnected.store(true, Ordering::SeqCst);
        }
        fn stop(&self) {
            self.shared.stopped.store(true, Ordering::SeqCst);
        }
    }

    struct FakeFollowerFactory {
        handles: Mutex<Vec<Arc<FakeFollowerHandle>>>,
    }

    impl FakeFollowerFactory {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                handles: Mutex::new(Vec::new()),
            })
        }
    }

    impl FollowerRunnerFactory for FakeFollowerFactory {
        /// Mirrors the real `GlueFollowerFactory`, which answers an authorized
        /// `Hello` with exactly one `accepted: true` `HelloAck`
        /// (`follower::run_follower_stream_with_hello`) and then keeps driving
        /// the stream. Acking matters for the tests below: `accept_stream`
        /// itself only ever writes a REJECT ack, so without an accept ack here
        /// an accepted Hello and a silently-dropped one are indistinguishable
        /// to whoever dialed.
        fn spawn(
            &self,
            _assignment: &PartitionAssignment,
            hello: ReplHello,
            _leader_recv: BusRecv,
            mut leader_send: BusSend,
        ) -> Result<Box<dyn FollowerRunner>, ReplError> {
            let ack = ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                // The real runner stores the Hello's epoch on the local
                // partition before acking; this fake has no partition.
                follower_epoch: hello.leader_epoch,
                // The value `accept_stream` just gated on.
                environment: hello.environment,
                reject: None,
            };
            // `spawn` is sync and the frame write is not, so the ack goes out
            // on its own task — the same shape the real factory has (it hands
            // both halves to a spawned stream driver). Unlike the real one this
            // fake does not keep driving the stream after acking, so the read
            // half is dropped: nothing here asserts on stream liveness, only on
            // the verdict frame.
            tokio::spawn(async move {
                let _ = frames::write_frame(&mut leader_send, &ReplFrame::HelloAck(ack)).await;
            });
            let shared = Arc::new(FakeFollowerHandle {
                leo: AtomicU64::new(0),
                hw: AtomicU64::new(0),
                lease_expired: AtomicBool::new(false),
                disconnected: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            });
            self.handles.lock().push(Arc::clone(&shared));
            Ok(Box::new(FakeFollowerRunner { shared }))
        }
    }

    #[derive(Default)]
    struct FakeAudit {
        failovers: Mutex<u32>,
        transfers: Mutex<u32>,
        evictions: Mutex<u32>,
    }

    impl ReplAudit for FakeAudit {
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
            *self.failovers.lock() += 1;
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
            *self.transfers.lock() += 1;
        }
        fn evicted(&self, _node_id: &str, _reason: &str, _count: u32) {
            *self.evictions.lock() += 1;
        }
    }

    struct Fixture {
        manager: Arc<ReplicationManager>,
        transport: Arc<FakeTransport>,
        assignments: Arc<FakeAssignmentStore>,
        ledger: Arc<FakeLedger>,
        leader_factory: Arc<FakeLeaderFactory>,
        follower_factory: Arc<FakeFollowerFactory>,
        audit: Arc<FakeAudit>,
    }

    fn build(local_node_id: &str) -> Fixture {
        let transport = Arc::new(FakeTransport::new());
        let ledger = Arc::new(FakeLedger::new());
        let assignments = Arc::new(FakeAssignmentStore::new());
        let leader_factory = FakeLeaderFactory::new();
        let follower_factory = FakeFollowerFactory::new();
        let audit = Arc::new(FakeAudit::default());
        let manager = ReplicationManager::new(ReplicationManagerConfig {
            local_node_id: local_node_id.to_string(),
            local_env: NodeEnvironment::Prod,
            transport: transport.clone(),
            ledger: ledger.clone(),
            assignments: assignments.clone(),
            leader_factory: leader_factory.clone(),
            follower_factory: follower_factory.clone(),
            audit: audit.clone(),
            leo_query_timeout: Duration::from_millis(60),
            majority_await_timeout: Duration::from_millis(150),
        });
        Fixture {
            manager,
            transport,
            assignments,
            ledger,
            leader_factory,
            follower_factory,
            audit,
        }
    }

    // ---- apply_assignment: role per node ---------------------------------

    #[tokio::test]
    async fn apply_assignment_gives_each_node_the_correct_local_role() {
        let leader_fx = build("l");
        let a = assignment("org", "orders", 0, "l", &["l", "f1", "f2"], &["l", "f1"], 1);
        leader_fx.manager.apply_assignment(a.clone()).await;
        assert_eq!(
            leader_fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 1 }
        );

        let follower_fx = build("f1");
        follower_fx.manager.apply_assignment(a.clone()).await;
        assert_eq!(
            follower_fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "l".to_string(),
                epoch: 1,
            }
        );

        let bystander_fx = build("z");
        bystander_fx.manager.apply_assignment(a).await;
        assert_eq!(
            bystander_fx.manager.role("org", "orders", 0),
            PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment
            }
        );
    }

    #[tokio::test]
    async fn apply_assignment_spawns_the_leader_without_dialing() {
        let fx = build("l");
        let a = assignment("org", "t", 0, "l", &["l", "f1", "f2"], &["l"], 1);
        fx.manager.apply_assignment(a).await;
        // The manager never dials on the apply path (NON-NEGOTIABLE note on
        // `apply_assignment`): a dead peer's connect timeout must never delay
        // the registry insert or the leader's own serving capability. The
        // dialing is the glue supervisor's job — one stream-less supervisor
        // per other replica (glue's `spawn_with_epoch_mode`), covered by the
        // glue unit tests and the three-node handshake suite.
        assert_eq!(fx.transport.dials("f1"), 0);
        assert_eq!(fx.transport.dials("f2"), 0);
        assert_eq!(fx.leader_factory.spawned.lock().len(), 1);
    }

    // ---- full election happy path ----------------------------------------

    #[tokio::test]
    async fn lease_expiry_election_reaches_promoted_with_epoch_plus_one() {
        let fx = build("f1");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            5,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;

        // f2 answers the LeoQuery at the SAME leo as f1's own (no
        // `FollowerRunner` is attached in this fixture, so f1's own_leo
        // falls back to 0 — see `FollowerRunner::leo`'s doc) — a tie,
        // which f1 wins on the lowest-node_id rule (K-M2-3). "l" (crashed)
        // never answers at all.
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key.clone()).await;

        // Majority: propose succeeded (fake store admits epoch 5 -> 6),
        // and the ledger reports f2 acknowledged the op.
        let stored = fx.assignments.stored(&key).expect("assignment stored");
        assert_eq!(stored.leader_epoch, 6);
        assert_eq!(stored.leader_node_id, "f1");

        // Find the op id the fake store actually admitted for this key by
        // asking the ledger to acknowledge every candidate id 1..=8 is
        // wasteful; instead poll via the manager's own retry path: since
        // `majority_await_timeout` is 150 ms and nobody acked yet, the
        // attempt should currently be `AwaitingMajority` or already
        // `Abandoned{NoMajority}` — supply the ack out from under it by
        // scripting the ledger for ALL plausible ids, then retry once.
        for raw in 1u8..=4 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }
        fx.manager.apply_assignment(stored.clone()).await;
        fx.manager.run_election(key.clone()).await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 7 }
        );
        assert_eq!(*fx.audit.failovers.lock(), 1);
    }

    // ---- promotion dispatches the truncate it derived -------------------

    /// K-M2-1's `execute_promotion_actions` half of truncate-on-divergence.
    /// `election.rs`'s own test covers *which* replica gets truncated to
    /// *what* offset; this is the only place that decision is carried onto
    /// the `LeaderHandle` a follower stream can actually observe, and
    /// nothing else in this module ever reads `FakeLeaderHandle::truncated`
    /// — without this test the action could be dropped on the floor there
    /// and every other check would still pass.
    #[tokio::test]
    async fn promotion_dispatches_the_truncate_target_onto_the_new_leader_handle() {
        let fx = build("f1");
        let key: PartitionKey = ("org".to_string(), "orders".to_string(), 0u32);
        let proposed = assignment(
            "org",
            "orders",
            0,
            "f1",
            &["l", "f1", "f2"],
            &["f1", "f2"],
            6,
        );

        fx.manager
            .execute_promotion_actions(
                &key,
                &proposed,
                vec![
                    PromotionAction::SetLeaderEpoch(6),
                    PromotionAction::StartFeeders,
                    PromotionAction::SendTruncate {
                        node: "f2".to_string(),
                        to: 5,
                    },
                ],
            )
            .await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 6 },
            "promotion actions must leave this node leader at the new epoch"
        );
        let handles = fx.leader_factory.handles.lock();
        let handle = handles
            .last()
            .expect("promotion must spawn exactly one leader handle");
        assert_eq!(
            *handle.truncated.lock(),
            vec![("f2".to_string(), 5)],
            "the derived truncate target must reach the handle that owns f2's stream"
        );
    }

    // ---- no majority: abandon, then retryable -----------------------------

    #[tokio::test]
    async fn no_majority_abandons_and_a_later_retry_can_still_succeed() {
        let fx = build("f1");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            1,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        // Tie at leo 0 (no `FollowerRunner` attached — f1's own_leo
        // defaults to 0), which f1 wins on the lowest-node_id rule.
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        // Nobody acks -> majority window (150 ms) elapses -> Abandoned.
        fx.manager.run_election(key.clone()).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "l".to_string(),
                epoch: 1,
            }
        );

        // Retry: this time the ledger reports f2 acked whichever op id
        // comes out (the fake store's ids are deterministic small bytes).
        for raw in 1u8..=6 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }
        fx.manager.run_election(key.clone()).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        );
    }

    #[tokio::test]
    async fn propose_failure_leaves_the_partition_a_follower() {
        let fx = build("f1");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        fx.assignments.fail_next_propose();

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "l".to_string(),
                epoch: 1,
            }
        );
    }

    // ---- two candidates, same epoch: only the lower node_id wins ----------

    #[test]
    fn fake_assignment_store_epoch_gate_admits_only_the_lower_node_id_at_a_tie() {
        let store = FakeAssignmentStore::new();
        let base = assignment("org", "t", 0, "l", &["l", "a", "b"], &["l", "a", "b"], 1);
        store.seed(base.clone());

        let mut from_b = base.clone();
        from_b.leader_node_id = "b".to_string();
        from_b.leader_epoch = 2;
        store.propose(from_b.clone()).expect("b proposes first");
        assert_eq!(
            store.get("org", "t", 0).unwrap().unwrap().leader_node_id,
            "b"
        );

        // "a" proposes the SAME epoch (a genuine split-vote race) — since
        // "a" < "b" lexicographically, this must win over the already
        // stored "b" entry despite arriving second.
        let mut from_a = base.clone();
        from_a.leader_node_id = "a".to_string();
        from_a.leader_epoch = 2;
        store.propose(from_a).expect("a proposes second");
        assert_eq!(
            store.get("org", "t", 0).unwrap().unwrap().leader_node_id,
            "a"
        );

        // A third, higher-node-id proposal at the SAME epoch must now lose
        // against the already-admitted "a".
        let mut from_c = base;
        from_c.leader_node_id = "c".to_string();
        from_c.leader_epoch = 2;
        store.propose(from_c).expect("c proposes third");
        assert_eq!(
            store.get("org", "t", 0).unwrap().unwrap().leader_node_id,
            "a"
        );
    }

    // ---- candidate not in ISR never proposes (via the manager) ------------

    #[tokio::test]
    async fn manager_never_proposes_when_local_node_is_not_in_isr() {
        let fx = build("f1");
        // f1 is a replica but NOT in the ISR.
        let a = assignment("org", "orders", 0, "l", &["l", "f1", "f2"], &["l", "f2"], 1);
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key).await;

        assert!(
            fx.assignments
                .stored(&("org".into(), "orders".into(), 0))
                .unwrap()
                .leader_node_id
                == "l"
        );
        assert_eq!(fx.transport.dials("l"), 0);
        assert_eq!(fx.transport.dials("f2"), 0);
    }

    // ---- preflight ---------------------------------------------------------

    #[tokio::test]
    async fn preflight_rejects_not_enough_replicas() {
        let fx = build("l");
        // RF=3 -> min_isr=2, but ISR only has the leader itself.
        let a = assignment("org", "orders", 0, "l", &["l", "f1", "f2"], &["l"], 1);
        fx.manager.apply_assignment(a).await;
        let err = fx
            .manager
            .preflight("org", "orders", 0, Acks::Quorum)
            .unwrap_err();
        assert!(matches!(
            err,
            ReplError::NotEnoughReplicas {
                isr: 1,
                required: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn preflight_ok_for_rf1() {
        let fx = build("solo");
        let a = assignment("org", "orders", 0, "solo", &["solo"], &["solo"], 3);
        fx.manager.apply_assignment(a).await;
        assert_eq!(
            fx.manager
                .preflight("org", "orders", 0, Acks::Leader)
                .unwrap(),
            3
        );
    }

    /// T1's finding (4): `preflight`'s `min_isr` gate and `snapshot()`'s
    /// `isr` field must both track the LIVE ISR (`LeaderHandle::isr`,
    /// stood in here by a `FakeLeaderHandle` mutated out-of-band via
    /// `set_isr`, exactly as `PartitionLeader::reconcile_follower`
    /// mutates the real one on an ack timeout/reconnect) rather than the
    /// static `PartitionAssignment.isr` last seen at `apply_assignment`
    /// time — "stop one follower" (simulated: shrink the live ISR without
    /// touching the registered assignment at all) must make BOTH observe
    /// the shrink immediately, refuse a write once below `min_isr`, then
    /// recover once the follower "restarts" (the live ISR expands again).
    #[tokio::test]
    async fn preflight_and_snapshot_track_the_live_isr_not_the_static_assignment() {
        let fx = build("l");
        // RF=3 -> min_isr=2. The registered assignment's OWN isr field
        // stays `[l, f1, f2]` for the whole test — only the live
        // `FakeLeaderHandle` is mutated, proving neither read path falls
        // back to the stale static field.
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            1,
        );
        fx.manager.apply_assignment(a).await;
        assert_eq!(
            fx.manager
                .preflight("org", "orders", 0, Acks::Quorum)
                .unwrap(),
            1,
            "full live ISR must satisfy min_isr=2"
        );
        let snap = fx.manager.snapshot("org", Some("orders"));
        assert_eq!(snap.partitions[0].isr.len(), 3);

        // "Stop one follower": the live ISR shrinks to `[l, f1]` — still
        // >= min_isr=2, so preflight must still succeed.
        let handle = fx.leader_factory.handles.lock()[0].clone();
        handle.set_isr(vec!["l".to_string(), "f1".to_string()]);
        assert_eq!(
            fx.manager
                .preflight("org", "orders", 0, Acks::Quorum)
                .unwrap(),
            1
        );
        let snap = fx.manager.snapshot("org", Some("orders"));
        assert_eq!(
            snap.partitions[0].isr,
            vec!["l".to_string(), "f1".to_string()],
            "snapshot must reflect the live shrink immediately, not the static [l,f1,f2]"
        );

        // "Stop the second follower": live ISR shrinks to `[l]` alone,
        // below min_isr=2 — preflight must now refuse.
        handle.set_isr(vec!["l".to_string()]);
        let err = fx
            .manager
            .preflight("org", "orders", 0, Acks::Quorum)
            .unwrap_err();
        assert!(matches!(
            err,
            ReplError::NotEnoughReplicas {
                isr: 1,
                required: 2,
                ..
            }
        ));
        let snap = fx.manager.snapshot("org", Some("orders"));
        assert_eq!(snap.partitions[0].isr, vec!["l".to_string()]);

        // "Restart": the follower rejoins the live ISR — preflight must
        // succeed again without any new `apply_assignment` call.
        handle.set_isr(vec!["l".to_string(), "f1".to_string(), "f2".to_string()]);
        assert_eq!(
            fx.manager
                .preflight("org", "orders", 0, Acks::Quorum)
                .unwrap(),
            1
        );
        let snap = fx.manager.snapshot("org", Some("orders"));
        assert_eq!(snap.partitions[0].isr.len(), 3);
    }

    // ---- transfer_leader ----------------------------------------------------

    #[tokio::test]
    async fn transfer_leader_happy_path_steps_down_and_bumps_epoch() {
        let fx = build("l");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            4,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 4 }
        );

        // Ack whatever op id `propose` mints so majority is reached
        // immediately (no polling loop needed for this synchronous path).
        for raw in 1u8..=4 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f1".to_string()]);
        }

        let epoch = fx
            .manager
            .transfer_leader("org", "orders", 0, "f1")
            .expect("transfer succeeds");
        assert_eq!(epoch, 5);
        assert_eq!(*fx.audit.transfers.lock(), 1);
        // Stepped down locally: no longer reporting Leader (corrected by
        // the next `apply_assignment` once the op materializes back).
        assert_ne!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 4 }
        );
    }

    #[tokio::test]
    async fn transfer_leader_rejects_a_target_outside_the_isr() {
        let fx = build("l");
        let a = assignment("org", "orders", 0, "l", &["l", "f1", "f2"], &["l", "f1"], 1);
        fx.manager.apply_assignment(a).await;
        let err = fx
            .manager
            .transfer_leader("org", "orders", 0, "f2")
            .unwrap_err();
        assert!(matches!(err, ReplError::NotAReplica { node_id, .. } if node_id == "f2"));
    }

    // ---- reassign/evict bump the epoch (T1's finding (2)) -----------------

    /// `FakeAssignmentStore::propose`'s admission gate mirrors
    /// `core_materializer::apply_bus_partition_assignment` exactly (that
    /// gate's own doc comment): a proposal at the SAME epoch as what is
    /// already stored is only admitted if its `leader_node_id` is LOWER
    /// than the stored one. A same-leader `reassign` therefore needs a
    /// strictly higher epoch to ever materialize — this test fails at
    /// `stored.leader_epoch, 4` if `reassign` stops bumping the epoch
    /// again.
    #[tokio::test]
    async fn reassign_bumps_the_epoch_so_a_same_leader_change_is_admitted_by_the_materializer_gate()
    {
        let fx = build("l");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            3,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;

        let touched = fx
            .manager
            .reassign(
                "org",
                "orders",
                Some(0),
                &["l".to_string(), "f1".to_string(), "f3".to_string()],
            )
            .expect("reassign succeeds");
        assert_eq!(touched, 1);

        let stored = fx
            .assignments
            .stored(&("org".to_string(), "orders".to_string(), 0))
            .expect("assignment stored");
        assert_eq!(
            stored.leader_epoch, 4,
            "a same-leader reassign must bump the epoch or the materializer's \
             own admission gate (== epoch requires a LOWER leader_node_id) \
             silently drops it (Ok(0), no error)"
        );
        assert_eq!(
            stored.replicas,
            vec!["l".to_string(), "f1".to_string(), "f3".to_string()]
        );
        // Fala 4 finding: `reassign` narrowing the ISR (f2 was in the
        // seeded isr, is not in the new replica set, so `isr.retain` drops
        // it) must bump `isr_shrink_total` the same way `evict_node_from_
        // replica_sets` does — this is the second (and only other) place
        // this manager itself removes a member from an assignment's `isr`.
        assert_eq!(
            fx.manager.isr_shrink_total(),
            1,
            "reassign narrowed the ISR (f2 dropped) but isr_shrink_total was not bumped"
        );
    }

    /// Same reasoning as `reassign` above, for `evict_node_from_replica_
    /// sets` (`dispatch/environment.rs`'s only production caller).
    #[tokio::test]
    async fn evict_node_from_replica_sets_bumps_the_epoch_so_it_is_admitted() {
        let fx = build("l");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            7,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;

        let touched = fx
            .manager
            .evict_node_from_replica_sets("f2", "env_change")
            .expect("evict succeeds");
        assert_eq!(touched, 1);

        let stored = fx
            .assignments
            .stored(&("org".to_string(), "orders".to_string(), 0))
            .expect("assignment stored");
        assert_eq!(
            stored.leader_epoch, 8,
            "eviction must bump the epoch (same admission-gate reasoning as reassign)"
        );
        assert!(!stored.replicas.iter().any(|r| r == "f2"));
        assert_eq!(*fx.audit.evictions.lock(), 1);
    }

    // ---- PeerDisconnected accelerates --------------------------------------

    #[tokio::test]
    async fn peer_disconnected_marks_the_matching_follower_runner() {
        let fx = build("f1");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;

        let (client, server) = tokio::io::duplex(4096);
        let (mut client_recv, mut client_send) = split(client);
        let hello = ReplFrame::Hello(frames::ReplHello {
            org_id: "org".into(),
            topic: "orders".into(),
            partition: 0,
            leader_node_id: "l".into(),
            leader_epoch: 1,
            replicas: vec!["l".into(), "f1".into()],
            environment: NodeEnvironment::Prod,
        });
        let (server_recv, server_send) = split(server);
        tokio::spawn(async move {
            let mut send = client_send;
            frames::write_frame(&mut send, &hello).await.unwrap();
            let mut buf = [0u8; 1];
            let _ = client_recv.read(&mut buf).await;
        });
        fx.manager
            .accept_stream(
                "l".to_string(),
                Box::new(server_recv),
                Box::new(server_send),
            )
            .await;

        assert_eq!(fx.follower_factory.handles.lock().len(), 1);
        let handle = fx.follower_factory.handles.lock()[0].clone();
        assert!(!handle.disconnected.load(Ordering::SeqCst));

        fx.manager.on_peer_disconnected("l");
        assert!(handle.disconnected.load(Ordering::SeqCst));
    }

    // ---- Hello vs. the replica's own assignment materialization (wave 3) --
    //
    // `Transport` here is a fake and the duplex is in-memory, but every
    // other moving part is production code: the real `accept_stream`, its
    // real verdict order, the real `apply_assignment`, and a
    // `FakeAssignmentStore` standing in for the materialized
    // `bus_partition_assignments` table `init.rs`'s poll reads. These four
    // tests are the manager-level reproduction of the one defect
    // `tests/process_three_node_bus_failover.rs` could only show as a
    // symptom (`isr=1, required=2` on a publish issued after all three
    // nodes already reported their role): a leader dialing a replica whose
    // own registry is behind the ledger.

    /// The `ReplHello` a leader authorizes itself with for `a` — the same
    /// fields `leader::run_follower_stream` puts on the wire.
    fn hello_from(a: &PartitionAssignment) -> ReplHello {
        ReplHello {
            org_id: a.org_id.clone(),
            topic: a.topic.clone(),
            partition: a.partition,
            leader_node_id: a.leader_node_id.clone(),
            leader_epoch: a.leader_epoch,
            replicas: a.replicas.clone(),
            environment: NodeEnvironment::Prod,
        }
    }

    /// Writes one `Hello` at `manager`'s real `accept_stream` over an
    /// in-memory duplex and returns the `HelloAck` it gets back. Both halves
    /// of the leader side stay alive until the ack is read, so a `200 OK`
    /// means "accepted on this stream", not "accepted then hung up".
    async fn hello_roundtrip(manager: Arc<ReplicationManager>, hello: ReplHello) -> ReplHelloAck {
        let (leader_side, follower_side) = tokio::io::duplex(16 * 1024);
        let (mut leader_recv, mut leader_send) = split(leader_side);
        let (follower_recv, follower_send) = split(follower_side);
        tokio::spawn(async move {
            manager
                .accept_stream(
                    "leader".to_string(),
                    Box::new(follower_recv),
                    Box::new(follower_send),
                )
                .await;
        });
        frames::write_frame(&mut leader_send, &ReplFrame::Hello(hello))
            .await
            .expect("write Hello");
        match frames::read_frame(&mut leader_recv)
            .await
            .expect("read HelloAck")
        {
            ReplFrame::HelloAck(ack) => ack,
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hello_for_a_materialized_but_unapplied_assignment_is_reconciled_not_rejected() {
        let fx = build("f1");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        // The ledger row is materialized; this node's assignment poll has
        // not run, so `apply_assignment` was never called and the registry
        // is empty. Pre-fix, this is exactly where `TopicUnknown` was.
        fx.assignments.seed(a.clone());

        let started = Instant::now();
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;

        assert!(
            ack.accepted,
            "a Hello for a partition the ledger already assigns here must not be bounced (got {:?})",
            ack.reject
        );
        assert!(
            started.elapsed() < ASSIGNMENT_AWAIT / 2,
            "the store read-through must resolve the miss on the spot, not after a wait ({:?} elapsed)",
            started.elapsed()
        );
        // And the reconciliation has to be the REAL one: role registered and
        // this stream's runner attached, so the leader's live ISR can grow.
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower { .. }
        ));
        assert_eq!(
            fx.follower_factory.handles.lock().len(),
            1,
            "the reconciled assignment must attach a follower runner on the Hello's own stream"
        );
    }

    #[tokio::test]
    async fn a_hello_waits_for_a_still_in_flight_assignment_instead_of_bouncing_it() {
        let fx = build("f1");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        // The ledger op has not arrived at all yet; it lands mid-wait, the
        // way a slow sync push lands one poll tick after the leader dialed.
        let store = Arc::clone(&fx.assignments);
        let late = a.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ASSIGNMENT_AWAIT_RETRY * 2).await;
            store.seed(late);
        });

        let started = Instant::now();
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;

        assert!(
            ack.accepted,
            "an in-flight assignment must be awaited, not rejected out of hand (got {:?})",
            ack.reject
        );
        assert!(
            started.elapsed() >= ASSIGNMENT_AWAIT_RETRY,
            "this is the wait path; a sub-tick return would mean it never waited"
        );
        assert!(
            started.elapsed() < ASSIGNMENT_AWAIT / 2,
            "the wait must end when the row lands, not when the budget runs out ({:?} elapsed)",
            started.elapsed()
        );
        assert_eq!(fx.follower_factory.handles.lock().len(), 1);
    }

    #[tokio::test]
    async fn an_assignment_applied_by_the_poll_wakes_a_parked_hello_at_once() {
        let fx = build("f1");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        // The other half of the real sync path: `init.rs`'s replay/poll
        // calling `apply_assignment` (the poll reads `list_for_node`, which
        // this store serves from the same rows) while a Hello is parked. The
        // row never becomes readable through `get` here, so the ONLY thing
        // that can admit this Hello in time is the `assignments_changed`
        // wake — which is what makes the wait event-driven rather than a
        // blind re-read loop.
        let apply = {
            let manager = Arc::clone(&fx.manager);
            let a = a.clone();
            async move {
                tokio::time::sleep(ASSIGNMENT_AWAIT_RETRY * 3).await;
                manager.apply_assignment(a).await;
            }
        };
        tokio::spawn(apply);

        let started = Instant::now();
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;

        assert!(
            ack.accepted,
            "a Hello parked on a not-yet-applied assignment must be admitted when it is applied (got {:?})",
            ack.reject
        );
        assert!(
            started.elapsed() < ASSIGNMENT_AWAIT / 2,
            "the wake must admit the Hello, not the expiry of the budget ({:?} elapsed)",
            started.elapsed()
        );
        assert_eq!(fx.follower_factory.handles.lock().len(), 1);
    }

    #[tokio::test]
    async fn a_ledger_row_that_excludes_this_node_is_answered_without_the_wait() {
        let fx = build("f1");
        // Materialized, readable, and this node is not in it: "not a replica
        // of this" is an answer, so there must be no 2 s hold on it.
        fx.assignments.seed(assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "someone-else"],
            &["l"],
            1,
        ));

        let started = Instant::now();
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "orders",
                0,
                "l",
                &["l", "f1"],
                &["l", "f1"],
                1,
            )),
        )
        .await;

        assert!(!ack.accepted, "a non-replica must still be rejected");
        assert!(
            started.elapsed() < ASSIGNMENT_AWAIT / 4,
            "an authoritative row must short-circuit the wait ({:?} elapsed)",
            started.elapsed()
        );
        assert_eq!(
            fx.follower_factory.handles.lock().len(),
            0,
            "nothing may be attached for a partition this node is not a replica of"
        );
    }

    #[tokio::test]
    async fn a_hello_for_a_partition_that_never_materializes_still_rejects() {
        let fx = build("f1");
        let started = Instant::now();
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "ghost",
                0,
                "l",
                &["l", "f1"],
                &["l", "f1"],
                1,
            )),
        )
        .await;

        assert!(!ack.accepted);
        assert_eq!(
            ack.reject,
            Some(ReplReject::TopicUnknown),
            "the hold is bounded and ends in the same honest rejection as before, never a silent hang"
        );
        assert!(
            started.elapsed() >= ASSIGNMENT_AWAIT,
            "the full budget must be spent before giving up ({:?} elapsed)",
            started.elapsed()
        );
        assert_eq!(fx.follower_factory.handles.lock().len(), 0);
        // And nothing was invented on the way out: the held Hello expired
        // with the registry exactly as empty as it started, so this node has
        // no opinion about a partition the ledger never assigned it.
        assert!(matches!(
            fx.manager.role("org", "ghost", 0),
            PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment
            }
        ));
    }

    // ---- delete (assignment removed) tears down without looping -----------

    #[tokio::test]
    async fn removing_the_assignment_tears_down_leader_state_without_looping() {
        let fx = build("l");
        let a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        fx.manager.apply_assignment(a).await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { .. }
        ));

        // Materializer delivers a "no longer a replica" assignment for
        // this node (the topic/partition was deleted).
        let deleted = assignment("org", "orders", 0, "f1", &["f1"], &["f1"], 1);
        fx.manager.apply_assignment(deleted).await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment
            }
        );
        // Idempotent: applying the same "not a replica" state again must
        // not panic, hang, or re-dial anyone (no retry loop) — the dial
        // count is whatever it already was (1, from the original leader
        // setup above) and must not grow.
        let dials_after_delete = fx.transport.dials("f1");
        let deleted_again = assignment("org", "orders", 0, "f1", &["f1"], &["f1"], 1);
        fx.manager.apply_assignment(deleted_again).await;
        assert_eq!(fx.transport.dials("f1"), dials_after_delete);
    }

    // Silences an unused-import lint on `ReplTruncate`, pulled in for
    // readability of the frame-construction helpers above even on
    // configurations that end up not exercising every branch.
    #[allow(dead_code)]
    fn _unused_type_anchor(_: ReplTruncate) {}

    // ---- LeoQuery on the accept path (P8 exclusive promotion, half 0) ------
    //
    // A candidate's `LeoQuery` arrives on a FRESH stream whose first frame
    // is not a `Hello`; before the `LeoQuery` arm existed in
    // `accept_stream`, that stream was dropped silently, every candidate's
    // reply set stayed empty, `choose_candidate` fell back to self on BOTH
    // survivors of a crashed leader, and both proposed the same next epoch
    // — the P8 tie the node-id tie-break could never resolve because the
    // leo exchange never happened. These tests drive `accept_stream`
    // directly with a `LeoQuery`-first duplex.

    /// Writes one frame at `manager`'s real `accept_stream` over an
    /// in-memory duplex and returns the first reply frame.
    async fn accept_roundtrip(manager: Arc<ReplicationManager>, outbound: &ReplFrame) -> ReplFrame {
        let (caller_side, accept_side) = tokio::io::duplex(16 * 1024);
        let (mut caller_recv, mut caller_send) = split(caller_side);
        let (accept_recv, accept_send) = split(accept_side);
        tokio::spawn(async move {
            manager
                .accept_stream(
                    "peer".to_string(),
                    Box::new(accept_recv),
                    Box::new(accept_send),
                )
                .await;
        });
        frames::write_frame(&mut caller_send, outbound)
            .await
            .expect("write first frame");
        frames::read_frame(&mut caller_recv)
            .await
            .expect("read reply frame")
    }

    #[tokio::test]
    async fn accept_stream_answers_a_leo_query_for_a_partition_this_node_follows() {
        let fx = build("f1");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            3,
        );
        fx.manager.apply_assignment(a).await;

        let reply = accept_roundtrip(
            Arc::clone(&fx.manager),
            &ReplFrame::LeoQuery(ReplLeoQuery {
                org_id: "org".into(),
                topic: "orders".into(),
                partition: 0,
                known_epoch: 3,
            }),
        )
        .await;
        match reply {
            ReplFrame::LeoReply(r) => {
                // The fake follower runner reports leo/hw 0; the epoch comes
                // from the registry's assignment and `in_isr` from having a
                // live follower role.
                assert_eq!((r.leo, r.hw, r.leader_epoch, r.in_isr), (0, 0, 3, true));
            }
            other => panic!("expected LeoReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn accept_stream_answers_a_leo_query_for_a_partition_this_node_leads() {
        let fx = build("l");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            4,
        );
        fx.manager.apply_assignment(a).await;
        // The fake leader handle carries hw/leo 0; only the routing (leader
        // entry, `in_isr: true`, assignment epoch) is under test here.
        let reply = accept_roundtrip(
            Arc::clone(&fx.manager),
            &ReplFrame::LeoQuery(ReplLeoQuery {
                org_id: "org".into(),
                topic: "orders".into(),
                partition: 0,
                known_epoch: 4,
            }),
        )
        .await;
        match reply {
            ReplFrame::LeoReply(r) => {
                assert_eq!((r.leo, r.hw, r.leader_epoch, r.in_isr), (0, 0, 4, true));
            }
            other => panic!("expected LeoReply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_leo_query_for_an_unknown_partition_answers_zeroes_without_hanging() {
        let fx = build("f1");
        let reply = accept_roundtrip(
            Arc::clone(&fx.manager),
            &ReplFrame::LeoQuery(ReplLeoQuery {
                org_id: "org".into(),
                topic: "ghost".into(),
                partition: 9,
                known_epoch: 0,
            }),
        )
        .await;
        match reply {
            ReplFrame::LeoReply(r) => {
                assert_eq!((r.leo, r.hw, r.leader_epoch, r.in_isr), (0, 0, 0, false));
            }
            other => panic!("expected LeoReply, got {other:?}"),
        }
    }

    // ---- Exclusive promotion: fencing on the Hello (P8, half 1) ------------
    //
    // Two simultaneous self-elections at the same epoch produced the ~48 s
    // mutual-`NotAReplica` livelock the 3-process chaos run measured: each
    // leader refused the other's Hello forever, so neither re-formed an ISR.
    // The deterministic rule (the materializer gate's own: higher epoch
    // wins; equal epoch, lower node id wins) now fences the LOSER on the
    // Hello itself — it stops serving and becomes the winner's follower.

    /// C holds a same-epoch leadership claim; B (lower node id — the
    /// deterministic winner) dials in. C must step down on the Hello and
    /// accept the stream as B's follower, not answer `NotAReplica`.
    #[tokio::test]
    async fn a_leader_receiving_an_equal_epoch_lower_id_peers_hello_fences_itself() {
        let fx = build("C");
        let own = assignment("org", "orders", 0, "C", &["B", "C"], &["B", "C"], 2);
        fx.manager.apply_assignment(own).await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        ));
        let leader_handle = fx.leader_factory.handles.lock()[0].clone();

        // B's Hello at the SAME epoch — the tie-break (lower node id) is
        // what makes B the winner, not a higher epoch.
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "orders",
                0,
                "B",
                &["B", "C"],
                &["B", "C"],
                2,
            )),
        )
        .await;
        assert!(
            ack.accepted,
            "the deterministic loser must accept the winner's Hello (got {:?})",
            ack.reject
        );
        assert!(leader_handle.stopped.load(Ordering::SeqCst),
            "fencing must stop this node's own leader handle — no further bytes under the old claim");
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "B".to_string(),
                epoch: 2,
            },
            "the loser must be a follower of the equal-epoch winner after the fence"
        );
    }

    /// Same shape but the winner is at a strictly HIGHER epoch — a leader
    /// rejoining (or still serving) against a partition that moved on must
    /// step down the same way.
    #[tokio::test]
    async fn a_leader_receiving_a_higher_epoch_peers_hello_fences_itself() {
        let fx = build("C");
        let own = assignment("org", "orders", 0, "C", &["B", "C"], &["B", "C"], 1);
        fx.manager.apply_assignment(own).await;
        let leader_handle = fx.leader_factory.handles.lock()[0].clone();

        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "orders",
                0,
                "B",
                &["B", "C"],
                &["B", "C"],
                2,
            )),
        )
        .await;
        assert!(ack.accepted);
        assert!(leader_handle.stopped.load(Ordering::SeqCst));
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower { leader_node_id, epoch: 2 } if leader_node_id == "B"
        ));
    }

    /// The inverse direction must stay a rejection: the node that WINS the
    /// deterministic rule (lower id at an equal epoch) does NOT step down
    /// for the loser's Hello — that is what makes the resolution converge
    /// instead of flip-flopping.
    #[tokio::test]
    async fn the_deterministic_winner_still_rejects_an_equal_epoch_losers_hello() {
        let fx = build("B");
        let own = assignment("org", "orders", 0, "B", &["B", "C"], &["B", "C"], 2);
        fx.manager.apply_assignment(own).await;
        let leader_handle = fx.leader_factory.handles.lock()[0].clone();

        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "orders",
                0,
                "C",
                &["B", "C"],
                &["B", "C"],
                2,
            )),
        )
        .await;
        assert!(
            !ack.accepted,
            "the equal-epoch winner must not be fenced by the loser's Hello"
        );
        assert!(!leader_handle.stopped.load(Ordering::SeqCst));
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        ));
    }

    /// A Hello whose replica set does not include this node never fences
    /// anything, even from a "winning" peer — the peer's claim is not
    /// about this partition's membership and must not tear down leadership.
    #[tokio::test]
    async fn a_winning_hello_that_excludes_this_node_does_not_fence_it() {
        let fx = build("C");
        let own = assignment("org", "orders", 0, "C", &["C", "D"], &["C", "D"], 2);
        fx.manager.apply_assignment(own).await;
        let leader_handle = fx.leader_factory.handles.lock()[0].clone();

        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org",
                "orders",
                0,
                "B",
                &["B", "D"],
                &["B", "D"],
                3,
            )),
        )
        .await;
        assert!(!ack.accepted);
        assert!(!leader_handle.stopped.load(Ordering::SeqCst));
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        ));
    }

    // ---- Exclusive promotion: settle against the ledger (P8, half 2) -------
    //
    // Majority admission counts OUTBOX acks for the candidate's OWN op —
    // but a peer acks a same-epoch assignment that LOSES the node-id
    // tie-break too (the materializer applies it as a no-op and the inbox
    // still acknowledges delivery). Without a final check against the
    // materialized row, both candidates promote. The settle check makes
    // the ledger row — the one thing that converged deterministically —
    // the last word.

    /// The store already settled on `b` at the epoch `f1` is proposing
    /// (`f1 > b` lexicographically, so `f1`'s own proposal lost the
    /// tie-break): the promotion must YIELD — no leader handle spawned,
    /// the node follows the stored leader instead.
    #[tokio::test]
    async fn promotion_yields_when_the_ledger_already_settled_on_a_lower_id_leader() {
        let fx = build("f1");
        // Last assignment: leader "l" at epoch 5, so f1's election proposes
        // epoch 6. The ledger row ALREADY holds (leader "b", epoch 6) —
        // b's own concurrent election won the materializer tie-break.
        let base = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            5,
        );
        fx.assignments.seed(base.clone());
        fx.manager.apply_assignment(base).await;
        let settled = assignment(
            "org",
            "orders",
            0,
            "b",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            6,
        );
        fx.assignments.seed(settled);

        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );
        // Ack whichever op id f1's propose mints so the majority resolves.
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key).await;

        assert!(
            fx.leader_factory.spawned.lock().is_empty(),
            "a promotion the ledger already settled against must not spawn a leader handle"
        );
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "b".to_string(),
                epoch: 6,
            },
            "the yielding candidate must become a follower of the settled leader"
        );
    }

    /// The mirror case: the ledger row names THIS node (or is still behind
    /// — the author's own op has not materialized locally yet), so the
    /// promotion proceeds exactly as before the settle check existed.
    #[tokio::test]
    async fn promotion_proceeds_when_the_ledger_agrees_or_has_not_caught_up() {
        let fx = build("f1");
        let base = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            5,
        );
        fx.assignments.seed(base.clone());
        fx.manager.apply_assignment(base).await;

        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key).await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 6 },
            "the settled election must still promote normally"
        );
        assert_eq!(fx.leader_factory.spawned.lock().len(), 1);
    }
}
