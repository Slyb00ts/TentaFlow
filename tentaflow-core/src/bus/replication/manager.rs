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
    self, LocalRole, LogPosition, PromotionAction, PromotionEvent, PromotionState,
};
use crate::bus::replication::frames::{
    self, ReplFrame, ReplHello, ReplHelloAck, ReplLeoQuery, ReplLeoReply, ReplReject,
};
use crate::bus::topics::Acks;
use crate::bus::{
    AckOutcome, PartitionReplicaInfo, PartitionRole, ReplError, ReplicaLagInfo, ReplicaNodeInfo,
    ReplicationCoordinator, ReplicationSnapshot, UnavailableReason,
};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::sync::ledger::{OperationId, SyncLedgerStore};

/// `(org_id, topic, partition)` — the registry's key everywhere in this
/// file.
///
/// plan-app-platform §1.6 asks for a 4-tuple with `BusInstanceId` leading.
/// Checked against the actual W4 shape instead of widening on the plan's
/// word alone: `ReplicationManager::registry` (below) is a field of ONE
/// `ReplicationManager`, and W4 gives every running TentaBus instance its
/// own manager with its own `registry` — `ReplicationManagerConfig::
/// instance_id`'s doc, `bus/replication/init.rs::init` (one manager per
/// `PartitionProvider::instance_id()`). No structure keyed by
/// `PartitionKey` is ever process-global or shared across two managers —
/// `AssignmentStore`/`LedgerAdmission` are shared (one ledger backs every
/// instance), but every one of their methods already takes `instance_id`
/// as an explicit argument (`AssignmentStore::get`'s own doc), never folds
/// it into a `PartitionKey`. So a 3-tuple is sufficient PROVIDED the one
/// thing that used to route by `PartitionKey` alone — the mesh's single
/// `ALPN_BUS` accept handler, formerly `ReplicationManager::
/// install_accept_handler` — now demuxes by instance FIRST, before any
/// `PartitionKey` lookup: `replication::router` reads the first frame's
/// `instance_id`, resolves the target manager, and only THEN hands the
/// frame to that manager's own `accept_hello`/`answer_leo_query`, which
/// look the rest up by this 3-tuple inside a registry only that one
/// instance's manager owns. `accept_hello`/`answer_leo_query` also recheck
/// the frame's `instance_id` against `self.instance_id` on their own
/// (belt-and-suspenders — the same "nothing may ever mix" reasoning as
/// `BusCallContext`/`check_instance`, §1.7), so a frame that somehow
/// reached the wrong manager without going through the router is still
/// refused, not silently answered from the wrong registry.
///
/// This type would need widening to a 4-tuple the day any of the above
/// stops being true — e.g. a single `ReplicationManager` ever serves more
/// than one instance's partitions in one `registry`, or a caller starts
/// looking a partition up in this registry WITHOUT having first resolved
/// the instance (bypassing `replication::router`).
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

/// One quorum wait's view of the leader handle it waits on, captured with
/// it so `successor_leader` can tell whether a rebuilt handle continues the
/// same log.
struct AckWait {
    handle: Arc<dyn LeaderHandle>,
    epoch: u32,
    leadership: u64,
    truncations: u64,
    /// The replica-set size the handle serves; `required_acks` is
    /// recomputed from it on every wake, since `acks=all` follows the live
    /// ISR.
    replicas: usize,
}

/// How often an `acks=all` wait re-reads the live ISR it must reach: a
/// follower dropped from the ISR mid-wait stops being waited for within
/// this long, instead of timing out a write every remaining replica has.
const ACKS_ALL_ISR_RECHECK: Duration = Duration::from_millis(50);

/// The replica count `acks` asks for under `assignment` — recomputed for
/// every handle a wait moves onto, since a rebuild may have changed the
/// replica set.
/// `acks=all` counts the in-sync replicas the handle serves now
/// (`live_isr`), never fewer than `election::availability_quorum`: a
/// follower dropped from the ISR is not waited for, and at RF ≤ 2 the
/// surviving replica alone completes the write.
fn required_acks(acks: Acks, rf: usize, live_isr: usize) -> u32 {
    match acks {
        Acks::Leader => 1,
        Acks::Quorum => election::min_isr_required(rf) as u32,
        Acks::All => live_isr.max(election::availability_quorum(rf)) as u32,
    }
}

/// `successor_leader`'s re-read tick while a same-node rebuild is between
/// stopping the old leader handle and attaching the new one (the old
/// handle's `stop()` flushes partition meta, measured at 124-302 ms).
const SUCCESSOR_POLL_INTERVAL: Duration = Duration::from_millis(5);

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
/// definition; `election::admitted_by_quorum` adds self back in).
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
    /// plan-app-platform §7 W4: `instance_id` scopes the lookup — two
    /// TentaBus instances never share a `PartitionKey`, but `manager.rs`'s
    /// own registry key (`(org, topic, partition)`) does not carry it, so
    /// every cold lookup against the store needs it passed in explicitly.
    fn get(
        &self,
        instance_id: &str,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError>;
    fn list_for_topic(
        &self,
        instance_id: &str,
        org: &str,
        topic: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError>;
    fn list_for_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError>;
    /// Submits `assignment` as a new ledger operation, returning its id for
    /// `LedgerAdmission::admitted_by` polling. Needs no separate instance
    /// parameter — `assignment.instance_id` already carries it.
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
    /// How many truncations have discarded records from the local log this
    /// handle leads (`tentaflow_bus::Partition::truncation_count`).
    fn truncation_count(&self) -> u64;
    /// Blocks (up to `timeout`) until `next_offset` is acknowledged by
    /// `required` replicas (this node included).
    fn await_acks(&self, next_offset: u64, required: u32, timeout: Duration) -> AckOutcome;
    /// K-M2-5: records a consumer group's offset commit for `ReplOffsets`
    /// coalescing.
    fn note_offset_commit(&self, group: &str, partition: u32, offset: u64, attempts: u32);
    /// K-M2-1: truncates `node`'s tail down to `to_offset` (a replica ahead
    /// of the new leader's own `leo` — see `election.rs`'s header).
    fn send_truncate(&self, node: &str, to_offset: u64);
    /// The highest `leader_epoch` a peer has PROVED to be newer than this
    /// leader's own claim, by refusing its outbound `Hello` with
    /// `ReplReject::StaleEpoch { have }` — the mirror of `FollowerRunner::
    /// last_hello_reject` for the leader side, and the signal
    /// `ReplicationManager::check_stale_leadership` steps down on. `None`
    /// (the default) means "no peer ever said so", which is the right
    /// answer for any handle that never dials.
    fn observed_stale_epoch(&self) -> Option<u32> {
        None
    }
    /// Whether a majority of the replica set (this leader counted) has
    /// acknowledged recently enough for this leader to still be the live
    /// one. What `answer_leo_query` reports as `leading`: a leader that
    /// stopped hearing its quorum — hung feeders, a partition — must stop
    /// holding candidates off, or it could never be replaced.
    fn holds_quorum_lease(&self) -> bool;
    /// This handle became the one serving its partition: publishes admitted
    /// through it may now land (`Partition::open_leader_writes`). Called
    /// under the registry guard that installs it, and only for that one
    /// handle — a spare spawned for the same term never opens them, so
    /// stopping the serving handle closes them however many spares linger.
    /// `stop()` closes what this opened.
    fn open_writes(&self);
    /// Claims this handle's term on the led partition's log
    /// (`tentaflow_bus::epochs`), so a majority of followers can confirm it
    /// and commit the earlier-term records below it without a record of this
    /// term. Called after `open_writes`, OUTSIDE the registry guard: the
    /// claim is a write to the partition's writer thread, queued behind its
    /// appends, and ends in an fsync.
    fn claim_term(&self);
    /// `Partition::log_epoch` of the led partition.
    fn log_epoch(&self) -> u32;
    /// `Partition::committed_offset` of the led partition.
    fn committed_offset(&self) -> u64;
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
    /// The leader epoch the local log was last written under
    /// (`Partition::log_epoch`, `election::LogPosition::epoch`) — not the
    /// epoch the accepted `Hello` stamped.
    fn log_epoch(&self) -> u32;
    /// `Partition::committed_offset` of the followed partition.
    fn committed(&self) -> u64;
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
    /// How long a `Follower` entry that no leader has dialed waits before
    /// `check_leases` treats its lease as expired, counted from when the
    /// entry was left without a runner. Without it a node whose leader died
    /// before dialing it would have no lease to expire and would never
    /// stand for election. It must outlast a LIVE leader's redial — the
    /// runner lease plus the leader's longest reconnect backoff and a
    /// connect attempt — or every restart of a follower would elect over
    /// a healthy leader.
    fn undialed_lease(&self) -> Duration;
    /// This node's own log for the assignment's partition, read from the
    /// local partition. An entry with no runner answers a `LeoQuery` and
    /// stands for election with it, never with zeros: a candidate that took
    /// its own log for empty would truncate every replica ahead of it to
    /// nothing. Blocking (it may open the partition); the manager calls it
    /// on the blocking pool.
    fn local_log_position(
        &self,
        assignment: &PartitionAssignment,
    ) -> Result<LogPosition, ReplError>;
    /// The lease the runners this factory spawns apply
    /// (`FollowerRunner::lease_expired`). A leader that has not held its
    /// quorum lease for this long steps down (`check_quorum_leases`).
    fn leader_lease(&self) -> Duration;
    /// Cuts the local log of `assignment`'s partition back to its committed
    /// offset (a leader-authority truncate keeping its own log epoch) and
    /// returns what is left. What is left is on a majority, so it is a
    /// prefix of every later term's chain. Blocking; called on the blocking
    /// pool.
    fn cut_to_committed(&self, assignment: &PartitionAssignment) -> Result<LogPosition, ReplError>;
    /// Raises the local partition's recognized leader epoch to `epoch`
    /// (never lowers it). A write this node admitted as leader of an older
    /// term is refused from then on (`Partition::append_batch_as_leader`).
    /// Blocking; called on the blocking pool.
    fn fence_to_epoch(&self, assignment: &PartitionAssignment, epoch: u32)
        -> Result<(), ReplError>;
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
    /// `Arc`, not `Box`: `await_acks` clones the handle out of the registry
    /// and blocks on the clone, so no DashMap shard guard is held for a
    /// publish's whole quorum wait (see `ReplicationCoordinator::await_acks`
    /// below).
    leader: Option<Arc<dyn LeaderHandle>>,
    follower: Option<Box<dyn FollowerRunner>>,
    /// When `follower` was last left empty (entry created, rebuilt, fenced,
    /// stepped down, runner replaced). A `Follower` entry without a runner
    /// has no lease of its own; `check_leases` counts one from here.
    unfollowed_since: Instant,
    /// Set when this node stepped down from a leadership a peer proved
    /// stale (`check_stale_leadership`): its log may end in a tail no
    /// majority ever held, written under a term that is over. Cleared once
    /// that tail is dealt with: cut back to the committed offset
    /// (`cut_stepped_down_log`, retried every lease tick) — or, when a newer
    /// leader's `Hello` got here first, left to that leader, whose
    /// handshake reconciles by the epochs the records were written in
    /// (`Partition::log_epoch`), dropped stream or not. Cutting under a live
    /// stream instead would drop records that leader already counted.
    /// Until cleared the entry neither stands nor may win anyone else's
    /// election (`ReplLeoReply::ineligible`).
    unreconciled: bool,
    /// The step-down cut of this entry's log is running
    /// (`cut_stepped_down_log`); every `Hello` is refused meanwhile
    /// (`ReplReject::Reconciling`).
    cutting: bool,
    /// Whether `run_election` already warned that the local log could not
    /// be read; cleared by the next successful read.
    log_read_warned: bool,
    /// Since when this entry's leader handle has not held its quorum lease
    /// (`LeaderHandle::holds_quorum_lease`); `None` while it holds it.
    quorum_lost_since: Option<Instant>,
    /// This node gave up leading this partition at this epoch
    /// (`check_quorum_leases`). A ledger row naming this node leader at that
    /// epoch or earlier makes it a follower, not a leader again — otherwise
    /// the next poll would restore exactly the leadership it just left.
    abdicated_epoch: Option<u32>,
    promotion: PromotionState,
    /// Identifies one unbroken stretch of this node's leadership of the
    /// partition: drawn fresh from `ReplicationManager::next_leadership`
    /// when the entry is created and every time its role leaves `Leader`,
    /// never while it stays `Leader` across a handle rebuild. A quorum wait
    /// may move onto a rebuilt handle only while this value is unchanged
    /// (`successor_leader`): once the role has left `Leader`, another
    /// node's authority may have cut this node's log below the offset
    /// being waited on.
    leadership: u64,
}

/// Whether `hello` leads the topic incarnation `held` belongs to. A leader
/// built before incarnations sends none (`ReplHello::topic_generation` is
/// `None`): unknown, so it is judged by epoch alone as that build was.
fn incarnation_matches(hello: &ReplHello, held: &PartitionAssignment) -> bool {
    hello
        .topic_generation
        .is_none_or(|theirs| theirs == held.topic_generation)
}

/// The ledger materializer's admission order for leadership claims
/// (`core_materializer::apply_bus_partition_assignment`): a higher epoch
/// wins, and at an equal epoch the lexicographically lower leader node id
/// wins. An empty `current` leader — a node that stepped down without
/// learning its successor (`check_stale_leadership`) — is outranked by every
/// named leader of the same epoch.
fn claim_outranks(epoch: u32, leader: &str, current: &PartitionAssignment) -> bool {
    epoch > current.leader_epoch
        || (epoch == current.leader_epoch
            && (current.leader_node_id.is_empty() || leader < current.leader_node_id.as_str()))
}

/// `claim_outranks`' complement for a claim that is neither newer nor the
/// same one — the one kind `apply_assignment` and `accept_hello` refuse.
fn claim_superseded(epoch: u32, leader: &str, current: &PartitionAssignment) -> bool {
    let same = epoch == current.leader_epoch && leader == current.leader_node_id;
    !same && !claim_outranks(epoch, leader, current)
}

/// `claim_superseded` for a whole ledger row: the topic incarnation decides
/// first (`PartitionAssignment::topic_generation` — a re-created topic
/// restarts at epoch 1), the claim order only within one incarnation.
fn assignment_superseded(incoming: &PartitionAssignment, current: &PartitionAssignment) -> bool {
    match incoming.topic_generation.cmp(&current.topic_generation) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => {
            claim_superseded(incoming.leader_epoch, &incoming.leader_node_id, current)
        }
    }
}

/// Whether two ledger rows describe the same follower term: a runner
/// attached under one keeps serving the other.
fn same_follower_term(incoming: &PartitionAssignment, current: &PartitionAssignment) -> bool {
    incoming.topic_generation == current.topic_generation
        && incoming.leader_epoch == current.leader_epoch
        && incoming.leader_node_id == current.leader_node_id
        && incoming.replicas == current.replicas
}

/// How many epochs past this node's ledger row an inbound `Hello` may claim
/// and still be adopted. A promotion reaches the ledger through majority
/// admission before its leader dials, so a live leader is at most a term or
/// two ahead of a lagging follower's row; a larger gap is a claim nobody
/// admitted, and adopting it would pin the entry above every real row.
const MAX_HELLO_EPOCH_LEAD: u32 = 2;

/// Why a candidate that just heard `reply` must not stand, if it must not.
/// `own_epoch` is the leader epoch of the candidate's own assignment view.
///
/// - A peer at a newer epoch proves that view stale: a newer row exists and
///   simply has not reached this node yet, and electing over it would only
///   produce a claim the ledger refuses.
/// - A peer that answers as the leader at this node's epoch (or later) is
///   the leader alive — silence on this node's follower stream means its
///   dial has not landed yet (a restart, a reconnect backoff), not that it
///   is gone. On a caught-up partition the tie-break would otherwise hand
///   leadership to whichever lower-id follower restarted.
/// - A peer whose follower stream is live and within its lease proves the
///   same from the other side, where the leader itself cannot say so (it
///   lost its quorum lease while this node was down, RF=2) or cannot be
///   reached from here (an isolated follower).
fn election_deferral(reply: &ReplLeoReply, own_epoch: u32) -> Option<&'static str> {
    if reply.leader_epoch > own_epoch {
        Some("a peer answered at a newer epoch; this node's assignment view is stale")
    } else if reply.leading && reply.leader_epoch >= own_epoch {
        Some("a peer answered as the live leader of this epoch")
    } else if reply.leader_alive && reply.leader_epoch >= own_epoch {
        Some("a peer still follows a live leader of this epoch")
    } else {
        None
    }
}

// ===== ReplicationManager ===================================================

pub struct ReplicationManagerConfig {
    /// plan-app-platform §1.6/§7 W4/W5: which TentaBus instance this
    /// manager serves (from `PartitionProvider::instance_id()`) — ONE
    /// manager per running instance (`init::init` builds exactly one),
    /// never shared. Two roles: (1) `PartitionKey`'s own doc explains why
    /// this manager's `registry` needs no instance component of its own —
    /// this field IS that component, held once per manager instead of once
    /// per entry; (2) the manager's own source of truth for every
    /// `AssignmentStore` call it makes without an existing
    /// `PartitionAssignment` (or `assignment.instance_id`) at hand. W5 adds
    /// a third: `accept_hello`/`answer_leo_query` compare an inbound
    /// frame's own `instance_id` against this field before touching
    /// `registry` at all, so a frame that reaches this manager despite
    /// naming a different instance (a `replication::router` bug, or a
    /// direct test call) is refused instead of answered from the wrong
    /// registry.
    pub instance_id: String,
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
    /// See `ReplicationManagerConfig::instance_id`'s doc.
    instance_id: String,
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
    /// Source of `PartitionEntry::leadership` values; monotonic, so a
    /// removed-and-recreated entry never reuses one.
    leadership_seq: AtomicU64,
}

fn reject_ack(environment: NodeEnvironment, reject: ReplReject) -> ReplHelloAck {
    ReplHelloAck {
        accepted: false,
        follower_leo: 0,
        follower_hw: 0,
        follower_epoch: 0,
        environment,
        reject: Some(reject),
        follower_log_epoch: None,
        follower_committed: None,
    }
}

impl ReplicationManager {
    pub fn new(config: ReplicationManagerConfig) -> Arc<Self> {
        Arc::new(Self {
            instance_id: config.instance_id,
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
            leadership_seq: AtomicU64::new(0),
        })
    }

    fn next_leadership(&self) -> u64 {
        self.leadership_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn local_node_id(&self) -> &str {
        &self.local_node_id
    }

    /// Which TentaBus instance this manager serves — `replication::router`'s
    /// `MANAGERS` key and `replication::stop`'s `router::unregister` both
    /// need it back out of an `Arc<ReplicationManager>` alone.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
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

    /// Reads the first frame of a newly accepted bi-stream and routes it:
    /// a `ReplHello` goes to the matching partition's `FollowerRunner`
    /// (rejecting with the specific `ReplReject` reason otherwise), and a
    /// `LeoQuery` — the K-M2-3 exception to the Hello-first rule, the
    /// CANDIDATE dialing this node directly for its pre-vote — is answered
    /// from this node's own replication state. Anything else is dropped
    /// silently.
    ///
    /// plan-app-platform §1.6/W5: this is no longer how a REAL accepted
    /// mesh connection reaches a manager — the mesh's single `ALPN_BUS`
    /// accept slot is installed once by `replication::router::register`,
    /// which reads the first frame itself (to learn WHICH instance's
    /// manager to hand the rest of the stream to, since N instances now
    /// share that one slot) and calls `accept_hello`/`answer_leo_query`
    /// directly. `accept_stream` stays exactly as it was for the
    /// single-manager in-memory-duplex tests that call it — a manager that
    /// already knows which instance it is needs no demux step of its own.
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
    pub async fn accept_stream(&self, remote_hex: String, mut recv: BusRecv, send: BusSend) {
        let first = frames::read_frame(&mut recv).await;
        match first {
            Ok(ReplFrame::Hello(hello)) => {
                self.accept_hello(&remote_hex, hello, recv, send).await;
            }
            Ok(ReplFrame::LeoQuery(query)) => {
                self.answer_leo_query(query, send).await;
            }
            _ => return,
        }
    }

    /// Answers one inbound `LeoQuery` (K-M2-3) from the registry entry for
    /// the queried partition: a `Leader` reports its own engine offsets
    /// (it is by definition caught up with itself) and, while its handle
    /// serves and still hears its quorum (`LeaderHandle::
    /// holds_quorum_lease`), `leading: true`; a
    /// `Follower` reports its runner's state — or, with no runner attached,
    /// its local log's (`local_log_position`), never zeros: a zero would
    /// hide a replica that is ahead from the candidate's truncation.
    /// `log_epoch` is the epoch that log was last written under, which the
    /// candidate ranks logs by (`election::LogPosition::rank`). `in_isr` is
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
    ///
    /// `pub(crate)`: `replication::router` calls this directly for a
    /// `LeoQuery` it has already matched to this manager's own
    /// `instance_id` (§1.6's demux) — see this method's own instance check
    /// below for why that match is re-verified here too, not just trusted.
    pub(crate) async fn answer_leo_query(&self, query: ReplLeoQuery, mut send: BusSend) {
        // plan-app-platform §1.6 belt-and-suspenders: `replication::router`
        // already matched `query.instance_id` to route the frame here, but
        // `accept_stream`'s own single-manager tests call this without
        // going through the router at all — and either way, `ReplLeoReply`
        // has no `reject` field to name a mismatch on, so a wrong instance
        // is folded into the SAME "unknown partition" zero-reply an absent
        // registry entry already produces, never evaluated against THIS
        // manager's registry.
        let key: PartitionKey = (query.org_id, query.topic, query.partition);
        let zeros = ReplLeoReply {
            leo: 0,
            hw: 0,
            leader_epoch: 0,
            in_isr: false,
            log_epoch: None,
            leading: false,
            ineligible: true,
            committed: None,
            leader_alive: false,
        };
        // Read from the local log outside the registry guard: that opens
        // the partition, which may touch the disk.
        let mut read_local = None;
        // A manager that has shut down serves nothing: it answers the way the
        // router answers for an instance no longer registered, so its log —
        // no longer fed, no longer leading — is not weighed as a candidate's.
        let mut reply = if query.instance_id != self.instance_id || self.shutdown.is_cancelled() {
            zeros
        } else {
            match self.registry.get(&key) {
                None => zeros,
                Some(entry) => {
                    let epoch = entry.assignment.leader_epoch;
                    let in_isr = entry
                        .assignment
                        .isr
                        .iter()
                        .any(|m| m == &self.local_node_id);
                    let base = ReplLeoReply {
                        leader_epoch: epoch,
                        in_isr,
                        // A leader never runs an election, and a log that
                        // awaits reconciliation must not win one.
                        ineligible: entry.role == LocalRole::Leader || entry.unreconciled,
                        ..zeros
                    };
                    match entry.role {
                        LocalRole::Leader => match entry.leader.as_ref() {
                            Some(l) => ReplLeoReply {
                                leo: l.log_end_offset(),
                                hw: l.high_watermark(),
                                in_isr: true,
                                log_epoch: Some(l.log_epoch()),
                                committed: Some(l.committed_offset()),
                                leading: l.holds_quorum_lease(),
                                ..base
                            },
                            // Mid-rebuild, its handle not attached yet: the
                            // log is still this node's, but nothing is being
                            // served, so it does not claim to lead.
                            None => {
                                read_local = Some(entry.assignment.clone());
                                ReplLeoReply {
                                    in_isr: true,
                                    ..base
                                }
                            }
                        },
                        _ => match entry.follower.as_ref() {
                            Some(f) => ReplLeoReply {
                                leo: f.leo(),
                                hw: f.hw(),
                                log_epoch: Some(f.log_epoch()),
                                committed: Some(f.committed()),
                                leader_alive: !f.lease_expired(),
                                ..base
                            },
                            None => {
                                read_local = Some(entry.assignment.clone());
                                base
                            }
                        },
                    }
                }
            }
        };
        if let Some(assignment) = read_local {
            if let Ok(local) = self.local_log_position(&assignment).await {
                reply.leo = local.leo;
                // A peer that predates `committed` reads `hw` in its place.
                reply.hw = local.committed;
                reply.committed = Some(local.committed);
                reply.log_epoch = Some(local.epoch);
            }
        }
        let _ = frames::write_frame(&mut send, &ReplFrame::LeoReply(reply)).await;
    }

    /// This node's own log of `assignment`'s partition, read from the local
    /// partition on the blocking pool: opening a partition that is not open
    /// yet reads (and for a dropped incarnation removes) files, which must
    /// not stall a runtime worker — least of all inside the candidate's
    /// `LeoQuery` budget.
    async fn local_log_position(
        &self,
        assignment: &PartitionAssignment,
    ) -> Result<LogPosition, ReplError> {
        let factory = Arc::clone(&self.follower_factory);
        let assignment = assignment.clone();
        tokio::task::spawn_blocking(move || factory.local_log_position(&assignment))
            .await
            .map_err(|e| ReplError::Internal(format!("local log read task failed: {e}")))?
    }

    /// The `Hello`-first half of `accept_stream` (the original body,
    /// extracted so the `LeoQuery` exception can share the entry point).
    ///
    /// The registry lookup is preceded by `await_local_assignment`, so a
    /// `TopicUnknown` here means "the ledger agrees I am not a replica of
    /// this partition (or never will within `ASSIGNMENT_AWAIT`)", not "my
    /// own startup is behind".
    ///
    /// `pub(crate)`: `replication::router` calls this directly for a
    /// `Hello` it has already matched to this manager's own `instance_id`
    /// (§1.6's demux). The instance check just below is re-verified here
    /// anyway — the same "nothing may ever mix" defense in depth
    /// `BusCallContext`/`check_instance` applies at the engine layer (§1.7)
    /// — so a `Hello` that reaches this manager WITHOUT having gone through
    /// the router (a direct test call, or a future bug) is still refused
    /// rather than answered from a registry that is not its own.
    ///
    /// `remote_node_id` is the dialing peer's authenticated mesh identity
    /// (the iroh endpoint id, which IS the node id in this mesh — see
    /// `IrohMeshManager::node_id`). A leader always dials its followers
    /// itself, so a `Hello` naming any other leader is refused before it can
    /// fence, adopt or be followed.
    pub(crate) async fn accept_hello(
        &self,
        remote_node_id: &str,
        hello: ReplHello,
        recv: BusRecv,
        mut send: BusSend,
    ) {
        if hello.instance_id != self.instance_id {
            // W5 review finding D4: this arm is only reachable when
            // `replication::router` already matched `hello.instance_id` to
            // route the frame HERE (so this branch means the router itself
            // has a bug) or when a caller drives `accept_hello` directly
            // without going through the router at all (a direct test call,
            // or a future bug) — either way it is the one mechanism
            // enforcing "two instances must never see each other's data"
            // firing on its own manager, and previously left no trace.
            tracing::warn!(
                hello_instance_id = %hello.instance_id,
                manager_instance_id = %self.instance_id,
                leader_node_id = %hello.leader_node_id,
                org_id = %hello.org_id, topic = %hello.topic, partition = hello.partition,
                "replication: accept_hello refused a Hello naming a different instance \
                 than this manager's own — replying UnknownInstance"
            );
            let ack = reject_ack(self.local_env, ReplReject::UnknownInstance);
            let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
            return;
        }
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
        if hello.leader_node_id != remote_node_id {
            tracing::warn!(
                peer = %remote_node_id,
                claimed_leader = %hello.leader_node_id,
                org_id = %hello.org_id, topic = %hello.topic, partition = hello.partition,
                "replication: refused a Hello whose leader is not the dialing peer"
            );
            let ack = reject_ack(self.local_env, ReplReject::LeaderIdentityMismatch);
            let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
            return;
        }
        let key: PartitionKey = (hello.org_id.clone(), hello.topic.clone(), hello.partition);

        // A registry miss is not yet an answer: resolve it against the
        // ledger's own row first, so a leader that dialed this node before
        // its materialization poll caught up is held, not bounced.
        self.await_local_assignment(&key).await;

        // A claim far past this node's ledger row was admitted by nobody;
        // fencing to it or adopting it would pin the entry above every real
        // assignment (`MAX_HELLO_EPOCH_LEAD`). The ledger row, not the
        // registry, is the reference: the registry already carries earlier
        // adoptions and would let a chain of Hellos ratchet the cap upward.
        let ledger_row = match self
            .assignments
            .get(&self.instance_id, &key.0, &key.1, key.2)
        {
            Ok(Some(row)) => Some(row),
            _ => self.registry.get(&key).map(|e| e.assignment.clone()),
        };
        // A leader of another incarnation of this topic is not a claim on
        // this partition at all, whatever its epoch: epochs restart at 1 on
        // re-creation, so a leader still serving the deleted incarnation at
        // epoch 3 would otherwise outrank the new one's epoch 1 below, fence
        // or pin this node to a dead claim, and append the old incarnation's
        // records into the new log. Judged here against the ledger row, and
        // again under the guard against the entry (`incarnation_matches`).
        if let Some(row) = &ledger_row {
            if !incarnation_matches(&hello, row) {
                let ack = reject_ack(
                    self.local_env,
                    ReplReject::TopicIncarnationMismatch {
                        theirs: hello.topic_generation.unwrap_or_default(),
                        ours: row.topic_generation,
                    },
                );
                let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
                return;
            }
        }
        let ledger_epoch = ledger_row.map(|row| row.leader_epoch);
        if let Some(ledger_epoch) = ledger_epoch {
            if hello.leader_epoch > ledger_epoch.saturating_add(MAX_HELLO_EPOCH_LEAD) {
                tracing::warn!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    peer = %remote_node_id,
                    hello_epoch = hello.leader_epoch,
                    ledger_epoch,
                    "replication: refused a Hello too far ahead of this node's ledger"
                );
                let ack = reject_ack(
                    self.local_env,
                    ReplReject::EpochAheadOfLedger { ledger_epoch },
                );
                let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
                return;
            }
        }

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
        let self_assigned = hello.replicas.iter().any(|r| r == &self.local_node_id);
        let mut fenced = None;
        if let Some(mut entry) = self.registry.get_mut(&key) {
            // Judged under the write guard it acts on, so a concurrent
            // re-stamp cannot slip between the check and the fence.
            if entry.role == LocalRole::Leader
                && self_assigned
                && incarnation_matches(&hello, &entry.assignment)
                && claim_outranks(hello.leader_epoch, &hello.leader_node_id, &entry.assignment)
            {
                // Stop serving first (`stop` aborts the feeder tasks — no
                // further bytes leave this node under the old claim), then
                // adopt the peer's view so the accept below sees a
                // `Follower` entry at the peer's epoch.
                fenced = entry.leader.take();
                entry.assignment = PartitionAssignment {
                    leader_node_id: hello.leader_node_id.clone(),
                    leader_epoch: hello.leader_epoch,
                    // The Hello carries no ISR of its own; the full replica
                    // set is the honest upper bound until this node's own
                    // poll materializes the ledger row (the live ISR the
                    // leader tracks needs no static answer from here).
                    isr: hello.replicas.clone(),
                    replicas: hello.replicas.clone(),
                    updated_at_ms: now_ms(),
                    ..entry.assignment.clone()
                };
                entry.role = LocalRole::Follower;
                entry.leadership = self.next_leadership();
                entry.follower = None;
                entry.unfollowed_since = Instant::now();
                // A promotion in flight on this entry is moot: the
                // deterministic winner just dialed us.
                entry.promotion = PromotionState::Idle;
            }
        }
        if let Some(leader) = fenced {
            tracing::warn!(
                org_id = %key.0, topic = %key.1, partition = key.2,
                peer = %hello.leader_node_id,
                peer_epoch = hello.leader_epoch,
                "replication: fencing own leadership on a newer peer Hello \
                 (equal epochs resolve to the lower node id)"
            );
            // Outside the guard: `stop()` flushes partition meta.
            leader.stop();
            self.assignments_changed.send_replace(());
        }

        // Judged and adopted under ONE write guard. They used to be two
        // guards — a read for the verdict, then a write to adopt the Hello's
        // term — and a promotion of this node landing in between flipped the
        // entry to `Leader` at the same epoch: the second guard saw a claim
        // that outranks it, adopted the peer's assignment, attached the
        // follower runner and left `role = Leader` with this node's own
        // leader handle still dialing. Measured in the chaos run: both
        // survivors `ROLE Leader { epoch: 3 }` for the whole 20 s settle
        // window, one of them following the other's stream while its own
        // supervisor collected `NotAReplica` every backoff. With one guard
        // a promotion lands either before (the entry is `Leader`: refused
        // below, and the peer's next dial fences it above) or after (the
        // promotion sees a followed claim that outranks it and yields).
        // No guard is held across the `.await`s that follow.
        enum Verdict {
            Accept {
                assignment: PartitionAssignment,
                replaced_runner: Option<Box<dyn FollowerRunner>>,
            },
            Reject(ReplReject),
        }
        let verdict = match self.registry.get_mut(&key) {
            None => Verdict::Reject(ReplReject::TopicUnknown),
            Some(entry) if entry.cutting => Verdict::Reject(ReplReject::Reconciling),
            Some(entry) if entry.role != LocalRole::Follower => {
                Verdict::Reject(ReplReject::NotAReplica)
            }
            Some(entry) if !incarnation_matches(&hello, &entry.assignment) => {
                Verdict::Reject(ReplReject::TopicIncarnationMismatch {
                    theirs: hello.topic_generation.unwrap_or_default(),
                    ours: entry.assignment.topic_generation,
                })
            }
            // The same order the ledger and the fence above use: an older
            // epoch, or an equal epoch led by a node ranked above the one
            // this entry follows, is not a claim this node may follow.
            Some(entry)
                if claim_superseded(
                    hello.leader_epoch,
                    &hello.leader_node_id,
                    &entry.assignment,
                ) =>
            {
                Verdict::Reject(ReplReject::StaleEpoch {
                    have: entry.assignment.leader_epoch,
                })
            }
            Some(mut entry) => {
                // An accepted Hello from a term this entry does not know yet
                // — the ledger row for it has not reached this node — is the
                // term this node now actually follows, so the registry
                // adopts it the way the leader-side fence above does. Left at
                // the older row, `role()` named a leader this node no longer
                // followed, and the newer row's later arrival looked like a
                // leader change: a rebuild that stopped this very stream in
                // the middle of the leader's writes. Adopted, that row is a
                // metadata update.
                if claim_outranks(hello.leader_epoch, &hello.leader_node_id, &entry.assignment) {
                    entry.assignment = PartitionAssignment {
                        leader_node_id: hello.leader_node_id.clone(),
                        leader_epoch: hello.leader_epoch,
                        isr: hello.replicas.clone(),
                        replicas: hello.replicas.clone(),
                        updated_at_ms: now_ms(),
                        ..entry.assignment.clone()
                    };
                }
                entry.unfollowed_since = Instant::now();
                // A stepped-down tail is now this leader's to reconcile: its
                // handshake judges the log by the epochs its records were
                // written in, and no step-down cut may start under the
                // stream it opens (`cut_stepped_down_log`).
                entry.unreconciled = false;
                Verdict::Accept {
                    assignment: entry.assignment.clone(),
                    replaced_runner: entry.follower.take(),
                }
            }
        };

        let (assignment, replaced_runner) = match verdict {
            Verdict::Reject(reject) => {
                let ack = reject_ack(self.local_env, reject);
                let _ = frames::write_frame(&mut send, &ReplFrame::HelloAck(ack)).await;
                return;
            }
            Verdict::Accept {
                assignment,
                replaced_runner,
            } => (assignment, replaced_runner),
        };
        if let Some(old) = replaced_runner {
            old.stop();
        }
        match self.follower_factory.spawn(&assignment, hello, recv, send) {
            Ok(runner) => {
                // Attached only to the entry the verdict accepted into: a
                // promotion or rebuild that moved it on while the runner was
                // spawned must not end up with a follower stream riding a
                // `Leader` entry (the same stuck state the single guard
                // above closes).
                let mut stale_runner = Some(runner);
                if let Some(mut entry) = self.registry.get_mut(&key) {
                    if entry.role == LocalRole::Follower
                        && entry.assignment.leader_node_id == assignment.leader_node_id
                        && entry.assignment.leader_epoch == assignment.leader_epoch
                        && entry.assignment.topic_generation == assignment.topic_generation
                    {
                        entry.follower = stale_runner.take();
                    }
                }
                if let Some(runner) = stale_runner {
                    runner.stop();
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
            match self
                .assignments
                .get(&self.instance_id, &key.0, &key.1, key.2)
            {
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
    ///
    /// W5 review finding D2: `assignment.instance_id` is checked against
    /// `self.instance_id` before anything else below — this is the ONLY
    /// function that inserts into `registry`, keyed purely on
    /// `(org_id, topic, partition)` with no instance component of its own
    /// (`PartitionKey`'s own doc, above). A caller that hands this manager
    /// a row it did not filter by instance (a W6 boot pass over every
    /// enabled instance's assignments, a future reconcile path, or a bug
    /// symmetric to amendment 9e) would otherwise spawn a `LeaderHandle`
    /// for ANOTHER instance's partition, start feeding it, and then reject
    /// that other instance's real leader's `Hello` with `UnknownInstance`
    /// forever — silent wrong-instance leadership plus a permanently
    /// fenced real one. `PartitionKey`'s doc promises this manager's
    /// registry never crosses instances; this is where that promise is
    /// actually enforced for the assignment-write path (the frame-receive
    /// path is enforced by `accept_hello`/`answer_leo_query` instead).
    pub async fn apply_assignment(&self, assignment: PartitionAssignment) {
        if assignment.instance_id != self.instance_id {
            tracing::warn!(
                assignment_instance_id = %assignment.instance_id,
                manager_instance_id = %self.instance_id,
                org_id = %assignment.org_id, topic = %assignment.topic, partition = assignment.partition,
                "replication: apply_assignment refused a row for a different instance \
                 (caller bug — every assignment reaching this manager must already be \
                 filtered by instance_id)"
            );
            return;
        }
        let key: PartitionKey = (
            assignment.org_id.clone(),
            assignment.topic.clone(),
            assignment.partition,
        );
        // Epochs only grow (K-M2-1), and the registry can be AHEAD of the
        // ledger row this call carries: a winning peer's `Hello` fences this
        // node straight to the peer's term (`accept_hello`) and a refused
        // `Hello` raises it on step-down, both before the ledger delivers
        // that term's row. Applying the older row afterwards — typically
        // this node's own create-time placement — re-promoted a follower to
        // a superseded term and tore down the live leader's stream mid-write
        // until its own stale `Hello` was refused again. Checked here for the
        // log line and again under each write guard below, since an adoption
        // or fence can land in between.
        let current_generation = self
            .registry
            .get(&key)
            .map(|e| e.assignment.topic_generation);
        if current_generation.is_some_and(|g| assignment.topic_generation > g) {
            // A new incarnation of the topic: everything this entry holds —
            // handles, epochs, the leadership stint — belongs to the deleted
            // one. Dropped outright, so the new incarnation starts clean even
            // when its delete and re-create both landed between two polls.
            self.forget_partition(&key);
        }
        if let Some(current) = self.registry.get(&key) {
            if assignment_superseded(&assignment, &current.assignment) {
                // Rate-bounded by the callers: the poll loop applies each
                // distinct row once (its fingerprint cache), and the other
                // callers act on a row once per event.
                tracing::warn!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    current_leader = %current.assignment.leader_node_id,
                    current_epoch = current.assignment.leader_epoch,
                    stale_leader = %assignment.leader_node_id,
                    stale_epoch = assignment.leader_epoch,
                    "replication: ignoring an assignment older than the term this node already follows"
                );
                return;
            }
        }
        let is_replica = assignment.replicas.iter().any(|r| r == &self.local_node_id);
        let abdicated = self
            .registry
            .get(&key)
            .and_then(|e| e.abdicated_epoch)
            .is_some_and(|epoch| assignment.leader_epoch <= epoch);
        let new_role = if !is_replica {
            LocalRole::NotReplica
        } else if assignment.leader_node_id == self.local_node_id && !abdicated {
            LocalRole::Leader
        } else {
            LocalRole::Follower
        };

        // Three-way, because "the row changed" and "this node's replication
        // topology changed" are not the same question. `PartitionAssignment`
        // derives `PartialEq` over `updated_at_ms` and `isr`, so ANY row
        // rewrite compared unequal here and tore the leader down — aborting
        // every replica supervisor, which each follower then saw as a dead
        // stream and therefore an expired lease, manufacturing election
        // candidates out of a leader that never went anywhere.
        //
        // A rewrite that preserves the role, the leader, the replica set AND
        // the epoch cannot change any of that, so it is a metadata update.
        // Deliberately narrow: an epoch change still rebuilds, because a new
        // epoch has to reach the followers through a fresh `Hello`.
        enum Reconcile {
            Nothing,
            MetadataOnly,
            Rebuild,
        }
        let mut previous: Option<(LocalRole, String, u32)> = None;
        let plan = match self.registry.get(&key) {
            None => Reconcile::Rebuild,
            Some(e) => {
                previous = Some((
                    e.role,
                    e.assignment.leader_node_id.clone(),
                    e.assignment.leader_epoch,
                ));
                if e.role == new_role && e.assignment == assignment {
                    Reconcile::Nothing
                } else if e.role == new_role
                    && e.assignment.leader_node_id == assignment.leader_node_id
                    && e.assignment.replicas == assignment.replicas
                    && e.assignment.leader_epoch == assignment.leader_epoch
                {
                    Reconcile::MetadataOnly
                } else {
                    Reconcile::Rebuild
                }
            }
        };
        match plan {
            Reconcile::Nothing => return,
            Reconcile::MetadataOnly => {
                // Two statements, not one: the read guard above is already
                // dropped, and writing it as a single expression would let a
                // later edit hold both guards on the same shard and deadlock.
                if let Some(mut e) = self.registry.get_mut(&key) {
                    if !assignment_superseded(&assignment, &e.assignment) {
                        e.assignment = assignment;
                    }
                }
                self.assignments_changed.send_replace(());
                return;
            }
            Reconcile::Rebuild => {
                // A rebuild stops the current leader handle / follower
                // runner, which ends every replication stream of this
                // partition; without this line a stream teardown seen on a
                // peer has no visible cause here.
                tracing::info!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    previous = ?previous,
                    new_role = ?new_role,
                    new_leader = %assignment.leader_node_id,
                    new_epoch = assignment.leader_epoch,
                    "replication: rebuilding partition role"
                );
            }
        }

        // Re-stamped IN PLACE, handles taken but the entry never removed.
        // `answer_leo_query` reads this same map, and an absent entry is
        // indistinguishable there from "not a replica of this partition": it
        // answers `(leo 0, hw 0, epoch 0, in_isr false)`. Removing the entry
        // therefore opened a window — the old handle's `stop()` alone flushes
        // partition meta, measured at 124-302 ms — in which this node denied
        // its own leadership to any candidate mid-election. The candidate saw
        // the incumbent at leo 0 and epoch 0, could not tell a stale
        // assignment view from a dead leader, and promoted itself over a
        // leader that had never gone anywhere. Carrying the NEW assignment
        // through the rebuild is what makes `run_election`'s
        // stale-view deferral (`election_deferral`) able to fire at all.
        let mut old_leader = None;
        let mut old_follower = None;
        let mut restamped = false;
        if let Some(mut e) = self.registry.get_mut(&key) {
            // The check above ran under a read guard that is gone now.
            if assignment_superseded(&assignment, &e.assignment) {
                return;
            }
            old_leader = e.leader.take();
            old_follower = e.follower.take();
            e.unfollowed_since = Instant::now();
            e.assignment = assignment.clone();
            if new_role != LocalRole::Leader {
                e.leadership = self.next_leadership();
            }
            e.role = new_role;
            e.promotion = PromotionState::Idle;
            restamped = true;
        }
        // Outside the `if let`: `stop()` blocks on the engine's writer thread,
        // and holding a `DashMap` write guard across it would stall every
        // other shard user — `answer_leo_query` above included, which is
        // exactly the reader this rewrite exists to keep answering.
        if let Some(leader) = old_leader {
            leader.stop();
        }
        if let Some(follower) = old_follower {
            follower.stop();
        }

        match new_role {
            // The one case that really does leave the registry: this node is
            // no longer a replica, so answering a `LeoQuery` with zeros is the
            // truthful answer rather than a self-denial.
            LocalRole::NotReplica => {
                self.registry.remove(&key);
            }
            LocalRole::Leader => {
                // No dials here (see the NON-NEGOTIABLE note above): the
                // glue spawns one supervisor per replica, each dialing in
                // its own task. `spawn_deferred` keeps the engine's
                // writer-thread epoch stamp off this call path too.
                match self.leader_factory.spawn_deferred(&assignment, Vec::new()) {
                    Ok(handle) => match self.attach_leader(&key, &assignment, handle) {
                        Ok(serving) => serving.claim_term(),
                        // Either a handle another task attached while this
                        // one was blocked, or — when the entry has since
                        // moved on — the handle just spawned here. Stopping
                        // it is not optional: neither `GlueLeaderHandle` nor
                        // `GlueFollowerRunner` implements `Drop`, so a
                        // handle that is merely dropped leaves its feeder
                        // tasks running and still writing to the partition.
                        Err(orphan) => orphan.stop(),
                    },
                    Err(e) => {
                        // The re-stamp above may have left this key holding a
                        // Leader entry with no handle. Drop it — but only if it
                        // is still the entry this call stamped: `preflight`
                        // refuses every publish through a handle-less leader
                        // anyway, and leaving it would make the next assignment
                        // poll compare EQUAL (`Reconcile::Nothing`) and never
                        // retry the spawn.
                        self.remove_if_still_ours(&key, &assignment, new_role);
                        tracing::warn!(error = %e, "replication: leader handle spawn failed");
                    }
                }
            }
            LocalRole::Follower => {
                // Nothing to spawn: the entry only has to EXIST so
                // `accept_stream` has somewhere to attach the `FollowerRunner`
                // once the leader dials in, and the in-place re-stamp above
                // already put it there with the new role and assignment. Only
                // a key that had no entry at all still needs one; another
                // call may have installed one meanwhile
                // (`install_follower_entry`).
                if !restamped {
                    self.install_follower_entry(key, assignment);
                }
            }
        }
        // Reached only when something actually changed — the `unchanged`
        // path above returns first. Wakes anything parked in
        // `await_local_assignment` without waiting for its own re-read tick.
        self.assignments_changed.send_replace(());
    }

    /// The `apply_assignment` path for a key it found no entry for. It may
    /// run concurrently with another call for the same key (the assignment
    /// poll, a step-down, a promotion), so an entry installed since is
    /// judged like any existing entry, not overwritten: overwriting a leader
    /// entry dropped its handle without `stop()`, leaving its feeders
    /// running and its leader writes open.
    fn install_follower_entry(&self, key: PartitionKey, assignment: PartitionAssignment) {
        let displaced = match self.registry.entry(key) {
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(PartitionEntry {
                    assignment,
                    role: LocalRole::Follower,
                    leader: None,
                    follower: None,
                    unfollowed_since: Instant::now(),
                    unreconciled: false,
                    cutting: false,
                    log_read_warned: false,
                    quorum_lost_since: None,
                    abdicated_epoch: None,
                    promotion: PromotionState::Idle,
                    leadership: self.next_leadership(),
                });
                None
            }
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let e = occupied.get_mut();
                if assignment_superseded(&assignment, &e.assignment) {
                    None
                } else if e.role == LocalRole::Follower
                    && same_follower_term(&assignment, &e.assignment)
                {
                    // Already following this exact term, possibly with a
                    // runner attached since: stopping it would only force a
                    // redial and drop this replica from the ISR.
                    e.assignment = assignment;
                    None
                } else {
                    let leader = e.leader.take();
                    let follower = e.follower.take();
                    if e.role != LocalRole::Follower {
                        e.leadership = self.next_leadership();
                    }
                    e.assignment = assignment;
                    e.role = LocalRole::Follower;
                    e.unfollowed_since = Instant::now();
                    e.promotion = PromotionState::Idle;
                    Some((leader, follower))
                }
            }
        };
        if let Some((leader, follower)) = displaced {
            if let Some(leader) = leader {
                leader.stop();
            }
            if let Some(follower) = follower {
                follower.stop();
            }
        }
    }

    /// The handle that replaced `wait.handle` as this node's leader for
    /// `key` once `apply_assignment` has attached it, with the `required`
    /// count recomputed from the successor's assignment — or `None` when no
    /// successor may finish a publish begun under the stopped handle, or
    /// `deadline` passes first.
    ///
    /// A rebuild that keeps this node as leader (an epoch bump on the same
    /// node, a replica-set change) stops the old handle, which ends every
    /// quorum wait on it at once. While this node has led the partition
    /// without a break, the record such a publish appended is still at its
    /// offset in this node's log and the successor replicates that log, so
    /// its acks answer the same question. Every condition below guards that
    /// premise:
    /// - `leadership` unchanged: the role never left `Leader`. A fence or
    ///   step-down in between hands the log to another node's authority,
    ///   whose handshake may truncate this node below the waited offset —
    ///   even if this node leads again later, possibly at the very epoch it
    ///   was fenced to.
    /// - no truncation of the local log since the wait began, and the
    ///   successor's log still reaches `next_offset`: the offset names the
    ///   same record.
    /// - at most one term later: a larger jump means terms passed that this
    ///   node did not lead.
    fn successor_leader(
        &self,
        key: &PartitionKey,
        wait: &AckWait,
        next_offset: u64,
        deadline: Instant,
    ) -> Option<AckWait> {
        loop {
            {
                let entry = self.registry.get(key)?;
                let epoch = entry.assignment.leader_epoch;
                if entry.role != LocalRole::Leader
                    || entry.leadership != wait.leadership
                    || entry.assignment.leader_node_id != self.local_node_id
                    || epoch > wait.epoch.saturating_add(1)
                {
                    return None;
                }
                // `None` here is the rebuild window: the entry is already
                // re-stamped, the new handle not yet attached.
                if let Some(handle) = entry.leader.as_ref() {
                    if !std::ptr::addr_eq(Arc::as_ptr(handle), Arc::as_ptr(&wait.handle)) {
                        if handle.truncation_count() != wait.truncations
                            || handle.log_end_offset() < next_offset
                        {
                            return None;
                        }
                        return Some(AckWait {
                            handle: Arc::clone(handle),
                            epoch,
                            leadership: entry.leadership,
                            truncations: wait.truncations,
                            replicas: entry.assignment.replicas.len(),
                        });
                    }
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(SUCCESSOR_POLL_INTERVAL);
        }
    }

    /// Drops this node's replication state for `key` — stops the leader
    /// handle and follower runner and removes the entry — once the ledger no
    /// longer lists this node as a replica of it: the topic was deleted
    /// (`reassign(.., &[])`, then the rows themselves), or this node was
    /// moved off the replica set. Without it the entry outlived the topic,
    /// its handles kept running against a dead incarnation, and a recreated
    /// topic's epoch-1 placement was refused as older than the dead
    /// incarnation's last term.
    pub(crate) fn forget_partition(&self, key: &PartitionKey) {
        if let Some((_, mut entry)) = self.registry.remove(key) {
            tracing::info!(
                org_id = %key.0, topic = %key.1, partition = key.2,
                epoch = entry.assignment.leader_epoch,
                role = ?entry.role,
                "replication: dropping a partition this node no longer replicates"
            );
            if let Some(leader) = entry.leader.take() {
                leader.stop();
            }
            if let Some(follower) = entry.follower.take() {
                follower.stop();
            }
            self.assignments_changed.send_replace(());
        }
    }

    /// Puts the leader handle `apply_assignment` just spawned into the
    /// registry entry that same call re-stamped. `Ok` is the handle now
    /// serving, whose term the caller claims once the guard is gone; `Err`
    /// is the handle that must be stopped instead of kept.
    ///
    /// A whole-value `insert` cannot be used here. The re-stamp deliberately
    /// leaves the entry present and writable for the 124-302 ms the old
    /// handle's `stop()` blocks, so `accept_hello` can attach a follower runner
    /// (or fence this node down to `Follower` on a winning peer's `Hello`) in
    /// the meantime. Overwriting the entry with values captured before that
    /// window would drop those handles without stopping them: nothing in
    /// `bus::replication` implements `Drop`, so both `GlueLeaderHandle` and
    /// `GlueFollowerRunner` abort their tasks ONLY inside `stop()` — a dropped
    /// runner keeps consuming its stream and appending to the same partition,
    /// invisible to a registry that believes it is gone.
    ///
    /// The identity check is role plus leader, replica set and epoch rather
    /// than full assignment equality: a concurrent `Reconcile::MetadataOnly`
    /// may legitimately have refreshed `updated_at_ms`/`isr` underneath, and
    /// that does not make this handle stale. Anything more than that means
    /// another call has taken the key over with newer information, and the
    /// handle spawned here is the one to throw away.
    ///
    /// A handle another task already attached for the same term is kept and
    /// the one spawned here is thrown away instead (a promotion and the
    /// assignment poll both install the promotion's own row). Swapping would
    /// stop a leader whose replica streams are already up, and each follower
    /// would see its stream end and hand-shake again — an ISR dip right after
    /// a failover for no reason.
    fn attach_leader(
        &self,
        key: &PartitionKey,
        assignment: &PartitionAssignment,
        handle: Box<dyn LeaderHandle>,
    ) -> Result<Arc<dyn LeaderHandle>, Arc<dyn LeaderHandle>> {
        let handle: Arc<dyn LeaderHandle> = Arc::from(handle);
        match self.registry.get_mut(key) {
            Some(mut e) => {
                if e.role == LocalRole::Leader
                    && e.leader.is_none()
                    && e.assignment.leader_node_id == assignment.leader_node_id
                    && e.assignment.replicas == assignment.replicas
                    && e.assignment.leader_epoch == assignment.leader_epoch
                {
                    handle.open_writes();
                    e.leader = Some(Arc::clone(&handle));
                    Ok(handle)
                } else {
                    Err(handle)
                }
            }
            None => {
                // No entry: either this key never had one (a first assignment,
                // which ran no `stop()` and so raced nothing) or another call
                // removed it. Inserting is right in the first case and harmless
                // in the second — the next assignment poll reconciles it.
                self.registry.insert(
                    key.clone(),
                    PartitionEntry {
                        assignment: assignment.clone(),
                        role: LocalRole::Leader,
                        leader: Some({
                            handle.open_writes();
                            Arc::clone(&handle)
                        }),
                        follower: None,
                        unfollowed_since: Instant::now(),
                        unreconciled: false,
                        cutting: false,
                        log_read_warned: false,
                        quorum_lost_since: None,
                        abdicated_epoch: None,
                        promotion: PromotionState::Idle,
                        leadership: self.next_leadership(),
                    },
                );
                Ok(handle)
            }
        }
    }

    /// Removes `key` only while it still holds the entry the calling
    /// `apply_assignment` stamped, and only while nothing has attached to it
    /// since. Same reasoning as `attach_leader`: an unconditional `remove`
    /// would discard a follower runner another task attached during the
    /// blocking window, without stopping it.
    fn remove_if_still_ours(
        &self,
        key: &PartitionKey,
        assignment: &PartitionAssignment,
        role: LocalRole,
    ) {
        let ours = match self.registry.get(key) {
            Some(e) => {
                e.role == role
                    && e.leader.is_none()
                    && e.follower.is_none()
                    && e.assignment.leader_node_id == assignment.leader_node_id
                    && e.assignment.replicas == assignment.replicas
                    && e.assignment.leader_epoch == assignment.leader_epoch
            }
            None => false,
        };
        if ours {
            self.registry.remove(key);
        }
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

    /// Steps down any partition this node still LEADS after a peer proved
    /// the claim stale (`LeaderHandle::observed_stale_epoch`). Meant to be
    /// called on the same periodic tick as `check_leases`.
    ///
    /// WHY THIS EXISTS SEPARATELY FROM THE LEDGER. `apply_assignment`
    /// already demotes a leader whose materialized row names someone else,
    /// and that is the normal path. It cannot cover the case this method
    /// is for: a node that comes back after a crash reads its OWN stale row
    /// from disk, believes it still leads, and its ledger copy is exactly
    /// what is behind. Nothing local will ever tell it otherwise. The one
    /// authoritative fact it does receive is the refusal its own outbound
    /// `Hello` collects — `StaleEpoch { have }` from a peer already
    /// following a newer epoch. Epochs are minted through the ledger and
    /// only grow, so a strictly higher one is proof, not a hint.
    ///
    /// Until this ran, that proof was thrown away: the supervisor logged it
    /// at `debug`, slept on backoff and dialed again, while the node kept
    /// serving `publish` as leader — writes accepted at an epoch no replica
    /// will ever replicate. Measured in the three-process chaos scenario's
    /// phase 5: the restarted node answered `ROLE Leader { epoch: 2 }` for
    /// the entire 30 s rejoin window while both peers refused it.
    ///
    /// The step-down prefers the ledger's row when it already names the new
    /// leader (the clean path — role, handles and epoch all come from one
    /// place). When it does not, this node still stops leading: it keeps
    /// the replica set, adopts the proven epoch and becomes a `Follower`
    /// with no leader named yet, which is exactly the state `accept_hello`
    /// needs to accept the real leader's dial the moment it arrives.
    pub async fn check_stale_leadership(&self) {
        let due: Vec<(PartitionKey, u32)> = self
            .registry
            .iter()
            .filter_map(|entry| {
                if entry.role != LocalRole::Leader {
                    return None;
                }
                let have = entry.leader.as_ref()?.observed_stale_epoch()?;
                (have > entry.assignment.leader_epoch).then(|| (entry.key().clone(), have))
            })
            .collect();
        for (key, have) in due {
            // Either way below, this node leaves a term that is over, and its
            // log may end in a tail no majority ever held. Until that tail is
            // cut (`cut_stepped_down_log`, right after the step-down) it
            // neither stands nor may win anyone's comparison.
            let Some(assignment) = self.registry.get_mut(&key).map(|mut entry| {
                entry.unreconciled = true;
                entry.assignment.clone()
            }) else {
                continue;
            };
            // Before anything else: a publish this node admitted as leader
            // of the term now over may still be on its way to the partition
            // (`BusService::publish` admits every partition up front), and
            // must not land after the step-down stamped as that term.
            let factory = Arc::clone(&self.follower_factory);
            let fenced =
                tokio::task::spawn_blocking(move || factory.fence_to_epoch(&assignment, have))
                    .await
                    .map_err(|e| ReplError::Internal(format!("fence task failed: {e}")))
                    .and_then(|r| r);
            if let Err(e) = fenced {
                tracing::warn!(
                    org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                    "replication: could not fence the partition at the step-down epoch"
                );
            }
            if let Ok(Some(stored)) = self
                .assignments
                .get(&self.instance_id, &key.0, &key.1, key.2)
            {
                if stored.leader_node_id != self.local_node_id && stored.leader_epoch >= have {
                    tracing::warn!(
                        org_id = %key.0, topic = %key.1, partition = key.2,
                        peer_epoch = have, new_leader = %stored.leader_node_id,
                        "replication: stepping down — a peer refused this leader's Hello \
                         with a newer epoch and the ledger already names the new leader"
                    );
                    self.apply_assignment(stored).await;
                    self.cut_stepped_down_log(&key).await;
                    continue;
                }
            }
            let Some(mut entry) = self.registry.get_mut(&key) else {
                continue;
            };
            if entry.role != LocalRole::Leader || have <= entry.assignment.leader_epoch {
                continue; // Raced with a poll apply that already demoted us.
            }
            tracing::warn!(
                org_id = %key.0, topic = %key.1, partition = key.2,
                own_epoch = entry.assignment.leader_epoch, peer_epoch = have,
                "replication: stepping down — a peer refused this leader's Hello with a \
                 newer epoch; this node's own ledger copy does not name the new leader yet"
            );
            // Stopped below, outside the guard: `stop()` makes blocking
            // round trips to the partition's writer.
            let stepped_down = entry.leader.take();
            entry.assignment.leader_epoch = have;
            // Deliberately cleared rather than left pointing at this node:
            // "someone newer leads, and this node does not know who" is the
            // honest state, and `accept_hello` judges an inbound Hello on
            // role + epoch, never on this field.
            entry.assignment.leader_node_id = String::new();
            entry.role = LocalRole::Follower;
            entry.leadership = self.next_leadership();
            entry.follower = None;
            entry.unfollowed_since = Instant::now();
            entry.promotion = PromotionState::Idle;
            drop(entry);
            if let Some(leader) = stepped_down {
                leader.stop();
            }
            self.assignments_changed.send_replace(());
            self.cut_stepped_down_log(&key).await;
        }
    }

    /// Cuts a stepped-down leader's log back to its committed offset and
    /// only then clears `unreconciled`. Safe without anyone's authority:
    /// everything below the committed offset is on a majority, so what is
    /// left is a prefix of every later term's chain, and what goes was never
    /// acknowledged. Retried from `check_leases` until it succeeds; until
    /// then the entry neither stands nor may win anyone's comparison.
    async fn cut_stepped_down_log(&self, key: &PartitionKey) {
        let assignment = {
            let Some(mut entry) = self.registry.get_mut(key) else {
                return;
            };
            if !entry.unreconciled || entry.role != LocalRole::Follower || entry.cutting {
                return;
            }
            // Under the same guard `accept_hello` judges by: no stream can
            // attach between this check and the cut.
            entry.cutting = true;
            entry.assignment.clone()
        };
        let factory = Arc::clone(&self.follower_factory);
        let cut = tokio::task::spawn_blocking(move || factory.cut_to_committed(&assignment))
            .await
            .map_err(|e| ReplError::Internal(format!("log cut task failed: {e}")))
            .and_then(|r| r);
        if let Some(mut entry) = self.registry.get_mut(key) {
            entry.cutting = false;
        }
        match cut {
            Ok(log) => {
                if let Some(mut entry) = self.registry.get_mut(key) {
                    entry.unreconciled = false;
                    entry.log_read_warned = false;
                }
                tracing::info!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    leo = log.leo, log_epoch = log.epoch,
                    "replication: stepped-down leader cut its log back to the committed offset"
                );
            }
            Err(e) => {
                let first = self
                    .registry
                    .get_mut(key)
                    .map(|mut entry| !std::mem::replace(&mut entry.log_read_warned, true))
                    .unwrap_or(false);
                if first {
                    tracing::warn!(
                        org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                        "replication: could not cut a stepped-down leader's log; it will not \
                         stand until the cut succeeds"
                    );
                } else {
                    tracing::debug!(
                        org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                        "replication: stepped-down log cut still failing"
                    );
                }
            }
        }
    }

    /// Steps down any partition this node leads whose handle has not held
    /// its quorum lease (`LeaderHandle::holds_quorum_lease`) for longer than
    /// the leader lease. Such a leader no longer holds candidates off
    /// (`leading: false`), but as a leader it never stands either — and when
    /// its log is the best one, no follower may win over it
    /// (`AbandonReason::Outranked`). Stepping down makes it an ordinary,
    /// eligible follower: it stands at once and, if its log is still the
    /// best, is elected again at a newer epoch with a working quorum. Meant
    /// for the same periodic tick as `check_leases`.
    pub async fn check_quorum_leases(&self) {
        let lease = self.follower_factory.leader_lease();
        let now = Instant::now();
        let mut due = Vec::new();
        for mut entry in self.registry.iter_mut() {
            if entry.role != LocalRole::Leader {
                entry.quorum_lost_since = None;
                continue;
            }
            let Some(holds) = entry.leader.as_ref().map(|l| l.holds_quorum_lease()) else {
                continue;
            };
            if holds {
                entry.quorum_lost_since = None;
                continue;
            }
            let since = *entry.quorum_lost_since.get_or_insert(now);
            if now.saturating_duration_since(since) > lease {
                due.push(entry.key().clone());
            }
        }
        for key in due {
            let Some(mut entry) = self.registry.get_mut(&key) else {
                continue;
            };
            if entry.role != LocalRole::Leader {
                continue;
            }
            tracing::warn!(
                org_id = %key.0, topic = %key.1, partition = key.2,
                epoch = entry.assignment.leader_epoch,
                "replication: stepping down — no quorum of replicas has acknowledged \
                 within the leader lease"
            );
            let handle = entry.leader.take();
            entry.abdicated_epoch = Some(entry.assignment.leader_epoch);
            entry.quorum_lost_since = None;
            entry.role = LocalRole::Follower;
            entry.leadership = self.next_leadership();
            entry.follower = None;
            // Its own leadership is what just lapsed: no dial is coming to
            // wait for, so it may stand on the next tick.
            entry.unfollowed_since = now
                .checked_sub(self.follower_factory.undialed_lease())
                .unwrap_or(now);
            entry.promotion = PromotionState::Idle;
            drop(entry);
            if let Some(handle) = handle {
                handle.stop();
            }
            self.assignments_changed.send_replace(());
        }
    }

    /// Scans every `Follower` entry and starts an election for any whose
    /// lease has expired (per `FollowerRunner::lease_expired`) and who is
    /// still in the last known ISR (`election::should_start_election`).
    /// Meant to be called on a periodic tick by whatever owns this
    /// manager's lifecycle (wave 2, `tentaflow/src/main.rs`).
    pub async fn check_leases(&self) {
        let uncut: Vec<PartitionKey> = self
            .registry
            .iter()
            .filter(|e| e.unreconciled && e.role == LocalRole::Follower)
            .map(|e| e.key().clone())
            .collect();
        for key in uncut {
            self.cut_stepped_down_log(&key).await;
        }
        let due: Vec<PartitionKey> = self
            .registry
            .iter()
            .filter_map(|entry| {
                if entry.role != LocalRole::Follower || entry.unreconciled {
                    return None;
                }
                let lease_expired = match entry.follower.as_ref() {
                    Some(follower) => follower.lease_expired(),
                    // No leader has dialed this entry since it last lost its
                    // runner — typically the row of a winner that died before
                    // its first dial. Waiting for a stream that never comes
                    // would leave the partition leaderless for good.
                    None => {
                        entry.unfollowed_since.elapsed() >= self.follower_factory.undialed_lease()
                    }
                };
                let in_isr = entry
                    .assignment
                    .isr
                    .iter()
                    .any(|n| n == &self.local_node_id);
                // `Abandoned` is a finished attempt, not a running one: an
                // election that lost, found no majority or could not propose
                // must stand again on the next expired lease, or a partition
                // whose every candidate lost one round never gets a leader.
                // Only an attempt still in flight (querying, proposing,
                // awaiting its majority) keeps this node out.
                let idle = matches!(
                    entry.promotion,
                    PromotionState::Idle | PromotionState::Abandoned { .. }
                );
                if idle
                    && election::should_start_election(lease_expired, in_isr, LocalRole::Follower)
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
        let Some((assignment, runner_log)) = self.registry.get(&key).map(|e| {
            let log = e.follower.as_ref().map(|f| LogPosition {
                epoch: f.log_epoch(),
                leo: f.leo(),
                committed: f.committed(),
            });
            (e.assignment.clone(), log)
        }) else {
            return;
        };
        let own = match runner_log {
            Some(log) => log,
            None => match self.local_log_position(&assignment).await {
                Ok(log) => {
                    if let Some(mut entry) = self.registry.get_mut(&key) {
                        entry.log_read_warned = false;
                    }
                    log
                }
                Err(e) => {
                    // Retried on every lease tick (500 ms) for as long as the
                    // cause lasts: say it once, then keep it at debug.
                    let first = self
                        .registry
                        .get_mut(&key)
                        .map(|mut entry| !std::mem::replace(&mut entry.log_read_warned, true))
                        .unwrap_or(false);
                    if first {
                        tracing::warn!(
                            org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                            "replication: not standing for election — the local log of this \
                             partition could not be read"
                        );
                    } else {
                        tracing::debug!(
                            org_id = %key.0, topic = %key.1, partition = key.2, error = %e,
                            "replication: still not standing for election — local log unreadable"
                        );
                    }
                    return;
                }
            },
        };

        // Computed BEFORE stepping so the event's own `leo_query_deadline`
        // and this function's wait loop agree on exactly the same instant
        // — `election.rs` never invents a deadline of its own from a
        // hardcoded constant (see `PromotionEvent::LeaseExpired`'s doc).
        let leo_deadline = Instant::now() + self.leo_query_timeout;
        let event = PromotionEvent::LeaseExpired {
            instance_id: assignment.instance_id.clone(),
            org_id: key.0.clone(),
            topic: key.1.clone(),
            partition: key.2,
            topic_generation: assignment.topic_generation,
            self_id: self.local_node_id.clone(),
            current_epoch: assignment.leader_epoch,
            own,
            isr: assignment.isr.clone(),
            replicas: assignment.replicas.clone(),
            leo_query_deadline: leo_deadline,
        };
        let (mut state, actions) = PromotionState::Idle.step(event);
        self.set_promotion(&key, state.clone());
        let Some(PromotionAction::SendLeoQuery { to }) = actions.into_iter().next() else {
            return; // Abandoned{NotInIsr} — nothing to query.
        };

        // Every peer is asked at once, each within the same `leo_deadline`.
        // Asked one after another, a dead peer's dial — which has no timeout
        // of its own — burned the whole budget before the live ones were
        // asked; with a majority of logs now required
        // (`AbandonReason::TooFewReplies`), that turned a crashed leader
        // sitting first in `replicas` into an election nobody could win.
        let replies = futures::future::join_all(to.into_iter().map(|peer| {
            let key = &key;
            async move {
                let remaining = leo_deadline.saturating_duration_since(Instant::now());
                let reply = tokio::time::timeout(remaining, self.query_leo(key, &peer))
                    .await
                    .ok()
                    .flatten();
                (peer, reply)
            }
        }))
        .await;
        let mut deferral = None;
        for (peer, reply) in replies {
            let Some(reply) = reply else {
                continue;
            };
            if deferral.is_none() {
                deferral = election_deferral(&reply, assignment.leader_epoch)
                    .map(|reason| (peer.clone(), reason));
            }
            let (next, _) = state.step(PromotionEvent::LeoReply {
                node_id: peer,
                log: LogPosition {
                    // A peer that predates `log_epoch` reports only its
                    // assignment epoch, never below its log's.
                    epoch: reply.log_epoch.unwrap_or(reply.leader_epoch),
                    leo: reply.leo,
                    // A peer that predates `committed` knows only `hw`.
                    committed: reply.committed.unwrap_or(reply.hw),
                },
                in_isr: reply.in_isr,
                can_stand: !reply.ineligible,
            });
            state = next;
        }

        // `Idle`: a deferral is not a failed attempt, and the next lease tick
        // simply retries (`check_leases` restarts from `Idle` and from
        // `Abandoned` alike). It never delays a real failover: a crashed or
        // partitioned leader does not answer at all.
        if let Some((peer, reason)) = deferral {
            tracing::debug!(
                org_id = %key.0,
                topic = %key.1,
                partition = key.2,
                peer = %peer,
                own_epoch = assignment.leader_epoch,
                reason,
                "replication: deferring election"
            );
            self.set_promotion(&key, PromotionState::Idle);
            return;
        }

        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: Instant::now().max(leo_deadline),
            now_ms: now_ms(),
        });
        self.set_promotion(&key, state.clone());
        let Some(PromotionAction::ProposeAssignment(proposed)) = actions.into_iter().next() else {
            if let PromotionState::Abandoned { reason } = &state {
                tracing::debug!(
                    org_id = %key.0, topic = %key.1, partition = key.2, ?reason,
                    "replication: not standing this round"
                );
            }
            return;
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

    async fn query_leo(&self, key: &PartitionKey, peer: &str) -> Option<ReplLeoReply> {
        let (mut recv, mut send) = self.transport.open_stream(peer).await.ok()?;
        let known_epoch = self
            .registry
            .get(key)
            .map(|e| e.assignment.leader_epoch)
            .unwrap_or(0);
        let query = ReplFrame::LeoQuery(ReplLeoQuery {
            instance_id: self.instance_id.clone(),
            org_id: key.0.clone(),
            topic: key.1.clone(),
            partition: key.2,
            known_epoch,
        });
        frames::write_frame(&mut send, &query).await.ok()?;
        match frames::read_frame(&mut recv).await.ok()? {
            // `leader_epoch` was already on the wire and simply discarded here;
            // `run_election` uses it as proof that this node's assignment view
            // is merely stale.
            ReplFrame::LeoReply(r) => Some(r),
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
    /// Majority admission — `admitted_by_quorum` over the candidate's
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
        self.promote(key, proposed, actions).await;
        // Whatever `promote` concluded — leader, yielded, abandoned, spawn
        // failed — this attempt is over. An entry left in `Promoted` never
        // elects again (`check_leases` starts only from `Idle` or
        // `Abandoned`), so a node
        // that yielded to a leader which then died would leave the
        // partition leaderless for good.
        if let Some(mut entry) = self.registry.get_mut(key) {
            entry.promotion = PromotionState::Idle;
        }
    }

    async fn promote(
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
        //
        // `propose` materializes the op locally before returning, so the
        // stored row already carries the gate's verdict on THIS proposal.
        // No row, or a row of another topic incarnation, means the
        // materializer refused it: the topic was deleted (and perhaps
        // re-created) while the lease ran out. Promoting anyway would leave
        // a leader serving a partition the ledger no longer has — measured
        // in the three-process re-creation test as a node sitting at
        // `Leader { epoch: 4 }` of a deleted topic and refusing the new
        // incarnation's epoch-1 placement.
        let stored = match self
            .assignments
            .get(&self.instance_id, &key.0, &key.1, key.2)
        {
            Ok(None) => {
                tracing::info!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    proposed_epoch = assignment.leader_epoch,
                    "replication: abandoning promotion — the ledger no longer places \
                     this partition (topic deleted)"
                );
                self.forget_partition(key);
                return;
            }
            Ok(Some(stored)) if stored.topic_generation != assignment.topic_generation => {
                tracing::info!(
                    org_id = %key.0, topic = %key.1, partition = key.2,
                    stored_generation = stored.topic_generation,
                    proposed_generation = assignment.topic_generation,
                    "replication: abandoning promotion — the ledger places another \
                     incarnation of this topic"
                );
                self.apply_assignment(stored).await;
                return;
            }
            Ok(stored) => stored,
            Err(_) => None,
        };
        if let Some(stored) = stored {
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

        // An entry dropped while the election ran (`forget_partition`: the
        // placement went away) must stay dropped, not come back as a leader.
        let Some(from_node) = self
            .registry
            .get(key)
            .map(|e| e.assignment.leader_node_id.clone())
        else {
            return;
        };
        let started_at = Instant::now();

        // No dials here either (mirrors `apply_assignment`'s NON-NEGOTIABLE
        // note): the promotion's registry insert must not wait on ANY
        // dial — a dead peer's connect timeout must never delay the new
        // leader's own serving capability or leave its registry entry
        // removed. Every replica stream is the glue supervisor's job.
        match self.leader_factory.spawn_deferred(&assignment, Vec::new()) {
            Ok(handle) => {
                let handle: Arc<dyn LeaderHandle> = Arc::from(handle);
                // Switched IN PLACE, never removed and re-inserted. The old
                // follower runner's `stop()` blocks (it flushes partition
                // meta), and a key left absent for that long was measured
                // being picked up by the assignment poll, which applies this
                // very promotion's row: finding no entry, it spawned a second
                // leader handle, which the re-insert then overwrote without
                // stopping. Two live leaders of one partition kept replacing
                // each other's stream on every follower, so the new leader's
                // ISR kept dipping below quorum right after the failover.
                let mut old_follower = None;
                let mut old_leader = None;
                let mut spare = None;
                let mut claim = None;
                let serving = match self.registry.get_mut(key) {
                    Some(mut e) => {
                        let same_term = e.role == LocalRole::Leader
                            && e.assignment.leader_node_id == assignment.leader_node_id
                            && e.assignment.replicas == assignment.replicas
                            && e.assignment.leader_epoch == assignment.leader_epoch;
                        // This node already follows a claim that outranks the
                        // promotion — a peer's Hello was accepted while the
                        // election ran and its row has not landed yet. The
                        // settle check above read the ledger only.
                        let outranked = e.role == LocalRole::Follower
                            && claim_superseded(
                                assignment.leader_epoch,
                                &assignment.leader_node_id,
                                &e.assignment,
                            );
                        match e.leader.as_ref() {
                            _ if outranked => {
                                drop(e);
                                tracing::warn!(
                                    org_id = %key.0, topic = %key.1, partition = key.2,
                                    proposed_epoch = assignment.leader_epoch,
                                    "replication: yielding promotion — this node already \
                                     follows a claim that outranks it"
                                );
                                handle.stop();
                                return;
                            }
                            // The poll already installed this term: keep its
                            // handle, whose replica streams may be up.
                            Some(existing) if same_term => {
                                spare = Some(Arc::clone(&handle));
                                Arc::clone(existing)
                            }
                            _ => {
                                old_follower = e.follower.take();
                                e.unfollowed_since = Instant::now();
                                handle.open_writes();
                                claim = Some(Arc::clone(&handle));
                                old_leader = e.leader.replace(Arc::clone(&handle));
                                if e.role != LocalRole::Leader {
                                    e.leadership = self.next_leadership();
                                }
                                e.assignment = assignment.clone();
                                e.role = LocalRole::Leader;
                                e.promotion = PromotionState::Idle;
                                handle
                            }
                        }
                    }
                    // Dropped while the handle was being spawned: the
                    // placement went away, and so does this promotion.
                    None => {
                        handle.stop();
                        return;
                    }
                };
                // The election's truncate targets belong to the term, not to
                // the handle that happened to be spawned for it.
                for (node, to) in &truncates {
                    serving.send_truncate(node, *to);
                }
                // Outside the guard: every `stop()` and the claim block on the
                // engine.
                if let Some(serving) = claim {
                    serving.claim_term();
                }
                for stale in [spare, old_leader].into_iter().flatten() {
                    stale.stop();
                }
                if let Some(follower) = old_follower {
                    follower.stop();
                }
                self.assignments_changed.send_replace(());
                self.audit.failover(
                    &key.0,
                    &key.1,
                    key.2,
                    Some(from_node.as_str()),
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
        acks: Acks,
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
        // ledger round trip.
        //
        // A `Leader` entry with no handle is a real, if brief, state:
        // `apply_assignment` re-stamps the entry in place and only THEN
        // stops the old handle and spawns the new one, so that a candidate's
        // `LeoQuery` never sees this node deny its own leadership mid-
        // rebuild. Nothing can be published through that window — there is
        // no feeder to replicate it and no ack bookkeeping to wait on — so
        // it answers the same `NoAssignment` the removed entry used to.
        let Some(live_isr) = entry.leader.as_ref().map(|l| l.isr()) else {
            return Err(ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            });
        };
        // `acks=quorum` promises a majority of the replica set at every RF;
        // the other levels need `availability_quorum` — the same majority at
        // RF ≥ 3, one surviving in-sync replica at RF ≤ 2.
        let rf = entry.assignment.replicas.len();
        let required = match acks {
            Acks::Quorum => election::min_isr_required(rf),
            Acks::Leader | Acks::All => election::availability_quorum(rf),
        } as u32;
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
        // The quorum wait can last `timeout` (30 s by default); holding the
        // shard's read guard that long would block every `get_mut` on the
        // same shard — Hello fencing, stale-leadership step-down, role
        // changes — on whichever thread reaches it, a tokio worker
        // included. Resolve everything under the guard, then drop it.
        let deadline = Instant::now() + timeout;
        let mut wait = {
            let entry = self
                .registry
                .get(&key)
                .ok_or_else(|| ReplError::NoAssignment {
                    topic: topic.to_string(),
                    partition,
                })?;
            let Some(handle) = entry.leader.clone() else {
                return Err(ReplError::NoAssignment {
                    topic: topic.to_string(),
                    partition,
                });
            };
            AckWait {
                truncations: handle.truncation_count(),
                replicas: entry.assignment.replicas.len(),
                handle,
                epoch: entry.assignment.leader_epoch,
                leadership: entry.leadership,
            }
        };
        loop {
            let started = Instant::now();
            let remaining = deadline.saturating_duration_since(started);
            let required = required_acks(acks, wait.replicas, wait.handle.isr().len());
            let slice = match acks {
                Acks::All => remaining.min(ACKS_ALL_ISR_RECHECK),
                Acks::Leader | Acks::Quorum => remaining,
            };
            let outcome = wait.handle.await_acks(next_offset, required, slice);
            if outcome.acked_nodes >= outcome.required || Instant::now() >= deadline {
                return Ok(outcome);
            }
            // The slice ran out, not the handle: re-read the live ISR.
            if Instant::now() >= started + slice {
                continue;
            }
            // Back before the deadline without the quorum: the handle was
            // stopped under this publish. See `successor_leader`.
            match self.successor_leader(&key, &wait, next_offset, deadline) {
                Some(next) => wait = next,
                None => return Ok(outcome),
            }
        }
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
            if election::admitted_by_quorum(&acked, &proposed.replicas, &self.local_node_id) {
                if let Some(mut entry) = self.registry.get_mut(&key) {
                    if entry.role == LocalRole::Leader {
                        if let Some(leader) = entry.leader.take() {
                            leader.stop();
                        }
                        // Corrected to `Follower`/`NotReplica` by the next
                        // `apply_assignment` once the op materializes back.
                        entry.role = LocalRole::NotReplica;
                        entry.leadership = self.next_leadership();
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
            // An empty replica set is `delete_topic`/`purge_org`'s "stop
            // replicating this" signal: tear the local state down now rather
            // than when the assignment poll notices the rows are gone.
            if replicas.is_empty() {
                self.forget_partition(&key);
            }
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

    fn local_node_id(&self) -> String {
        self.local_node_id.clone()
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
        instance_id: &str,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<Option<PartitionAssignment>, ReplError> {
        self.get(instance_id, org, topic, partition)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    fn list_for_topic(
        &self,
        instance_id: &str,
        org: &str,
        topic: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        self.list_for_topic(instance_id, org, topic)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    fn list_for_node(
        &self,
        instance_id: &str,
        node_id: &str,
    ) -> Result<Vec<PartitionAssignment>, ReplError> {
        self.list_for_node(instance_id, node_id)
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
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
            instance_id: "tentabus-00000001".to_string(),
            org_id: org.to_string(),
            topic: topic.to_string(),
            partition,
            leader_node_id: leader.to_string(),
            replicas: replicas.iter().map(|s| s.to_string()).collect(),
            isr: isr.iter().map(|s| s.to_string()).collect(),
            leader_epoch: epoch,
            updated_at_ms: 0,
            topic_generation: 0,
        }
    }

    // ---- fakes ---------------------------------------------------------

    #[derive(Clone)]
    enum PeerScript {
        /// A follower of the querying candidate's own term: answers at the
        /// query's `known_epoch`, its log written under that epoch.
        LeoReply {
            leo: u64,
            in_isr: bool,
        },
        /// Answers exactly this reply.
        Reply(ReplLeoReply),
        Unreachable,
        /// A dial that never completes — a dead peer whose connect attempt
        /// has no timeout of its own.
        Hang,
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
            if matches!(script, PeerScript::Hang) {
                std::future::pending::<()>().await;
            }
            let (ours, theirs) = tokio::io::duplex(16 * 1024);
            tokio::spawn(async move {
                let (mut peer_recv, mut peer_send) = split(theirs);
                if let Ok(ReplFrame::LeoQuery(query)) = frames::read_frame(&mut peer_recv).await {
                    let reply = match script {
                        PeerScript::LeoReply { leo, in_isr } => ReplLeoReply {
                            leo,
                            hw: leo,
                            leader_epoch: query.known_epoch,
                            in_isr,
                            log_epoch: Some(query.known_epoch),
                            leading: false,
                            ineligible: false,
                            committed: None,
                            leader_alive: false,
                        },
                        PeerScript::Reply(reply) => reply,
                        PeerScript::Unreachable | PeerScript::Hang => return,
                    };
                    let _ = frames::write_frame(&mut peer_send, &ReplFrame::LeoReply(reply)).await;
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
    /// `incoming.leader_node_id < stored.leader_node_id`), plus the topic
    /// incarnation gate: a placement of an incarnation created before the
    /// topic's last deletion is dropped.
    struct FakeAssignmentStore {
        rows: Mutex<std::collections::HashMap<PartitionKey, PartitionAssignment>>,
        deleted_generations: Mutex<std::collections::HashMap<(String, String), u64>>,
        /// Runs once inside the next `propose`, after it has decided — a
        /// hook for something else happening while an election is running.
        on_propose: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        next_op: Mutex<u8>,
        fail_next: AtomicBool,
    }

    impl FakeAssignmentStore {
        fn new() -> Self {
            Self {
                rows: Mutex::new(std::collections::HashMap::new()),
                deleted_generations: Mutex::new(std::collections::HashMap::new()),
                on_propose: Mutex::new(None),
                next_op: Mutex::new(1),
                fail_next: AtomicBool::new(false),
            }
        }

        fn seed(&self, a: PartitionAssignment) {
            let key = (a.org_id.clone(), a.topic.clone(), a.partition);
            self.rows.lock().insert(key, a);
        }

        /// What materializing a topic delete does to this table: the
        /// placements go, and the delete keeps its place in the order a late
        /// placement of the deleted incarnation is judged against.
        fn delete_topic(&self, org: &str, topic: &str, generation: u64) {
            self.rows
                .lock()
                .retain(|k, _| !(k.0 == org && k.1 == topic));
            self.deleted_generations
                .lock()
                .insert((org.to_string(), topic.to_string()), generation);
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
            instance_id: &str,
            org: &str,
            topic: &str,
            partition: u32,
        ) -> Result<Option<PartitionAssignment>, ReplError> {
            Ok(self
                .rows
                .lock()
                .get(&(org.to_string(), topic.to_string(), partition))
                .filter(|a| a.instance_id == instance_id)
                .cloned())
        }

        fn list_for_topic(
            &self,
            instance_id: &str,
            org: &str,
            topic: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            Ok(self
                .rows
                .lock()
                .values()
                .filter(|a| a.instance_id == instance_id && a.org_id == org && a.topic == topic)
                .cloned()
                .collect())
        }

        fn list_for_node(
            &self,
            instance_id: &str,
            node_id: &str,
        ) -> Result<Vec<PartitionAssignment>, ReplError> {
            Ok(self
                .rows
                .lock()
                .values()
                .filter(|a| a.instance_id == instance_id && a.replicas.iter().any(|r| r == node_id))
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
            let deleted_generation = self
                .deleted_generations
                .lock()
                .get(&(key.0.clone(), key.1.clone()))
                .copied()
                .unwrap_or(0);
            let mut rows = self.rows.lock();
            let admitted = match rows.get(&key) {
                _ if assignment.topic_generation < deleted_generation => false,
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
            if let Some(hook) = self.on_propose.lock().take() {
                hook();
            }
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
        stale_epoch: AtomicU32,
        /// How long `await_acks` blocks before answering — stands in for a
        /// quorum that takes its time.
        ack_hold: Mutex<Duration>,
        /// How long `await_acks` still takes to return once the handle is
        /// stopped — a waiter thread descheduled right after its wait ended,
        /// which is when the registry can move on underneath it.
        stop_grace: Mutex<Duration>,
        truncations: AtomicU64,
        quorum_lease: AtomicBool,
        log_epoch: AtomicU32,
        committed: AtomicU64,
        /// When set, the replicas `await_acks` reports as acked instead of
        /// the whole ISR — a follower in the ISR that never acks.
        acked_override: Mutex<Option<u32>>,
        /// `open_writes` ran: this handle was installed as the serving one.
        writes_opened: AtomicBool,
        /// Runs inside `stop()` — a hook to observe what `stop()` runs under.
        on_stop: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        /// `claim_term` ran.
        claimed: AtomicBool,
        /// Runs inside `claim_term()`, like `on_stop`; set from the factory's
        /// `claim_hook` at spawn, since the manager claims right after it
        /// installs the handle.
        on_claim: Option<ClaimHook>,
    }

    impl FakeLeaderHandle {
        fn new(isr: Vec<String>, hw: u64, leo: u64) -> Self {
            Self {
                isr: Mutex::new(isr),
                hw: AtomicU64::new(hw),
                leo: AtomicU64::new(leo),
                truncated: Mutex::new(Vec::new()),
                stopped: AtomicBool::new(false),
                stale_epoch: AtomicU32::new(0),
                ack_hold: Mutex::new(Duration::ZERO),
                stop_grace: Mutex::new(Duration::ZERO),
                truncations: AtomicU64::new(0),
                quorum_lease: AtomicBool::new(true),
                log_epoch: AtomicU32::new(0),
                committed: AtomicU64::new(hw),
                acked_override: Mutex::new(None),
                writes_opened: AtomicBool::new(false),
                on_stop: Mutex::new(None),
                claimed: AtomicBool::new(false),
                on_claim: None,
            }
        }

        /// Stands in for a peer refusing this leader's outbound `Hello`
        /// with `ReplReject::StaleEpoch { have }` — what `glue.rs`'s
        /// follower supervisor records on the real handle.
        fn note_stale_epoch(&self, have: u32) {
            self.stale_epoch.store(have, Ordering::SeqCst);
        }

        fn set_isr(&self, isr: Vec<String>) {
            *self.isr.lock() = isr;
        }
    }

    impl LeaderHandle for FakeLeaderHandle {
        fn isr(&self) -> Vec<String> {
            self.isr.lock().clone()
        }
        fn truncation_count(&self) -> u64 {
            self.truncations.load(Ordering::SeqCst)
        }
        fn high_watermark(&self) -> u64 {
            self.hw.load(Ordering::SeqCst)
        }
        fn log_end_offset(&self) -> u64 {
            self.leo.load(Ordering::SeqCst)
        }
        /// Holds for `ack_hold` (capped at `timeout`) like a quorum that
        /// takes its time, but — like the real handle's closed waiters —
        /// returns as soon as the handle is stopped, with only the leader's
        /// own ack.
        fn await_acks(&self, _next_offset: u64, required: u32, timeout: Duration) -> AckOutcome {
            let until = std::time::Instant::now() + (*self.ack_hold.lock()).min(timeout);
            while std::time::Instant::now() < until && !self.stopped.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            let acked_nodes = if self.stopped.load(Ordering::SeqCst) {
                std::thread::sleep(*self.stop_grace.lock());
                1
            } else {
                self.acked_override
                    .lock()
                    .unwrap_or(self.isr.lock().len() as u32)
            };
            AckOutcome {
                acked_nodes,
                required,
                hw: self.high_watermark(),
            }
        }
        fn note_offset_commit(&self, _group: &str, _partition: u32, _offset: u64, _attempts: u32) {}
        fn send_truncate(&self, node: &str, to_offset: u64) {
            self.truncated.lock().push((node.to_string(), to_offset));
        }
        fn observed_stale_epoch(&self) -> Option<u32> {
            match self.stale_epoch.load(Ordering::SeqCst) {
                0 => None,
                e => Some(e),
            }
        }
        fn holds_quorum_lease(&self) -> bool {
            self.quorum_lease.load(Ordering::SeqCst)
        }
        fn open_writes(&self) {
            self.writes_opened.store(true, Ordering::SeqCst);
        }
        fn claim_term(&self) {
            self.claimed.store(true, Ordering::SeqCst);
            if let Some(hook) = &self.on_claim {
                hook();
            }
        }
        fn log_epoch(&self) -> u32 {
            self.log_epoch.load(Ordering::SeqCst)
        }
        fn committed_offset(&self) -> u64 {
            self.committed.load(Ordering::SeqCst)
        }
        fn stop(&self) {
            self.stopped.store(true, Ordering::SeqCst);
            if let Some(hook) = self.on_stop.lock().take() {
                hook();
            }
        }
    }

    type ClaimHook = Arc<dyn Fn() + Send + Sync>;

    struct FakeLeaderFactory {
        /// Handed to every handle spawned from now on (`on_claim`).
        claim_hook: Mutex<Option<ClaimHook>>,
        spawned: Mutex<Vec<PartitionAssignment>>,
        fail: AtomicBool,
        // `Arc`, not the `Box<dyn LeaderHandle>` `spawn` hands to the
        // manager: the live-ISR test below needs to keep mutating the SAME
        // handle (`set_isr`) after the manager already owns it.
        handles: Mutex<Vec<Arc<FakeLeaderHandle>>>,
        /// `log_end_offset` / `truncation_count` of every handle spawned
        /// from now on — the local log a rebuilt leader handle would see.
        spawn_leo: AtomicU64,
        spawn_truncations: AtomicU64,
    }

    impl FakeLeaderFactory {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                claim_hook: Mutex::new(None),
                spawned: Mutex::new(Vec::new()),
                fail: AtomicBool::new(false),
                handles: Mutex::new(Vec::new()),
                spawn_leo: AtomicU64::new(0),
                spawn_truncations: AtomicU64::new(0),
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
        fn truncation_count(&self) -> u64 {
            self.0.truncation_count()
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
        fn observed_stale_epoch(&self) -> Option<u32> {
            self.0.observed_stale_epoch()
        }
        fn holds_quorum_lease(&self) -> bool {
            self.0.holds_quorum_lease()
        }
        fn open_writes(&self) {
            self.0.open_writes()
        }
        fn claim_term(&self) {
            self.0.claim_term()
        }
        fn log_epoch(&self) -> u32 {
            self.0.log_epoch()
        }
        fn committed_offset(&self) -> u64 {
            self.0.committed_offset()
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
            let handle = Arc::new(FakeLeaderHandle {
                on_claim: self.claim_hook.lock().clone(),
                ..FakeLeaderHandle::new(
                    assignment.isr.clone(),
                    0,
                    self.spawn_leo.load(Ordering::SeqCst),
                )
            });
            handle.truncations.store(
                self.spawn_truncations.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            self.handles.lock().push(Arc::clone(&handle));
            Ok(Box::new(SharedFakeLeaderHandle(handle)))
        }
    }

    struct FakeFollowerHandle {
        leo: AtomicU64,
        hw: AtomicU64,
        lease_expired: AtomicBool,
        log_epoch: AtomicU32,
        committed: AtomicU64,
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
        fn log_epoch(&self) -> u32 {
            self.shared.log_epoch.load(Ordering::SeqCst)
        }
        fn committed(&self) -> u64 {
            self.shared.committed.load(Ordering::SeqCst)
        }
        fn mark_leader_disconnected(&self) {
            self.shared.disconnected.store(true, Ordering::SeqCst);
        }
        fn stop(&self) {
            self.shared.stopped.store(true, Ordering::SeqCst);
        }
    }

    /// The lease `FakeFollowerFactory` reports; `PAST_THE_LEASE` outlasts it.
    const FAKE_UNDIALED_LEASE: Duration = Duration::from_millis(100);
    const FAKE_LEADER_LEASE: Duration = Duration::from_millis(50);

    struct FakeFollowerFactory {
        handles: Mutex<Vec<Arc<FakeFollowerHandle>>>,
        /// The local log `local_log_position` reads for any partition: end
        /// offset, high watermark (capped at the end offset) and epoch
        /// (`None`: written under the assignment's own epoch).
        local_leo: AtomicU64,
        local_hw: AtomicU64,
        local_epoch: Mutex<Option<u32>>,
        local_read_fails: AtomicBool,
        /// How many times `cut_to_committed` ran.
        cuts: Mutex<u32>,
        /// `fence:<epoch>` / `cut`, in call order.
        events: Mutex<Vec<String>>,
    }

    impl FakeFollowerFactory {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                handles: Mutex::new(Vec::new()),
                local_leo: AtomicU64::new(0),
                local_hw: AtomicU64::new(u64::MAX),
                local_epoch: Mutex::new(None),
                local_read_fails: AtomicBool::new(false),
                cuts: Mutex::new(0),
                events: Mutex::new(Vec::new()),
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
                follower_log_epoch: None,
                follower_committed: None,
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
                // The records already on the local log keep their epoch; a
                // test that sets none follows a leader whose records it has.
                log_epoch: AtomicU32::new(self.local_epoch.lock().unwrap_or(hello.leader_epoch)),
                committed: AtomicU64::new(0),
                disconnected: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            });
            self.handles.lock().push(Arc::clone(&shared));
            Ok(Box::new(FakeFollowerRunner { shared }))
        }

        fn undialed_lease(&self) -> Duration {
            FAKE_UNDIALED_LEASE
        }

        fn leader_lease(&self) -> Duration {
            FAKE_LEADER_LEASE
        }

        fn fence_to_epoch(
            &self,
            _assignment: &PartitionAssignment,
            epoch: u32,
        ) -> Result<(), ReplError> {
            self.events.lock().push(format!("fence:{epoch}"));
            Ok(())
        }

        fn cut_to_committed(
            &self,
            assignment: &PartitionAssignment,
        ) -> Result<LogPosition, ReplError> {
            self.events.lock().push("cut".to_string());
            let log = self.local_log_position(assignment)?;
            self.local_leo.store(log.committed, Ordering::SeqCst);
            *self.cuts.lock() += 1;
            self.local_log_position(assignment)
        }

        fn local_log_position(
            &self,
            assignment: &PartitionAssignment,
        ) -> Result<LogPosition, ReplError> {
            if self.local_read_fails.load(Ordering::SeqCst) {
                return Err(ReplError::Internal("forced local log read failure".into()));
            }
            let leo = self.local_leo.load(Ordering::SeqCst);
            Ok(LogPosition {
                epoch: self.local_epoch.lock().unwrap_or(assignment.leader_epoch),
                leo,
                committed: self.local_hw.load(Ordering::SeqCst).min(leo),
            })
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
            instance_id: "tentabus-00000001".to_string(),
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

    // W5 review finding D2: `apply_assignment` must refuse a row whose
    // `instance_id` does not match this manager's own — the registry key
    // carries no instance component (`PartitionKey`'s own doc), so without
    // this check a caller that forgot to filter by instance would spawn a
    // `LeaderHandle` for another instance's partition here and then reject
    // that instance's real leader's `Hello` as `UnknownInstance` forever.
    #[tokio::test]
    async fn apply_assignment_refuses_a_row_for_a_different_instance() {
        let fx = build("l");
        let mut a = assignment("org", "orders", 0, "l", &["l", "f1"], &["l", "f1"], 1);
        a.instance_id = "tentabus-0badc0de".to_string();
        fx.manager.apply_assignment(a).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment
            },
            "a row for another instance must never land in this manager's registry"
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
        // What `run_election` guarantees before it gets here: the node
        // already replicates the partition, and `propose` has materialized
        // the proposal locally.
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
        fx.assignments.seed(proposed.clone());

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
            store
                .get("tentabus-00000001", "org", "t", 0)
                .unwrap()
                .unwrap()
                .leader_node_id,
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
            store
                .get("tentabus-00000001", "org", "t", 0)
                .unwrap()
                .unwrap()
                .leader_node_id,
            "a"
        );

        // A third, higher-node-id proposal at the SAME epoch must now lose
        // against the already-admitted "a".
        let mut from_c = base;
        from_c.leader_node_id = "c".to_string();
        from_c.leader_epoch = 2;
        store.propose(from_c).expect("c proposes third");
        assert_eq!(
            store
                .get("tentabus-00000001", "org", "t", 0)
                .unwrap()
                .unwrap()
                .leader_node_id,
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

    /// An epoch bump that keeps this node as leader rebuilds the leader
    /// handle, and stopping the old one ends its quorum wait at once. The
    /// record is in this node's log either way, so a publish in flight must
    /// finish on the successor handle instead of failing with the old one's
    /// partial count (measured: the chaos test's steady-state phase failing
    /// with `acked=1, required=2` 0.5 s into a 1.5 s ack timeout).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ack_wait_carries_over_to_the_same_nodes_rebuilt_leader() {
        let fx = build("l");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
            .await;
        *fx.leader_factory.handles.lock()[0].ack_hold.lock() = Duration::from_secs(30);
        fx.leader_factory.spawn_leo.store(1, Ordering::SeqCst);

        let manager = fx.manager.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome =
                manager.await_acks("org", "orders", 0, 1, Acks::Quorum, Duration::from_secs(10));
            (outcome, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 2))
            .await;
        assert_eq!(
            fx.leader_factory.handles.lock().len(),
            2,
            "rebuilt at epoch 2"
        );

        let (outcome, elapsed) = waiter.join().unwrap();
        let outcome = outcome.expect("the partition still has a leader here");
        assert!(
            outcome.acked_nodes >= outcome.required,
            "the wait must finish on the successor handle: {outcome:?}"
        );
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    /// Starts a publish's quorum wait (offset 1, 10 s budget) on `fx`'s
    /// first leader handle, held open until that handle is stopped.
    fn pending_ack_wait(
        fx: &Fixture,
    ) -> std::thread::JoinHandle<(Result<AckOutcome, ReplError>, Duration)> {
        *fx.leader_factory.handles.lock()[0].ack_hold.lock() = Duration::from_secs(30);
        let manager = fx.manager.clone();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome =
                manager.await_acks("org", "orders", 0, 1, Acks::Quorum, Duration::from_secs(10));
            (outcome, started.elapsed())
        })
    }

    /// Asserts the wait ended promptly WITHOUT the quorum — the publish
    /// fails, it is not confirmed through a handle that may not own its
    /// record.
    fn assert_not_carried_over(
        waiter: std::thread::JoinHandle<(Result<AckOutcome, ReplError>, Duration)>,
    ) {
        let (outcome, elapsed) = waiter.join().unwrap();
        let outcome = outcome.expect("an outcome, not an error");
        assert!(
            outcome.acked_nodes < outcome.required,
            "must not carry over: {outcome:?}"
        );
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    /// Review finding 1: L leads at N with a publish pending; X's Hello at
    /// N+1 fences L (and X's handshake may truncate L's log below the
    /// offset); then L's own ledger row names L at N+1 — L ranks below X, so
    /// it outranks X's claim and L leads again at the very epoch it was
    /// fenced to. The epoch alone looks like "one term later, same node",
    /// yet another node's authority held the log in between: the wait must
    /// not move onto the new handle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ack_wait_does_not_carry_over_a_break_in_leadership() {
        let fx = build("L");
        let replicas = ["L", "M", "X"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "L", &replicas, &replicas, 1))
            .await;
        fx.leader_factory.spawn_leo.store(1, Ordering::SeqCst);
        // The waiter only looks for a successor after the fence AND the
        // re-promotion below have both landed — the interleaving where the
        // epoch, the role and the handle all look like a same-node rebuild.
        *fx.leader_factory.handles.lock()[0].stop_grace.lock() = Duration::from_millis(1500);
        let waiter = pending_ack_wait(&fx);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org", "orders", 0, "X", &replicas, &replicas, 2,
            )),
        )
        .await;
        assert!(ack.accepted, "{:?}", ack.reject);
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "L", &replicas, &replicas, 2))
            .await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        ));
        assert_not_carried_over(waiter);
    }

    /// Two terms later with no break in the local role still means terms
    /// passed that this node's handle did not lead through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ack_wait_does_not_carry_over_two_terms() {
        let fx = build("l");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
            .await;
        fx.leader_factory.spawn_leo.store(1, Ordering::SeqCst);
        let waiter = pending_ack_wait(&fx);
        tokio::time::sleep(Duration::from_millis(200)).await;
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 3))
            .await;
        assert_not_carried_over(waiter);
    }

    /// A truncation of the local log since the wait began, or a successor
    /// whose log no longer reaches the waited offset, means the offset may
    /// name a different record now.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ack_wait_does_not_carry_over_a_truncated_log() {
        for (leo, truncations) in [(1, 1), (0, 0)] {
            let fx = build("l");
            let replicas = ["l", "f1", "f2"];
            fx.manager
                .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
                .await;
            fx.leader_factory.spawn_leo.store(leo, Ordering::SeqCst);
            fx.leader_factory
                .spawn_truncations
                .store(truncations, Ordering::SeqCst);
            let waiter = pending_ack_wait(&fx);
            tokio::time::sleep(Duration::from_millis(200)).await;
            fx.manager
                .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 2))
                .await;
            assert_not_carried_over(waiter);
        }
    }

    /// Review finding 2: `delete_topic` signals `reassign(.., None, &[])`.
    /// The local entry must go with it — handles stopped — so the same
    /// topic recreated later places at epoch 1 and is applied, instead of
    /// being refused as older than the dead incarnation's last term.
    #[tokio::test]
    async fn a_deleted_topic_can_be_recreated_at_epoch_one() {
        let fx = build("l");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 4))
            .await;
        let old = fx.leader_factory.handles.lock()[0].clone();

        fx.manager.reassign("org", "orders", None, &[]).unwrap();
        assert!(
            old.stopped.load(Ordering::SeqCst),
            "the dead incarnation's handle is stopped"
        );
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Unavailable { .. }
        ));

        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
            .await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 1 }
        ));
    }

    /// The carry-over is only for this node staying leader: a rebuild that
    /// hands the partition to another node ends the wait promptly with the
    /// partial count, not at the ack timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ack_wait_ends_promptly_when_this_node_stops_leading() {
        let fx = build("l");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
            .await;
        *fx.leader_factory.handles.lock()[0].ack_hold.lock() = Duration::from_secs(30);

        let manager = fx.manager.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome =
                manager.await_acks("org", "orders", 0, 1, Acks::Quorum, Duration::from_secs(10));
            (outcome, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        fx.manager
            .apply_assignment(assignment(
                "org", "orders", 0, "f1", &replicas, &replicas, 2,
            ))
            .await;

        let (outcome, elapsed) = waiter.join().unwrap();
        let outcome = outcome.unwrap();
        assert!(outcome.acked_nodes < outcome.required, "{outcome:?}");
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    /// The registry can be ahead of the ledger: a winning peer's Hello
    /// fences this node to the peer's term before the ledger row for that
    /// term arrives. The older row — here this node's own create-time
    /// placement — must not then roll the partition back to an earlier
    /// term. Measured in the chaos test: a follower at epoch 2 re-promoted
    /// itself to leader at epoch 1 in the middle of the steady-state phase,
    /// tearing down the real leader's stream.
    #[tokio::test]
    async fn an_older_assignment_never_rolls_back_a_newer_term() {
        let fx = build("f1");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 2))
            .await;
        let followed = fx.manager.role("org", "orders", 0);

        let mut own_placement = assignment("org", "orders", 0, "f1", &replicas, &replicas, 1);
        own_placement.updated_at_ms += 1;
        fx.manager.apply_assignment(own_placement).await;
        assert_eq!(fx.manager.role("org", "orders", 0), followed, "older term");

        // Same term, a leader ranked above the current one: the ledger's own
        // tie-break (lower node id wins) would refuse it too.
        fx.manager
            .apply_assignment(assignment(
                "org",
                "orders",
                0,
                "z",
                &["l", "f1", "z"],
                &["l", "f1", "z"],
                2,
            ))
            .await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            followed,
            "same term, higher id"
        );
        assert!(
            fx.leader_factory.handles.lock().is_empty(),
            "never promoted"
        );

        // A newer term still applies.
        fx.manager
            .apply_assignment(assignment(
                "org", "orders", 0, "f1", &replicas, &replicas, 3,
            ))
            .await;
        assert_eq!(
            fx.leader_factory.handles.lock().len(),
            1,
            "promoted at epoch 3"
        );
    }

    /// A publish's quorum wait can last the whole ack timeout (30 s by
    /// default). It must not keep the registry shard's read guard for that
    /// long: Hello fencing, stale-leadership step-down and role changes all
    /// take the same entry with `get_mut`, and would stall — on a Tokio
    /// worker — until the unrelated publish gave up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pending_ack_wait_does_not_hold_the_registry_entry() {
        let fx = build("l");
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
        let handle = fx.leader_factory.handles.lock()[0].clone();
        *handle.ack_hold.lock() = Duration::from_secs(3);

        let manager = fx.manager.clone();
        let waiter = std::thread::spawn(move || {
            manager.await_acks("org", "orders", 0, 1, Acks::Quorum, Duration::from_secs(5))
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let key: PartitionKey = ("org".to_string(), "orders".to_string(), 0);
        let started = std::time::Instant::now();
        drop(fx.manager.registry.get_mut(&key).expect("registry entry"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "get_mut waited {:?} behind a pending ack wait",
            started.elapsed()
        );
        assert!(waiter.join().unwrap().is_ok());
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
            instance_id: "tentabus-00000001".into(),
            org_id: "org".into(),
            topic: "orders".into(),
            partition: 0,
            leader_node_id: "l".into(),
            leader_epoch: 1,
            replicas: vec!["l".into(), "f1".into()],
            environment: NodeEnvironment::Prod,
            topic_generation: Some(0),
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

    // ---- Stale leadership: a peer proved this node's claim is over -------
    //
    // The three-process chaos scenario's phase 5, reproduced at manager
    // level: a node that crashed while leading comes back, reads its OWN
    // stale row from disk and believes it still leads. Its ledger copy is
    // the thing that is behind, so nothing local will ever correct it —
    // the only authoritative fact it receives is the `StaleEpoch` refusal
    // its outbound `Hello` collects. Before `check_stale_leadership` that
    // refusal was logged and dropped, and the node answered
    // `ROLE Leader { epoch: 2 }` for the whole 30 s rejoin window while
    // both peers refused it.

    #[tokio::test]
    async fn a_leader_refused_with_a_newer_epoch_steps_down_onto_the_ledgers_row() {
        let fx = build("a");
        let stale = assignment(
            "org",
            "orders",
            0,
            "a",
            &["a", "b", "c"],
            &["a", "b", "c"],
            2,
        );
        fx.manager.apply_assignment(stale).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        );

        // The ledger already settled the partition on `b` at epoch 3 — the
        // clean path: role, handles and epoch all come from that one row.
        fx.assignments.seed(assignment(
            "org",
            "orders",
            0,
            "b",
            &["a", "b", "c"],
            &["b", "c"],
            3,
        ));
        fx.leader_factory.handles.lock()[0].note_stale_epoch(3);

        fx.manager.check_stale_leadership().await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "b".to_string(),
                epoch: 3
            },
            "the step-down must follow the leader the ledger names, not merely stop leading"
        );
        assert!(
            fx.manager
                .preflight("org", "orders", 0, Acks::Quorum)
                .is_err(),
            "a stepped-down node must refuse publishes — the whole point is that writes \
             accepted here would never be replicated"
        );
    }

    #[tokio::test]
    async fn a_leader_refused_with_a_newer_epoch_steps_down_even_with_no_ledger_row_yet() {
        let fx = build("a");
        let stale = assignment(
            "org",
            "orders",
            0,
            "a",
            &["a", "b", "c"],
            &["a", "b", "c"],
            2,
        );
        fx.manager.apply_assignment(stale).await;
        let handle = fx.leader_factory.handles.lock()[0].clone();
        handle.note_stale_epoch(3);
        // Deliberately NO seeded row: this is the restarted ex-leader whose
        // own ledger copy has not caught up. Stepping down must not depend
        // on it, or the node keeps serving until sync happens to arrive.

        fx.manager.check_stale_leadership().await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: String::new(),
                epoch: 3
            },
            "with no row naming the new leader the node must still stop leading, at the \
             epoch the peer proved, with the leader left unnamed"
        );
        assert!(
            handle.stopped.load(Ordering::SeqCst),
            "the leader handle must be stopped, not just relabelled — it is what feeds replicas"
        );
        assert!(fx
            .manager
            .preflight("org", "orders", 0, Acks::Quorum)
            .is_err());
    }

    #[tokio::test]
    async fn a_leader_refused_with_its_own_or_an_older_epoch_keeps_leading() {
        let fx = build("a");
        let a = assignment(
            "org",
            "orders",
            0,
            "a",
            &["a", "b", "c"],
            &["a", "b", "c"],
            3,
        );
        fx.manager.apply_assignment(a).await;
        // Equal, then older: neither proves anything. A stale probe from a
        // peer that has not caught up must never unseat a live leader.
        for have in [3u32, 2] {
            fx.leader_factory.handles.lock()[0].note_stale_epoch(have);
            fx.manager.check_stale_leadership().await;
            assert_eq!(
                fx.manager.role("org", "orders", 0),
                PartitionRole::Leader { epoch: 3 },
                "a refusal carrying epoch {have} must not unseat a leader at epoch 3"
            );
        }
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
            instance_id: a.instance_id.clone(),
            org_id: a.org_id.clone(),
            topic: a.topic.clone(),
            partition: a.partition,
            leader_node_id: a.leader_node_id.clone(),
            leader_epoch: a.leader_epoch,
            replicas: a.replicas.clone(),
            environment: NodeEnvironment::Prod,
            topic_generation: Some(a.topic_generation),
        }
    }

    /// Writes one `Hello` at `manager`'s real `accept_stream` over an
    /// in-memory duplex and returns the `HelloAck` it gets back. Both halves
    /// of the leader side stay alive until the ack is read, so a `200 OK`
    /// means "accepted on this stream", not "accepted then hung up".
    async fn hello_roundtrip(manager: Arc<ReplicationManager>, hello: ReplHello) -> ReplHelloAck {
        let remote = hello.leader_node_id.clone();
        hello_roundtrip_from(manager, &remote, hello).await
    }

    /// `hello_roundtrip` with the dialing peer's mesh identity given
    /// explicitly, for Hellos that claim a leader other than their sender.
    async fn hello_roundtrip_from(
        manager: Arc<ReplicationManager>,
        remote: &str,
        hello: ReplHello,
    ) -> ReplHelloAck {
        let remote = remote.to_string();
        let (leader_side, follower_side) = tokio::io::duplex(16 * 1024);
        let (mut leader_recv, mut leader_send) = split(leader_side);
        let (follower_recv, follower_send) = split(follower_side);
        tokio::spawn(async move {
            manager
                .accept_stream(remote, Box::new(follower_recv), Box::new(follower_send))
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

    // W5 review finding T2 (plan-app-platform §7 W5's own test line): a
    // frame encoded without `instance_id` decodes to an empty string
    // (`frames.rs`'s own unit test covers that decode half) — this covers
    // the other half the plan asks for, that an EMPTY `instance_id` is then
    // REJECTED by `accept_hello`, not merely decoded and left unverified.
    // `route_stream`'s own `BusInstanceId::parse("").ok()` already fails
    // shape validation before an empty instance_id could ever reach a real
    // manager (covered separately in `router.rs`'s own tests), so an empty
    // instance_id landing HERE is exactly the "direct test call, or a
    // future bug bypassing the router" case `accept_hello`'s own doc
    // names — driven here over the real `accept_stream` entry point, not a
    // hand-rolled shortcut.
    #[tokio::test]
    async fn a_hello_with_an_empty_instance_id_is_rejected_before_any_registry_wait() {
        let fx = build("f1");
        let mut hello = hello_from(&assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1"],
            &["l", "f1"],
            1,
        ));
        hello.instance_id = String::new();
        let started = Instant::now();
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello).await;

        assert!(!ack.accepted);
        assert_eq!(ack.reject, Some(ReplReject::UnknownInstance));
        assert!(
            started.elapsed() < ASSIGNMENT_AWAIT,
            "an empty instance_id must be refused immediately, before the \
             registry-materialization wait ever starts ({:?} elapsed)",
            started.elapsed()
        );
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
        // No leader has dialed yet: the answer comes from the local log.
        fx.follower_factory.local_leo.store(7, Ordering::SeqCst);

        let reply = accept_roundtrip(
            Arc::clone(&fx.manager),
            &ReplFrame::LeoQuery(ReplLeoQuery {
                instance_id: "tentabus-00000001".into(),
                org_id: "org".into(),
                topic: "orders".into(),
                partition: 0,
                known_epoch: 3,
            }),
        )
        .await;
        match reply {
            ReplFrame::LeoReply(r) => {
                // leo/hw from the local log (no runner attached), the epoch
                // from the registry's assignment and `in_isr` from its ISR.
                assert_eq!((r.leo, r.hw, r.leader_epoch, r.in_isr), (7, 7, 3, true));
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
                instance_id: "tentabus-00000001".into(),
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
                instance_id: "tentabus-00000001".into(),
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

    /// A follower whose registry still holds an older term (the ledger
    /// row for the new one has not arrived) adopts the term of the Hello it
    /// accepts, and the newer ledger row arriving afterwards is then a
    /// metadata update, not a rebuild that stops the live stream. Measured
    /// in the chaos test: followers kept reporting `Follower { leader: C,
    /// epoch: 1 }` while streaming from A at epoch 2, and the epoch-2 row's
    /// arrival tore A's stream down in the middle of its writes.
    #[tokio::test]
    async fn a_follower_adopts_the_newer_term_of_the_hello_it_accepts() {
        let fx = build("F");
        let replicas = ["A", "C", "F"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "C", &replicas, &replicas, 1))
            .await;
        let newer = assignment("org", "orders", 0, "A", &replicas, &replicas, 2);
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&newer)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "A".to_string(),
                epoch: 2,
            }
        );
        let runner = fx.follower_factory.handles.lock()[0].clone();
        fx.manager.apply_assignment(newer).await;
        assert!(
            !runner.stopped.load(Ordering::SeqCst),
            "the ledger row for the adopted term must not rebuild the live follower stream"
        );
    }

    /// Review finding 3: a Hello must come from the leader it names. One
    /// relayed or spoofed by another peer can neither fence this node nor be
    /// followed — refused before it touches the registry.
    #[tokio::test]
    async fn a_hello_naming_a_leader_other_than_its_sender_is_refused() {
        let fx = build("C");
        fx.manager
            .apply_assignment(assignment(
                "org",
                "orders",
                0,
                "C",
                &["B", "C"],
                &["B", "C"],
                1,
            ))
            .await;
        let own = fx.leader_factory.handles.lock()[0].clone();
        let ack = hello_roundtrip_from(
            Arc::clone(&fx.manager),
            "E",
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
        assert_eq!(ack.reject, Some(ReplReject::LeaderIdentityMismatch));
        assert!(
            !own.stopped.load(Ordering::SeqCst),
            "no fence on a spoofed claim"
        );
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 1 }
        ));
    }

    /// Review finding 3: a claim far past this node's ledger row is refused
    /// — neither fenced to nor adopted — so a bogus huge epoch cannot pin
    /// the entry above every real assignment. Within the lead it is taken.
    #[tokio::test]
    async fn a_hello_too_far_ahead_of_the_ledger_is_refused() {
        let fx = build("F");
        let replicas = ["A", "F"];
        let row = assignment("org", "orders", 0, "A", &replicas, &replicas, 3);
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;

        let far = 3 + MAX_HELLO_EPOCH_LEAD + 1;
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org", "orders", 0, "A", &replicas, &replicas, far,
            )),
        )
        .await;
        assert_eq!(
            ack.reject,
            Some(ReplReject::EpochAheadOfLedger { ledger_epoch: 3 })
        );
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "A".to_string(),
                epoch: 3,
            },
            "the refused claim must not be adopted"
        );

        let near = 3 + MAX_HELLO_EPOCH_LEAD;
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org", "orders", 0, "A", &replicas, &replicas, near,
            )),
        )
        .await;
        assert!(ack.accepted, "{:?}", ack.reject);
    }

    /// Review finding 5: at an equal epoch the materializer admits only the
    /// lower leader id, so a follower of B must refuse C's equal-epoch Hello
    /// rather than stream from it (the adoption rule already refused to
    /// adopt it, leaving `role()` and the live stream disagreeing).
    #[tokio::test]
    async fn an_equal_epoch_hello_from_a_higher_ranked_leader_is_refused() {
        let fx = build("F");
        let replicas = ["B", "C", "F"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "B", &replicas, &replicas, 2))
            .await;
        let ack = hello_roundtrip(
            Arc::clone(&fx.manager),
            hello_from(&assignment(
                "org", "orders", 0, "C", &replicas, &replicas, 2,
            )),
        )
        .await;
        assert_eq!(ack.reject, Some(ReplReject::StaleEpoch { have: 2 }));
        assert!(
            fx.follower_factory.handles.lock().is_empty(),
            "no stream from C"
        );

        // The leader it follows, and a lower-ranked one, are still accepted.
        for leader in ["B", "A"] {
            let mut replicas: Vec<&str> = replicas.to_vec();
            replicas.push("A");
            let ack = hello_roundtrip(
                Arc::clone(&fx.manager),
                hello_from(&assignment(
                    "org", "orders", 0, leader, &replicas, &replicas, 2,
                )),
            )
            .await;
            assert!(ack.accepted, "{leader}: {:?}", ack.reject);
        }
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
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Idle)
        ));
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

    /// The topic was deleted while this follower's lease ran out: its own
    /// proposal is refused by the incarnation gate, so the ledger has no row
    /// for the partition. The election must not leave a leader of a topic
    /// that no longer exists — the partition is dropped instead.
    #[tokio::test]
    async fn promotion_is_abandoned_when_the_topic_was_deleted_meanwhile() {
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
        fx.assignments.delete_topic("org", "orders", 9);

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
        fx.manager.run_election(key.clone()).await;

        assert!(
            fx.leader_factory.spawned.lock().is_empty(),
            "a promotion for a deleted topic must not spawn a leader handle"
        );
        assert!(fx.assignments.stored(&key).is_none());
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Unavailable {
                reason: UnavailableReason::NoAssignment
            },
            "the partition of a deleted topic must be dropped, not led"
        );
    }

    /// The topic was deleted and re-created while the lease ran out: the
    /// ledger now places the NEW incarnation. The old-incarnation election
    /// must not promote; the node adopts the stored placement instead.
    #[tokio::test]
    async fn promotion_adopts_the_new_incarnation_when_the_topic_was_recreated_meanwhile() {
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
        fx.assignments.delete_topic("org", "orders", 9);
        let mut recreated = assignment(
            "org",
            "orders",
            0,
            "f2",
            &["f2", "f1", "l"],
            &["f2", "f1", "l"],
            1,
        );
        recreated.topic_generation = 9;
        fx.assignments.seed(recreated);

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

        assert!(
            fx.leader_factory.spawned.lock().is_empty(),
            "an old-incarnation promotion must not spawn a leader handle"
        );
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "f2".to_string(),
                epoch: 1,
            },
            "the node must follow the re-created topic's placement"
        );
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Idle)
        ));
    }

    /// The assignment poll can apply a promotion's own row before the
    /// promotion installs its handle. The promotion must then keep the handle
    /// the poll attached for the same term — its replica streams may already
    /// be up — stop its own, and still deliver the election's truncate target.
    #[tokio::test]
    async fn a_promotion_keeps_the_leader_handle_the_poll_already_installed_for_its_term() {
        let fx = build("f1");
        let key: PartitionKey = ("org".to_string(), "orders".to_string(), 0u32);
        let base = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            5,
        );
        fx.manager.apply_assignment(base).await;
        let promoted = assignment(
            "org",
            "orders",
            0,
            "f1",
            &["l", "f1", "f2"],
            &["f1", "f2"],
            6,
        );
        fx.assignments.seed(promoted.clone());
        fx.manager.apply_assignment(promoted.clone()).await;
        assert_eq!(fx.leader_factory.handles.lock().len(), 1);

        fx.manager
            .execute_promotion_actions(
                &key,
                &promoted,
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
            PartitionRole::Leader { epoch: 6 }
        );
        let handles = fx.leader_factory.handles.lock();
        assert_eq!(
            handles.len(),
            2,
            "the promotion spawns its own handle first"
        );
        assert!(
            !handles[0].stopped.load(Ordering::SeqCst),
            "the handle the poll installed for this term must keep serving"
        );
        assert!(
            handles[1].stopped.load(Ordering::SeqCst),
            "the promotion's surplus handle must be stopped, not leaked"
        );
        // M1: only the serving handle opens leader writes. A spare that
        // opened them too would keep them open after the serving one is
        // fenced and stopped.
        assert!(handles[0].writes_opened.load(Ordering::SeqCst));
        assert!(
            !handles[1].writes_opened.load(Ordering::SeqCst),
            "a spare must never open leader writes"
        );
        assert_eq!(
            *handles[0].truncated.lock(),
            vec![("f2".to_string(), 5)],
            "the election's truncate target must reach the serving handle"
        );
    }

    /// A peer's Hello at the same epoch but from a lower node id was accepted
    /// while this node's election ran, and the peer's row has not reached the
    /// ledger here yet, so the settle check still reads this node's own
    /// proposal. The promotion must yield to the claim the node already
    /// follows instead of turning that entry into a second leader — and the
    /// entry must be able to elect again afterwards: left in `Promoted`,
    /// `check_leases` would never start another election for it.
    #[tokio::test]
    async fn a_promotion_yields_to_an_outranking_claim_the_node_already_follows() {
        let fx = build("f1");
        let key: PartitionKey = ("org".to_string(), "orders".to_string(), 0u32);
        let base = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "g"],
            &["l", "f1", "g"],
            5,
        );
        fx.assignments.seed(base.clone());
        fx.manager.apply_assignment(base).await;
        fx.transport.set_script(
            "g",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["g".to_string()]);
        }
        // What `accept_hello` does for an epoch-6 Hello from `e` (a lower
        // id than this node, so it wins the equal-epoch tie), landing while
        // this node's own epoch-6 proposal is out. `g` answers the LeoQuery
        // and loses the election's own tie-break to this node.
        let manager = Arc::clone(&fx.manager);
        let hook_key = key.clone();
        *fx.assignments.on_propose.lock() = Some(Box::new(move || {
            if let Some(mut e) = manager.registry.get_mut(&hook_key) {
                e.assignment.leader_node_id = "e".to_string();
                e.assignment.leader_epoch = 6;
            }
        }));

        fx.manager.run_election(key.clone()).await;

        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "e".to_string(),
                epoch: 6,
            },
            "the node must keep following the claim that outranks its promotion"
        );
        assert!(
            fx.leader_factory
                .handles
                .lock()
                .iter()
                .all(|h| h.stopped.load(Ordering::SeqCst)),
            "the yielded promotion's leader handle must be stopped"
        );
        assert!(
            matches!(
                fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
                Some(PromotionState::Idle)
            ),
            "a yielded promotion must leave the entry able to elect again"
        );
    }

    /// A leader still serving a deleted incarnation of the topic at epoch 3
    /// dials a follower of the re-created one at epoch 1. By epoch alone its
    /// claim outranks; it must be refused, and the follower must stay with
    /// the incarnation it holds rather than adopt the dead claim.
    #[tokio::test]
    async fn a_hello_from_another_topic_incarnation_is_refused_by_a_follower() {
        let fx = build("f");
        let mut current = assignment("org", "orders", 0, "x", &["x", "f"], &["x", "f"], 1);
        current.topic_generation = 9;
        fx.assignments.seed(current.clone());
        fx.manager.apply_assignment(current).await;

        let mut dead = assignment("org", "orders", 0, "old", &["old", "f"], &["old", "f"], 3);
        dead.topic_generation = 2;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&dead)).await;

        assert!(!ack.accepted);
        assert_eq!(
            ack.reject,
            Some(ReplReject::TopicIncarnationMismatch { theirs: 2, ours: 9 })
        );
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "x".to_string(),
                epoch: 1,
            }
        );
    }

    /// The same dead claim reaching the new incarnation's LEADER must not
    /// fence it: the fence is a two-leaders-of-one-partition rule, and a
    /// leader of another incarnation is not a leader of this partition.
    #[tokio::test]
    async fn a_hello_from_another_topic_incarnation_does_not_fence_a_leader() {
        let fx = build("l");
        let mut current = assignment("org", "orders", 0, "l", &["l", "z"], &["l", "z"], 1);
        current.topic_generation = 9;
        fx.assignments.seed(current.clone());
        fx.manager.apply_assignment(current).await;
        let leader_handle = fx.leader_factory.handles.lock()[0].clone();

        let mut dead = assignment("org", "orders", 0, "z", &["z", "l"], &["z", "l"], 3);
        dead.topic_generation = 2;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&dead)).await;

        assert!(!ack.accepted);
        assert!(!leader_handle.stopped.load(Ordering::SeqCst));
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 1 }
        );
    }

    /// A leader on a build before incarnations sends none: its Hello is
    /// judged by epoch alone, as that build's own followers would judge it,
    /// so a fleet keeps replicating while it is being upgraded.
    #[tokio::test]
    async fn a_hello_without_an_incarnation_is_judged_by_epoch_alone() {
        let fx = build("f");
        let mut current = assignment("org", "orders", 0, "x", &["x", "f"], &["x", "f"], 1);
        current.topic_generation = 9;
        fx.assignments.seed(current.clone());
        fx.manager.apply_assignment(current.clone()).await;

        let mut hello = hello_from(&current);
        hello.topic_generation = None;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello).await;
        assert!(ack.accepted, "{:?}", ack.reject);
    }

    /// An election that ends without a majority must not end this node's
    /// candidacy: once the lease check fires again, it stands again. Left
    /// `Abandoned`, `check_leases` — which starts only from an idle entry —
    /// never picked the partition up again, and a partition whose every
    /// candidate lost one round stayed leaderless.
    #[tokio::test]
    async fn after_an_abandoned_election_the_lease_check_stands_again() {
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
        fx.manager.apply_assignment(a.clone()).await;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        fx.follower_factory.handles.lock()[0]
            .lease_expired
            .store(true, Ordering::SeqCst);
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );

        // Nobody acks: the majority window runs out.
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.run_election(key.clone()).await;
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned { .. })
        ));

        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }
        fx.manager.check_leases().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 },
            "the lease check must start a new election after an abandoned one"
        );
    }

    /// Outlasts `FAKE_LEADER_LEASE`: a follower no leader has dialed is past
    /// its lease after it.
    const PAST_THE_LEASE: Duration = Duration::from_millis(300);

    /// The second half of a lost candidacy: the winner's row has landed, so
    /// this node follows the winner — but a follower stream, and with it a
    /// lease that can expire, exists only once the winner dials in. The
    /// winner dies before that dial; the old leader `l` is back and acks.
    /// The loser must stand again and lead, not stay a follower of nobody.
    async fn loser_stands_again_after_the_winner_dies(
        fx: &Fixture,
        winner_row: PartitionAssignment,
    ) {
        let winner = winner_row.leader_node_id.clone();
        let won_epoch = winner_row.leader_epoch;
        fx.assignments.seed(winner_row.clone());
        fx.manager.apply_assignment(winner_row).await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: winner.clone(),
                epoch: won_epoch,
            }
        );

        fx.transport.set_script(&winner, PeerScript::Unreachable);
        fx.transport.set_script(
            "l",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: false,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["l".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader {
                epoch: won_epoch + 1
            },
            "a node that lost to a winner which died before dialing it must stand again"
        );
    }

    /// Candidacy lost to a replica that is further ahead.
    #[tokio::test]
    async fn a_node_that_lost_to_a_longer_log_stands_again_when_the_winner_dies() {
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
        fx.manager.apply_assignment(a.clone()).await;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        fx.follower_factory.handles.lock()[0]
            .lease_expired
            .store(true, Ordering::SeqCst);
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 10,
                in_isr: true,
            },
        );

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.check_leases().await;
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned {
                reason: election::AbandonReason::LostElection { .. }
            })
        ));

        let won = assignment(
            "org",
            "orders",
            0,
            "f2",
            &["l", "f1", "f2"],
            &["f1", "f2"],
            2,
        );
        loser_stands_again_after_the_winner_dies(&fx, won).await;
    }

    /// Candidacy that won the LeoQuery round but never reached a majority;
    /// a lower-id replica's promotion at the same epoch then settled it.
    #[tokio::test]
    async fn a_node_that_found_no_majority_stands_again_when_the_winner_dies() {
        let fx = build("f1");
        let a = assignment(
            "org",
            "orders",
            0,
            "l",
            &["f0", "f1", "l"],
            &["f0", "f1", "l"],
            1,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a.clone()).await;
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        fx.follower_factory.handles.lock()[0]
            .lease_expired
            .store(true, Ordering::SeqCst);
        // Enough logs compared to propose (a majority answered), but nobody
        // acks the proposal.
        fx.transport.set_script(
            "l",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.check_leases().await;
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned {
                reason: election::AbandonReason::NoMajority
            })
        ));

        let won = assignment(
            "org",
            "orders",
            0,
            "f0",
            &["f0", "f1", "l"],
            &["f0", "f1"],
            2,
        );
        loser_stands_again_after_the_winner_dies(&fx, won).await;
    }

    /// A candidate no leader has dialed stands with its local log's
    /// position. Read as zero, a longer local log lost to a shorter peer —
    /// and a winning candidate would truncate every replica ahead of zero.
    #[tokio::test]
    async fn a_runnerless_candidate_stands_with_its_local_log_position() {
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
        fx.follower_factory.local_leo.store(20, Ordering::SeqCst);
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 10,
                in_isr: true,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }

        fx.manager
            .run_election(("org".to_string(), "orders".to_string(), 0u32))
            .await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 },
            "the longer local log must win over the shorter peer"
        );
        let handles = fx.leader_factory.handles.lock();
        assert!(
            handles.iter().all(|h| h.truncated.lock().is_empty()),
            "a replica behind the candidate's log must not be truncated"
        );
    }

    /// A reply from a follower (`leading: false`) or the live leader of
    /// `epoch` (`leading: true`, which never stands itself).
    fn leo_reply(leo: u64, hw: u64, epoch: u32, log_epoch: u32, leading: bool) -> PeerScript {
        PeerScript::Reply(ReplLeoReply {
            leo,
            hw,
            leader_epoch: epoch,
            in_isr: true,
            log_epoch: Some(log_epoch),
            leading,
            ineligible: leading,
            committed: None,
            leader_alive: false,
        })
    }

    /// The crashed-leader rejoin: `x` led epoch 1 and crashed holding a tail
    /// no majority had (leo 120, hw 90); `y` was elected at epoch 2 and
    /// committed records up to 100 with `z`. `x` restarts from its own row,
    /// leads, is refused with epoch 2 and steps down without the ledger
    /// naming `y` yet. Standing with its raw log it outranked the newer term
    /// by offset, won epoch 3 and replaced `y`'s committed records.
    #[tokio::test]
    async fn a_leader_proved_stale_does_not_stand_with_its_unreplicated_tail() {
        let fx = build("x");
        let a = assignment(
            "org",
            "orders",
            0,
            "x",
            &["x", "y", "z"],
            &["x", "y", "z"],
            1,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        fx.follower_factory.local_leo.store(120, Ordering::SeqCst);
        fx.follower_factory.local_hw.store(90, Ordering::SeqCst);
        *fx.follower_factory.local_epoch.lock() = Some(1);
        fx.leader_factory.handles.lock()[0].note_stale_epoch(2);
        fx.manager.check_stale_leadership().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: String::new(),
                epoch: 2,
            }
        );

        fx.transport
            .set_script("y", leo_reply(100, 100, 2, 2, true));
        fx.transport
            .set_script("z", leo_reply(100, 100, 2, 2, false));
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["z".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert_eq!(
            *fx.follower_factory.cuts.lock(),
            1,
            "the step-down cuts the tail no majority held"
        );
        assert_eq!(fx.follower_factory.local_leo.load(Ordering::SeqCst), 90);
        assert_eq!(
            fx.follower_factory.events.lock()[0],
            "fence:2",
            "the step-down fences the old term before anything else"
        );
        assert!(
            !fx.manager.registry.get(&key).unwrap().unreconciled,
            "cut, the log is a prefix of every later term and may stand"
        );
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: String::new(),
                epoch: 2,
            },
            "an epoch-1 log never wins over the epoch-2 term"
        );
        assert_eq!(
            fx.assignments.stored(&key).map(|r| r.leader_epoch),
            Some(1),
            "no proposal may leave this node"
        );
    }

    /// A restarted node whose ledger already names the newer term's leader
    /// is not marked stale-stepped-down, and that leader has died. Its log
    /// still ends in the old term's tail; a live peer holds the newer term.
    /// The newer term wins even though it is shorter.
    #[tokio::test]
    async fn an_old_term_log_loses_to_a_shorter_newer_term_log() {
        let fx = build("x");
        let row = assignment(
            "org",
            "orders",
            0,
            "y",
            &["x", "y", "z"],
            &["x", "y", "z"],
            2,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        fx.follower_factory.local_leo.store(120, Ordering::SeqCst);
        fx.follower_factory.local_hw.store(90, Ordering::SeqCst);
        *fx.follower_factory.local_epoch.lock() = Some(1);
        fx.transport
            .set_script("z", leo_reply(100, 100, 2, 2, false));
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["z".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(
            matches!(
                fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
                Some(PromotionState::Abandoned {
                    reason: election::AbandonReason::LostElection {
                        winner: Some(ref w)
                    }
                }) if w == "z"
            ),
            "the newer term must win the election"
        );
        assert_eq!(fx.assignments.stored(&key).map(|r| r.leader_epoch), Some(2));
    }

    /// A lower-id follower restarts under a healthy leader. Its entry has no
    /// runner until the leader redials it; a live leader answering as leader
    /// of the current epoch must hold its election off — on a caught-up
    /// partition the tie-break would otherwise hand it the leadership.
    #[tokio::test]
    async fn a_restarted_follower_does_not_elect_over_a_live_leader() {
        let fx = build("a");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["a", "l", "m"],
            &["a", "l", "m"],
            3,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        fx.follower_factory.local_leo.store(50, Ordering::SeqCst);
        fx.transport.set_script("l", leo_reply(50, 50, 3, 3, true));
        fx.transport.set_script(
            "m",
            PeerScript::LeoReply {
                leo: 50,
                in_isr: true,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["m".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;

        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower {
                leader_node_id: "l".to_string(),
                epoch: 3,
            }
        );
        assert_eq!(fx.assignments.stored(&key).map(|r| r.leader_epoch), Some(3));
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Idle)
        ));
    }

    /// A leader that stopped hearing its quorum stops holding candidates
    /// off: it answers `leading: false`, so a hung-but-responsive leader can
    /// still be replaced.
    #[tokio::test]
    async fn a_leader_without_its_quorum_lease_does_not_answer_as_leading() {
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
        let query = ReplFrame::LeoQuery(ReplLeoQuery {
            instance_id: "tentabus-00000001".into(),
            org_id: "org".into(),
            topic: "orders".into(),
            partition: 0,
            known_epoch: 4,
        });
        let leading = |reply: ReplFrame| match reply {
            ReplFrame::LeoReply(r) => r.leading,
            other => panic!("expected LeoReply, got {other:?}"),
        };
        assert!(leading(
            accept_roundtrip(Arc::clone(&fx.manager), &query).await
        ));
        fx.leader_factory.handles.lock()[0]
            .quorum_lease
            .store(false, Ordering::SeqCst);
        assert!(!leading(
            accept_roundtrip(Arc::clone(&fx.manager), &query).await
        ));
    }

    /// B2 at the manager: `l` lost its quorum lease, so it neither leads
    /// nor stands, but its log is the best — committed past the candidate's
    /// `leo`. The candidate must not win (its ledger ack alone is a
    /// majority) and cut `l`'s committed records; it abandons instead.
    #[tokio::test]
    async fn a_candidate_never_wins_over_a_leader_holding_committed_records_it_lacks() {
        let fx = build("f1");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            2,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        fx.follower_factory.local_leo.store(90, Ordering::SeqCst);
        fx.transport.set_script(
            "l",
            PeerScript::Reply(ReplLeoReply {
                leo: 100,
                hw: 100,
                leader_epoch: 2,
                in_isr: true,
                log_epoch: Some(2),
                leading: false,
                ineligible: true,
                committed: Some(100),
                leader_alive: false,
            }),
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["l".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned {
                reason: election::AbandonReason::Outranked { ref by }
            }) if by == "l"
        ));
        assert_eq!(fx.assignments.stored(&key).map(|r| r.leader_epoch), Some(2));
    }

    /// A leader no quorum has acknowledged for a whole leader lease steps
    /// down — and stays down when the poll re-applies the row naming it —
    /// so it can stand as an ordinary follower. With the best log it is
    /// elected again at a newer epoch; without this, a hung-but-responsive
    /// leader holding the best log stalled every candidate for good.
    #[tokio::test]
    async fn a_leader_without_its_quorum_lease_steps_down_and_can_be_elected_again() {
        let fx = build("l");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            2,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row.clone()).await;
        fx.leader_factory.handles.lock()[0]
            .quorum_lease
            .store(false, Ordering::SeqCst);

        fx.manager.check_quorum_leases().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 },
            "one lapse shorter than the lease is not a step-down"
        );
        tokio::time::sleep(FAKE_LEADER_LEASE * 2).await;
        fx.manager.check_quorum_leases().await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower { epoch: 2, .. }
        ));
        assert!(fx.leader_factory.handles.lock()[0]
            .stopped
            .load(Ordering::SeqCst));

        fx.manager.apply_assignment(row).await;
        assert!(
            matches!(
                fx.manager.role("org", "orders", 0),
                PartitionRole::Follower { epoch: 2, .. }
            ),
            "the poll must not restore the leadership just given up"
        );

        fx.follower_factory.local_leo.store(100, Ordering::SeqCst);
        fx.transport.set_script(
            "f1",
            PeerScript::LeoReply {
                leo: 90,
                in_isr: true,
            },
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f1".to_string()]);
        }
        fx.manager.check_leases().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 3 }
        );
    }

    /// B1, scenario B: `x` accepted `y`'s epoch-2 Hello — a runner is
    /// attached — but its log still ends in an epoch-1 tail (leo 120,
    /// committed 90); `y` dies before reconciling it. Standing with the
    /// stamped epoch, `x` ranked (2, 120) and beat `z`'s epoch-2 records at
    /// 100; with the epoch its records were written under it loses.
    #[tokio::test]
    async fn a_follower_ranks_by_the_epoch_its_records_were_written_under() {
        let fx = build("x");
        let row = assignment(
            "org",
            "orders",
            0,
            "y",
            &["x", "y", "z"],
            &["x", "y", "z"],
            2,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row.clone()).await;
        *fx.follower_factory.local_epoch.lock() = Some(1);
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&row)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        {
            let handles = fx.follower_factory.handles.lock();
            handles[0].leo.store(120, Ordering::SeqCst);
            handles[0].committed.store(90, Ordering::SeqCst);
            handles[0].lease_expired.store(true, Ordering::SeqCst);
        }
        fx.transport
            .set_script("z", leo_reply(100, 100, 2, 2, false));
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["z".to_string()]);
        }
        fx.manager.check_leases().await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned {
                reason: election::AbandonReason::LostElection { winner: Some(ref w) }
            }) if w == "z"
        ));
    }

    /// M1: a follower whose stream from the live leader is within its lease
    /// says so, and a candidate that hears it defers — the leader is alive,
    /// only not reaching the candidate (RF=2 after a restart, an isolated
    /// follower).
    #[tokio::test]
    async fn a_candidate_defers_to_a_peer_that_still_follows_a_live_leader() {
        let follower = build("f2");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["a", "l", "f2"],
            &["a", "l", "f2"],
            3,
        );
        follower.manager.apply_assignment(row.clone()).await;
        let ack = hello_roundtrip(Arc::clone(&follower.manager), hello_from(&row)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        let query = ReplFrame::LeoQuery(ReplLeoQuery {
            instance_id: "tentabus-00000001".into(),
            org_id: "org".into(),
            topic: "orders".into(),
            partition: 0,
            known_epoch: 3,
        });
        let alive = |reply: ReplFrame| match reply {
            ReplFrame::LeoReply(r) => r.leader_alive,
            other => panic!("expected LeoReply, got {other:?}"),
        };
        assert!(alive(
            accept_roundtrip(Arc::clone(&follower.manager), &query).await
        ));
        follower.follower_factory.handles.lock()[0]
            .lease_expired
            .store(true, Ordering::SeqCst);
        assert!(!alive(
            accept_roundtrip(Arc::clone(&follower.manager), &query).await
        ));

        let fx = build("a");
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        fx.transport.set_script(
            "f2",
            PeerScript::Reply(ReplLeoReply {
                leo: 0,
                hw: 0,
                leader_epoch: 3,
                in_isr: true,
                log_epoch: Some(3),
                leading: false,
                ineligible: false,
                committed: Some(0),
                leader_alive: true,
            }),
        );
        for raw in 1u8..=8 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert_eq!(fx.assignments.stored(&key).map(|r| r.leader_epoch), Some(3));
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Idle)
        ));
    }

    /// X4: a stepped-down leader whose cut has not happened yet accepts the
    /// new leader's Hello. That leader reconciles the log from here — by
    /// the epochs its records were written in — so the step-down cut must
    /// never run under the stream: it would drop records the leader may
    /// already count as acknowledged here.
    #[tokio::test]
    async fn a_step_down_cut_never_runs_under_an_accepted_stream() {
        let fx = build("x");
        let a = assignment(
            "org",
            "orders",
            0,
            "x",
            &["x", "y", "z"],
            &["x", "y", "z"],
            1,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        fx.follower_factory
            .local_read_fails
            .store(true, Ordering::SeqCst);
        fx.leader_factory.handles.lock()[0].note_stale_epoch(2);
        fx.manager.check_stale_leadership().await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(fx.manager.registry.get(&key).unwrap().unreconciled);

        let y = assignment(
            "org",
            "orders",
            0,
            "y",
            &["x", "y", "z"],
            &["x", "y", "z"],
            2,
        );
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&y)).await;
        assert!(ack.accepted, "{:?}", ack.reject);
        assert!(!fx.manager.registry.get(&key).unwrap().unreconciled);

        fx.follower_factory
            .local_read_fails
            .store(false, Ordering::SeqCst);
        fx.manager.check_leases().await;
        assert_eq!(
            *fx.follower_factory.cuts.lock(),
            0,
            "no cut under a live stream"
        );
    }

    /// The reverse order: a Hello that arrives while the step-down cut runs
    /// is refused, so no stream can be fed past a cut still in progress.
    #[tokio::test]
    async fn a_hello_during_the_step_down_cut_is_refused() {
        let fx = build("x");
        let a = assignment(
            "org",
            "orders",
            0,
            "y",
            &["x", "y", "z"],
            &["x", "y", "z"],
            2,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a.clone()).await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        {
            let mut entry = fx.manager.registry.get_mut(&key).unwrap();
            entry.unreconciled = true;
            entry.cutting = true;
        }
        let ack = hello_roundtrip(Arc::clone(&fx.manager), hello_from(&a)).await;
        assert!(!ack.accepted);
        assert_eq!(ack.reject, Some(ReplReject::Reconciling));
        assert!(fx.follower_factory.handles.lock().is_empty());
    }

    /// A crashed incumbent sits first in `replicas` and its dial never
    /// completes. Asked one after another, it burned the whole LeoQuery
    /// budget, the live peer was never asked, and with a majority of logs
    /// required no candidate could ever win.
    #[tokio::test]
    async fn a_hanging_dial_to_the_dead_leader_does_not_starve_the_live_peers() {
        let fx = build("f1");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f1", "f2"],
            &["l", "f1", "f2"],
            1,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        fx.transport.set_script("l", PeerScript::Hang);
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
        fx.manager
            .run_election(("org".to_string(), "orders".to_string(), 0u32))
            .await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 2 }
        );
    }

    /// RF=2, owner decision (availability): the follower is gone and the
    /// live ISR is the leader alone. `acks=leader` and `acks=all` writes are
    /// still accepted and complete; `acks=quorum` keeps its contract — a
    /// majority of two is two — and is refused.
    #[tokio::test]
    async fn an_rf2_leader_keeps_accepting_writes_after_losing_its_follower() {
        let fx = build("l");
        let a = assignment("org", "orders", 0, "l", &["l", "f"], &["l", "f"], 1);
        fx.manager.apply_assignment(a).await;
        fx.leader_factory.handles.lock()[0].set_isr(vec!["l".to_string()]);

        assert!(fx
            .manager
            .preflight("org", "orders", 0, Acks::Leader)
            .is_ok());
        assert!(fx.manager.preflight("org", "orders", 0, Acks::All).is_ok());
        assert!(matches!(
            fx.manager.preflight("org", "orders", 0, Acks::Quorum),
            Err(ReplError::NotEnoughReplicas {
                isr: 1,
                required: 2,
                ..
            })
        ));
        let manager = fx.manager.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            manager.await_acks("org", "orders", 0, 1, Acks::All, Duration::from_secs(5))
        })
        .await
        .unwrap()
        .expect("acks=all on the surviving ISR");
        assert!(outcome.acked_nodes >= outcome.required);
        assert_eq!(outcome.required, 1);
    }

    /// RF=2: the leader dies; nobody answers and nobody acks the proposal.
    /// The surviving in-sync follower takes over alone.
    #[tokio::test]
    async fn an_rf2_follower_takes_over_when_the_leader_dies() {
        let fx = build("f");
        let row = assignment("org", "orders", 0, "l", &["l", "f"], &["l", "f"], 3);
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;
        assert_eq!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { epoch: 4 }
        );
    }

    /// RF=3 keeps its rules: the same lone survivor, one of three, neither
    /// proposes nor leads.
    #[tokio::test]
    async fn an_rf3_lone_survivor_still_does_not_take_over() {
        let fx = build("f");
        let row = assignment(
            "org",
            "orders",
            0,
            "l",
            &["l", "f", "g"],
            &["l", "f", "g"],
            3,
        );
        fx.assignments.seed(row.clone());
        fx.manager.apply_assignment(row).await;
        tokio::time::sleep(PAST_THE_LEASE).await;
        fx.manager.check_leases().await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower { epoch: 3, .. }
        ));
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        assert!(matches!(
            fx.manager.registry.get(&key).map(|e| e.promotion.clone()),
            Some(PromotionState::Abandoned {
                reason: election::AbandonReason::TooFewReplies { .. }
            })
        ));
    }

    /// `acks=all` follows the live ISR during the wait, not only at its
    /// start: `f2` never acks and drops out of the ISR mid-wait, and the
    /// write every remaining replica holds completes instead of timing out.
    #[tokio::test]
    async fn an_acks_all_wait_follows_an_isr_that_shrinks_mid_wait() {
        let fx = build("l");
        let replicas = ["l", "f1", "f2"];
        fx.manager
            .apply_assignment(assignment("org", "orders", 0, "l", &replicas, &replicas, 1))
            .await;
        let handle = Arc::clone(&fx.leader_factory.handles.lock()[0]);
        // Like the real handle: holds until the wait's own budget runs out.
        *handle.ack_hold.lock() = Duration::from_secs(30);
        *handle.acked_override.lock() = Some(2);

        let manager = fx.manager.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome =
                manager.await_acks("org", "orders", 0, 1, Acks::All, Duration::from_secs(5));
            (outcome, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.set_isr(vec!["l".to_string(), "f1".to_string()]);

        let (outcome, elapsed) = waiter.join().unwrap();
        let outcome = outcome.expect("acks=all");
        assert!(
            outcome.acked_nodes >= outcome.required,
            "{outcome:?} after {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
    }

    /// L2: a step-down stops the leader handle outside the registry guard —
    /// `stop()` makes blocking round trips to the partition's writer, and
    /// holding a shard's write guard across them stalls every other user of
    /// that shard (`answer_leo_query`, accepts, the lease tick).
    #[tokio::test]
    async fn a_step_down_stops_the_leader_handle_outside_the_registry_guard() {
        let fx = build("x");
        let a = assignment(
            "org",
            "orders",
            0,
            "x",
            &["x", "y", "z"],
            &["x", "y", "z"],
            1,
        );
        fx.assignments.seed(a.clone());
        fx.manager.apply_assignment(a).await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        let locked = Arc::new(AtomicBool::new(false));
        {
            let handle = Arc::clone(&fx.leader_factory.handles.lock()[0]);
            let manager = Arc::clone(&fx.manager);
            let locked = Arc::clone(&locked);
            let key = key.clone();
            *handle.on_stop.lock() = Some(Box::new(move || {
                locked.store(manager.registry.try_get(&key).is_locked(), Ordering::SeqCst);
            }));
            handle.note_stale_epoch(2);
        }
        fx.manager.check_stale_leadership().await;
        assert!(fx.leader_factory.handles.lock()[0]
            .stopped
            .load(Ordering::SeqCst));
        assert!(
            !locked.load(Ordering::SeqCst),
            "stop() ran under the registry entry's guard"
        );
    }

    /// Arms the factory so every spawned handle's `claim_term` records
    /// whether `key`'s registry entry was locked while it ran.
    fn observe_claims(fx: &Fixture, key: &PartitionKey) -> Arc<AtomicBool> {
        let locked = Arc::new(AtomicBool::new(false));
        let manager = Arc::downgrade(&fx.manager);
        let seen = Arc::clone(&locked);
        let key = key.clone();
        *fx.leader_factory.claim_hook.lock() = Some(Arc::new(move || {
            if let Some(manager) = manager.upgrade() {
                if manager.registry.try_get(&key).is_locked() {
                    seen.store(true, Ordering::SeqCst);
                }
            }
        }));
        locked
    }

    /// A leader handle installed by the assignment path claims its term, and
    /// not under the registry guard it was installed under: the claim is a
    /// write to the partition's writer, queued behind its appends and ending
    /// in an fsync, and the guard's shard serves every other partition on
    /// it, `accept_hello` and the lease tick.
    #[tokio::test]
    async fn an_installed_leader_claims_its_term_outside_the_registry_guard() {
        let fx = build("x");
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        let locked = observe_claims(&fx, &key);
        // Following first, so the leader handle is attached to the entry the
        // call re-stamps rather than inserted as a new one.
        let follows = assignment(
            "org",
            "orders",
            0,
            "y",
            &["x", "y", "z"],
            &["x", "y", "z"],
            1,
        );
        fx.manager.apply_assignment(follows).await;
        let leads = assignment(
            "org",
            "orders",
            0,
            "x",
            &["x", "y", "z"],
            &["x", "y", "z"],
            2,
        );
        fx.assignments.seed(leads.clone());
        fx.manager.apply_assignment(leads).await;
        let handle = Arc::clone(&fx.leader_factory.handles.lock()[0]);
        assert!(handle.writes_opened.load(Ordering::SeqCst));
        assert!(
            handle.claimed.load(Ordering::SeqCst),
            "the term was never claimed"
        );
        assert!(
            !locked.load(Ordering::SeqCst),
            "claim_term ran under the registry entry's guard"
        );
    }

    /// The same for a handle a won election installs.
    #[tokio::test]
    async fn a_promoted_leader_claims_its_term_outside_the_registry_guard() {
        let fx = build("f1");
        let key = ("org".to_string(), "orders".to_string(), 0u32);
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
        fx.transport.set_script(
            "f2",
            PeerScript::LeoReply {
                leo: 0,
                in_isr: true,
            },
        );
        for raw in 1u8..=4 {
            fx.ledger
                .set_acked(OperationId::from_hash([raw; 32]), vec!["f2".to_string()]);
        }
        let locked = observe_claims(&fx, &key);
        fx.manager.run_election(key.clone()).await;
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Leader { .. }
        ));
        let serving: Vec<_> = fx
            .leader_factory
            .handles
            .lock()
            .iter()
            .filter(|h| h.writes_opened.load(Ordering::SeqCst))
            .cloned()
            .collect();
        assert!(!serving.is_empty(), "no handle was installed as serving");
        assert!(
            serving.iter().all(|h| h.claimed.load(Ordering::SeqCst)),
            "a serving handle never claimed its term"
        );
        assert!(
            !locked.load(Ordering::SeqCst),
            "claim_term ran under the registry entry's guard"
        );
    }

    /// (a): two `apply_assignment` calls for one key with no entry yet — one
    /// installing this node as leader, one as a follower of a newer term.
    /// Whichever inserts second must not overwrite the other's entry and
    /// drop a leader handle without stopping it.
    #[tokio::test]
    async fn a_follower_insert_never_drops_a_concurrently_installed_leader_handle() {
        let fx = build("x");
        let leads = assignment("org", "orders", 0, "x", &["x", "y"], &["x", "y"], 1);
        fx.manager.apply_assignment(leads).await;
        let installed = Arc::clone(&fx.leader_factory.handles.lock()[0]);
        // The racing call saw no entry; by the time it installs its
        // follower entry, the leader entry above is there.
        let follows = assignment("org", "orders", 0, "y", &["x", "y"], &["x", "y"], 2);
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        fx.manager.install_follower_entry(key, follows);
        assert!(matches!(
            fx.manager.role("org", "orders", 0),
            PartitionRole::Follower { epoch: 2, .. }
        ));
        assert!(
            installed.stopped.load(Ordering::SeqCst),
            "the displaced leader handle must be stopped, not dropped"
        );
    }

    #[tokio::test]
    async fn a_follower_insert_keeps_a_runner_already_serving_the_same_term() {
        let fx = build("x");
        let follows = assignment("org", "orders", 0, "y", &["x", "y"], &["x", "y"], 2);
        fx.manager.apply_assignment(follows.clone()).await;
        let key = ("org".to_string(), "orders".to_string(), 0u32);
        let runner = Arc::new(FakeFollowerHandle {
            leo: AtomicU64::new(0),
            hw: AtomicU64::new(0),
            lease_expired: AtomicBool::new(false),
            log_epoch: AtomicU32::new(0),
            committed: AtomicU64::new(0),
            disconnected: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        fx.manager.registry.get_mut(&key).unwrap().follower = Some(Box::new(FakeFollowerRunner {
            shared: Arc::clone(&runner),
        }));
        // The racing call saw no entry and installs the same row.
        fx.manager.install_follower_entry(key.clone(), follows);
        assert!(
            !runner.stopped.load(Ordering::SeqCst),
            "a runner of the same term must keep serving"
        );
        assert!(fx.manager.registry.get(&key).unwrap().follower.is_some());
    }
}
