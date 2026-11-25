// =============================================================================
// File: bus/replication/leader.rs — M2 replication leader (PLAN-M2 §1b)
// =============================================================================
//
// Owns the leader half of replication for one (org, topic, partition):
// `PartitionLeader` is the shared state (ISR, per-follower offsets, ack
// bookkeeping) every one of this partition's `run_follower_stream` tasks
// reads and mutates, and is also what `manager.rs` (agent EL, not yet
// written) bridges into `bus::ReplicationCoordinator::{preflight,
// await_acks, note_offset_commit, snapshot}` for one (org, topic,
// partition) at a time. `run_follower_stream` itself owns exactly ONE
// leader<->follower bidi stream's lifecycle: `Hello`/`HelloAck`, then a
// feeder loop driven by `Partition::subscribe_leo` (plus
// `PartitionLeader::subscribe_hw`, whose doc says what it is worth now),
// concurrent `Ack` intake, heartbeats in
// silence, and coalesced `ReplOffsets` — ending with
// a `FollowerStreamError` the caller (manager.rs) uses to decide whether
// to reconnect (with backoff) or give up for good (`Detached`).
//
// RAW-BYTES FEEDER (M2 wave 2, agent G — closes the wave-1 CPU trade-off
// this comment used to document): `PartitionLeader`'s feeder now reads
// through `PartitionReader::fetch_raw_to_end_of_log`, which — unlike
// `fetch_from_offset`'s parsed `BatchView` — hands back each batch's exact
// on-disk bytes (`RawBatch::bytes`) with zero re-parsing/re-encoding. Every
// `Batch` frame sent to a follower now carries those bytes UNCHANGED, so a
// follower's `append_replicated` receives byte-identical batches to the
// leader's own segment file, matching PLAN-M2 §1b's original "zero
// re-serialization" intent exactly (wave 1's `reencode_batch`/`BatchBuilder`
// round trip — one CPU-bound encode/possibly-recompress per follower per
// batch — is gone).
//
// LAG-BYTES TRADE-OFF: `FollowerState.lag_bytes` is NOT
// `leader_leo - follower_leo` measured in bytes (the engine has no
// leo->byte-position index this module could use for that without
// re-reading the whole gap first). It is the leader-side count of wire
// bytes already SENT to a follower but not yet confirmed by that
// follower's `Ack.follower_leo` (`InFlightTracker`). Under the case that
// actually matters for ISR management — a follower falling behind while
// the leader keeps feeding it — this is an equally faithful measure of
// outstanding un-replicated data, and it needs no extra bookkeeping beyond
// what the feeder already tracks per batch sent.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::{Condvar, Mutex};
use tentaflow_bus::{BusError, Partition, PartitionReader};
use tentaflow_protocol::environment::NodeEnvironment;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

use super::election::{availability_quorum, min_isr_required, LogPosition};
use super::frames::{
    write_frame, FrameReader, ReplAck, ReplBatchHeader, ReplCodecError, ReplFrame, ReplGroup,
    ReplHeartbeat, ReplHello, ReplOffsets, ReplProducerMark, ReplReject, ReplTruncate,
    LOCAL_BUS_FEATURES,
};
use super::metrics::LeaderMetrics;
use crate::bus::topics::Acks;
use crate::bus::AckOutcome;

/// Tuning knobs for a `PartitionLeader` and every `run_follower_stream`
/// task it drives. Defaults match PLAN-M2 §1b's own numbers verbatim; a
/// test wanting faster cadences constructs its own value instead of
/// mutating shared constants (same pattern `follower.rs`'s
/// `FollowerConfig` uses).
/// Diagnostic hook invoked synchronously for every `IsrEvent` — lag-driven
/// shrink/expand from `reconcile_follower`, stream attach/detach from
/// `register_follower`/`remove_follower` (TentaBus 1C, OTWARTE-POZYCJE.md
/// `P7-pipeline-flake-open`, added 2026-09-22): before this, no ISR
/// shrink/expand instrumentation existed anywhere reachable from a bench or
/// test, which is exactly why the leading hypothesis for `bus_replication`'s
/// P7 flake — a spurious ISR eviction under scheduler contention on a
/// shared host — had never been investigated. `LeaderConfig::isr_event_hook`
/// defaulting to `None` costs nothing beyond one branch per event; a caller
/// (e.g. `benches/bus_replication.rs`'s `gate_p7`) installs one to
/// `eprintln!` every transition with its reason during a measurement window.
pub type IsrEventHook = Arc<dyn Fn(&IsrEvent) + Send + Sync>;

#[derive(Clone)]
pub struct LeaderConfig {
    /// A follower stream sends a bare `Heartbeat` once this much wall time
    /// has passed since its last frame of any kind (PLAN-M2 §1b: "co
    /// 500 ms w ciszy").
    pub heartbeat_interval: Duration,
    /// `ReplOffsets` coalescing window (PLAN-M2 §1b, K-M2-5): consumer
    /// group commit/discard notes accumulate for up to this long before
    /// being flushed in one frame.
    pub offsets_coalesce_interval: Duration,
    /// ISR-shrink threshold: bytes sent to one follower that remain
    /// unacknowledged (PLAN-M2 §1b default: 64 MiB).
    pub replica_lag_max_bytes: u64,
    /// ISR-shrink threshold: milliseconds since a follower's last `Ack`
    /// (PLAN-M2 §1b default: 5 s).
    pub replica_lag_max_ms: u64,
    /// Read budget per `PartitionReader::fetch_from_offset` call while
    /// feeding one follower. Mirrors PLAN §5.3.1's `batch_max_bytes`
    /// default (1 MiB): large enough that a caught-up follower's steady
    /// state is one fetch per new batch, small enough that a follower
    /// resyncing after a long gap never forces one fetch to buffer an
    /// unbounded slice of the log in memory.
    pub batch_fetch_max_bytes: usize,
    /// See `IsrEventHook`'s doc. `None` in every production path (wired
    /// only by benches/tests that need live visibility into ISR churn).
    pub isr_event_hook: Option<IsrEventHook>,
}

impl fmt::Debug for LeaderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaderConfig")
            .field("heartbeat_interval", &self.heartbeat_interval)
            .field("offsets_coalesce_interval", &self.offsets_coalesce_interval)
            .field("replica_lag_max_bytes", &self.replica_lag_max_bytes)
            .field("replica_lag_max_ms", &self.replica_lag_max_ms)
            .field("batch_fetch_max_bytes", &self.batch_fetch_max_bytes)
            .field("isr_event_hook", &self.isr_event_hook.is_some())
            .finish()
    }
}

impl Default for LeaderConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_millis(500),
            offsets_coalesce_interval: Duration::from_millis(500),
            replica_lag_max_bytes: 64 * 1024 * 1024,
            replica_lag_max_ms: 5_000,
            batch_fetch_max_bytes: 1024 * 1024,
            isr_event_hook: None,
        }
    }
}

/// One follower's leader-side bookkeeping (PLAN-M2 §1b item 1).
#[derive(Debug, Clone)]
pub struct FollowerState {
    /// Last `log_end_offset` this follower reported via `Ack`.
    pub leo: u64,
    /// Last `high_watermark` this follower reported via `Ack`.
    pub hw: u64,
    pub last_ack_at: Instant,
    pub in_isr: bool,
    /// See the module doc's "LAG-BYTES TRADE-OFF" note.
    pub lag_bytes: u64,
    /// Last `Partition::log_epoch` this follower reported via `Ack`, claim
    /// included; `None` until one arrives or from a follower that predates
    /// the field.
    pub log_epoch: Option<u32>,
}

/// Why `reconcile_follower` shrank a follower out of the ISR (PLAN-M2 §1b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsrShrinkReason {
    LagBytes { lag_bytes: u64, max_bytes: u64 },
    AckTimeout { since_ms: u64, max_ms: u64 },
}

/// ISR membership change (PLAN-M2 §1b item 1: "shrink/expand emit an event
/// enum (for metrics/UI) — no audit"). `isr_size`/`min_isr` ride along on
/// `Shrink` so a subscriber can tell a routine shrink (still `>= min_isr`)
/// apart from one that just made the partition unwritable (K-M2-2) without
/// a second query back into `PartitionLeader`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsrEvent {
    Shrink {
        node_id: String,
        reason: IsrShrinkReason,
        isr_size: u32,
        min_isr: u32,
    },
    Expand {
        node_id: String,
        isr_size: u32,
    },
    /// A follower's replication stream completed its handshake and the
    /// follower was (re-)admitted, in the ISR (`register_follower`).
    Attached {
        node_id: String,
        isr_size: u32,
    },
    /// A follower's replication stream ended and the follower was dropped
    /// from this leader's bookkeeping until the supervisor reconnects it
    /// (`remove_follower`). Distinct from `Shrink`: the replica did not fall
    /// behind, its stream is gone — `reason` is the stream's end cause. A
    /// replica that leaves this way contributes nothing to `acks` either,
    /// so without this event a quorum wait failing with `acked=1` and NO
    /// `Shrink` was indistinguishable from a healthy ISR.
    Detached {
        node_id: String,
        reason: String,
        isr_size: u32,
        min_isr: u32,
    },
}

/// K-M2-5 note fed through `PartitionLeader::note_offset_commit`/
/// `note_offset_discarded` into every follower stream's own coalescing
/// buffer (PLAN-M2 §1b item 2).
#[derive(Debug, Clone)]
pub enum OffsetNote {
    Commit {
        group: String,
        offset: u64,
        attempts: u32,
    },
    Discarded {
        offset: u64,
    },
    /// A consumer-group row of this topic was written (`bus_groups`); the
    /// stream sends the current rows.
    GroupsChanged,
}

/// Every consumer group's committed offset on one partition, and the group
/// rows of its topic, read from this node's own stores.
#[derive(Debug, Clone, Default)]
pub struct GroupSnapshot {
    /// `(group, partition, offset, attempts)`, as `ReplOffsets::commits`.
    pub commits: Vec<(String, u32, u64, u32)>,
    pub groups: Vec<ReplGroup>,
}

/// Reads a `GroupSnapshot`; `with_commits` false skips the offsets, for a
/// stream that only needs the rows.
pub type GroupSnapshotLookup = Arc<dyn Fn(bool) -> GroupSnapshot + Send + Sync>;

/// Per-batch producer-identity side channel (PLAN-M2 §1b item 2): the
/// service layer (agent S) knows which published batch carried a producer
/// identity, `PartitionLeader`/the engine's `BatchView` does not (M1's
/// on-disk batch header has no `producer_id` field — see `frames.rs`'s
/// `ReplBatchHeader` doc, K-M2-6). Wrapped in its own tiny struct (rather
/// than a bare `Option<ReplProducerMark>` return from the lookup) so a
/// later wave can grow this without changing the lookup's signature.
#[derive(Debug, Clone, Default)]
pub struct OutboundBatchMeta {
    pub producer: Option<ReplProducerMark>,
}

/// `base_offset -> ReplProducerMark` lookup the service layer provides
/// (PLAN-M2 §1b item 2). `None` (no hook installed) always yields
/// `OutboundBatchMeta::default()` (no producer mark on any batch).
pub type ProducerMarkLookup = Arc<dyn Fn(u64) -> OutboundBatchMeta + Send + Sync>;

/// Every handled-record marker (`dlq::DiscardStore`) of the DLQ partition
/// this leader streams, at or above the given earliest retained offset. A
/// marker note (`OffsetNote::Discarded`) reaches only the streams open when
/// it is written, so each follower stream sends this whole set when it
/// starts and again whenever it missed notes (`RecvError::Lagged`): a
/// follower that was away still ends up with every marker. `None` for a
/// partition that is not a DLQ partition.
pub type HandledOffsetsLookup = Arc<dyn Fn(u64) -> Vec<u64> + Send + Sync>;

/// Handled markers one `Offsets` frame carries at most. A full marker set
/// (`HandledOffsetsLookup`) can be arbitrarily large, and a frame above
/// `MAX_FRAME_BYTES` would end the stream on every reconnect for good; at
/// ~15 CBOR bytes per `(partition, offset)` this stays near 1 MiB.
const DISCARDS_PER_OFFSETS_FRAME: usize = 65_536;

/// Queues `lookup`'s full marker set for the next `Offsets` frames
/// (`HandledOffsetsLookup`'s doc). The set is read from the local store on
/// the blocking pool, not on this stream's task, and merged into
/// `pending_discards`, a set, so a marker already queued is sent once.
async fn queue_handled_offsets(
    lookup: &Option<HandledOffsetsLookup>,
    earliest_offset: u64,
    pending_discards: &mut BTreeSet<u64>,
) {
    let Some(lookup) = lookup else {
        return;
    };
    let lookup = Arc::clone(lookup);
    match tokio::task::spawn_blocking(move || lookup(earliest_offset)).await {
        Ok(offsets) => pending_discards.extend(offsets),
        Err(e) => tracing::warn!(
            error = %e,
            "replication: reading the DLQ partition's handled markers did not finish"
        ),
    }
}

/// Reads the group state of `leader`'s partition for the next `Offsets`
/// frame: its commits (when `with_commits`) merge into `pending_commits`, the
/// group rows replace `pending_groups`. A snapshot commit already queued for
/// the same group is superseded by the later one, so it is not sent twice.
async fn queue_group_snapshot(
    leader: &Arc<PartitionLeader>,
    with_commits: bool,
    pending_commits: &mut Vec<(String, u32, u64, u32)>,
    pending_groups: &mut Option<Vec<ReplGroup>>,
    groups_seen: &mut u64,
) {
    // Read before the rows are: a change made while they are read is newer
    // than this snapshot and keeps the stream from being counted as flushed.
    let generation = leader.groups_gen.load(Ordering::SeqCst);
    let Some(lookup) = leader.group_snapshot_lookup() else {
        *groups_seen = generation;
        return;
    };
    match tokio::task::spawn_blocking(move || lookup(with_commits)).await {
        Ok(snapshot) => {
            *groups_seen = generation;
            pending_commits
                .retain(|(group, ..)| !snapshot.commits.iter().any(|(g, ..)| g == group));
            pending_commits.extend(snapshot.commits);
            *pending_groups = Some(snapshot.groups);
        }
        Err(e) => tracing::warn!(
            error = %e,
            "replication: reading the consumer groups' state did not finish"
        ),
    }
}

fn compute_min_isr(replica_count: usize) -> u32 {
    (replica_count as u32) / 2 + 1
}

/// Wakes every pending `await_acks` call whenever any follower's
/// acknowledged offset or ISR membership changes. One shared generation
/// counter + condvar rather than per-offset waiter lists: every waiter
/// recomputes its own readiness on each wakeup, so any change that could
/// satisfy ANY waiter bumps it, and a spurious wakeup costs one cheap
/// recheck over a handful of followers.
///
/// A waiter samples `generation` BEFORE checking readiness and sleeps only
/// while it is still that value, so an ack landing between the check and
/// the sleep is never lost (a bare `notify_waiters` stores no permit and
/// would leave that waiter asleep until some later, unrelated change).
/// Blocking on a condvar, not a tokio primitive, because the only caller is
/// `ReplicationCoordinator::await_acks` — a synchronous trait method on the
/// publish path (see `GlueLeaderHandle::await_acks`).
struct AckWaiters {
    generation: Mutex<u64>,
    changed: Condvar,
    closed: AtomicBool,
}

impl AckWaiters {
    fn new() -> Self {
        Self {
            generation: Mutex::new(0),
            changed: Condvar::new(),
            closed: AtomicBool::new(false),
        }
    }

    fn generation(&self) -> u64 {
        *self.generation.lock()
    }

    fn notify_all(&self) {
        *self.generation.lock() += 1;
        self.changed.notify_all();
    }

    /// Ends every current and future wait immediately: nothing will ever
    /// ack through a leader whose streams were stopped.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Sleeps until `generation` moves past `seen` or `deadline` passes.
    fn wait_for_change(&self, seen: u64, deadline: std::time::Instant) {
        let mut generation = self.generation.lock();
        while *generation == seen {
            if self
                .changed
                .wait_until(&mut generation, deadline)
                .timed_out()
            {
                return;
            }
        }
    }
}

/// Per (org, topic, partition) leader-side replication state (PLAN-M2 §1b
/// item 1): epoch, replica set, per-follower `FollowerState`, ISR
/// membership, and the ack-quorum bookkeeping `await_acks` blocks on.
/// Shared (`Arc`) across every `run_follower_stream` task feeding one of
/// this partition's followers, and across whatever bridges it into
/// `bus::ReplicationCoordinator` (manager.rs, agent EL).
pub struct PartitionLeader {
    /// plan-app-platform §1.6: which TentaBus instance this partition
    /// belongs to — stamped onto every `ReplHello` this leader sends
    /// (`run_follower_stream`'s own doc), since a `PartitionLeader` has no
    /// other route back to the `ReplicationManager`/`ReplicationManagerConfig::
    /// instance_id` that spawned it (`GlueLeaderFactory::spawn_with_epoch_mode`
    /// threads `assignment.instance_id` through here at construction).
    instance_id: String,
    org_id: String,
    topic: String,
    partition_id: u32,
    local_node_id: String,
    replicas: Vec<String>,
    epoch: AtomicU32,
    /// The topic incarnation this leader serves
    /// (`PartitionAssignment::topic_generation`), stamped onto every
    /// `ReplHello` it sends.
    topic_generation: u64,
    acks: Acks,
    environment: NodeEnvironment,
    min_isr: u32,
    followers: DashMap<String, FollowerState>,
    partition: Partition,
    ack_waiters: AckWaiters,
    events_tx: broadcast::Sender<IsrEvent>,
    offset_notes_tx: broadcast::Sender<OffsetNote>,
    /// Where a follower stream reads the group state it sends when it opens,
    /// after it lagged, and when the rows change. Installed by the glue.
    group_snapshot: Mutex<Option<GroupSnapshotLookup>>,
    /// Bumped by every `note_groups_changed`: what a stream has to have sent
    /// to be sure a change made before a handover reached its follower.
    groups_gen: AtomicU64,
    /// Per follower: the `groups_gen` its stream had read and sent the rows
    /// of (`mark_groups_sent`).
    groups_sent: DashMap<String, u64>,
    /// Per follower: the `groups_gen` it reported applying
    /// (`ReplAck::groups_applied`). Absent for a follower that has not said,
    /// which falls back on what was sent.
    groups_applied: DashMap<String, u64>,
    /// Followers whose acks carry no `groups_applied` (they predate it): for
    /// them "sent" is all a leader can know.
    groups_legacy: dashmap::DashSet<String>,
    /// Asks every stream to send what it has queued now instead of at the
    /// next coalescing tick (`request_offsets_flush`).
    flush_tx: watch::Sender<u64>,
    /// Wakes every feeder task whenever `recompute_hw` actually moves the
    /// engine-visible `high_watermark` — see `subscribe_hw`'s doc.
    hw_tx: watch::Sender<u64>,
    /// At RF 2, whether this leader may commit alone since its in-sync set
    /// shrank to itself: the follower acknowledged the shrink, or has been
    /// out of reach of the whole mesh long enough that it cannot be standing
    /// over this leader (`ReplicationManager::check_alone_rules`). Cleared by
    /// every shrink; `true` otherwise and at any other RF.
    alone_cleared: AtomicBool,
    /// How many times the in-sync set has shrunk under this leader at RF 2.
    /// Guards the grant of `alone_cleared`: an acknowledgement is for the shrink
    /// it answered. Held while `alone_cleared` is withdrawn or granted on it.
    shrink_seq: parking_lot::Mutex<u64>,
    /// The log end when this leader started: what an earlier incarnation of
    /// the same term durably left. Records of this term above it, on a
    /// follower that this incarnation has not fed yet, were written by that
    /// earlier incarnation and lost from this log in the crash
    /// (`previous_incarnation_cut`).
    start_leo: u64,
    /// Followers this incarnation has completed a handshake with.
    fed: dashmap::DashSet<String>,
    config: LeaderConfig,
    metrics: Arc<LeaderMetrics>,
}

impl PartitionLeader {
    /// `replicas` is the full replica set (RF nodes), stable order,
    /// INCLUDING `local_node_id` — mirrors `PartitionAssignment::replicas`
    /// (`assignment.rs`). `acks` is the topic's configured ack level
    /// (`topics::Acks`, PLAN §7.1): it is what drives the engine's own
    /// consumer-visible `high_watermark` (`recompute_hw`); `await_acks`
    /// still accepts an independent `acks` argument per call so a caller
    /// can observe a stronger/weaker threshold than the topic default
    /// without moving that shared watermark to match.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance_id: impl Into<String>,
        org_id: impl Into<String>,
        topic: impl Into<String>,
        partition_id: u32,
        local_node_id: impl Into<String>,
        replicas: Vec<String>,
        epoch: u32,
        topic_generation: u64,
        acks: Acks,
        environment: NodeEnvironment,
        partition: Partition,
        config: LeaderConfig,
        metrics: Arc<LeaderMetrics>,
    ) -> Self {
        let replicas_len = replicas.len();
        let min_isr = compute_min_isr(replicas.len().max(1));
        // Seeded with the current `hw` so a subscription created later only
        // fires on a FUTURE advance — the same contract
        // `Partition::subscribe_leo` gives its own receivers.
        let hw_tx = watch::channel(partition.high_watermark()).0;
        let start_leo = partition.log_end_offset();
        metrics.record_leader_epoch(epoch);
        let leader = Self {
            instance_id: instance_id.into(),
            org_id: org_id.into(),
            topic: topic.into(),
            partition_id,
            local_node_id: local_node_id.into(),
            replicas,
            epoch: AtomicU32::new(epoch),
            topic_generation,
            acks,
            environment,
            min_isr,
            followers: DashMap::new(),
            partition,
            ack_waiters: AckWaiters::new(),
            events_tx: broadcast::channel(64).0,
            offset_notes_tx: broadcast::channel(1024).0,
            group_snapshot: Mutex::new(None),
            groups_gen: AtomicU64::new(0),
            groups_sent: DashMap::new(),
            groups_applied: DashMap::new(),
            groups_legacy: dashmap::DashSet::new(),
            flush_tx: watch::channel(0).0,
            hw_tx,
            // A leader that starts alone commits alone only once its follower
            // is out of the way: it may hold a claim on this very term (a
            // transfer's old leader, a restart's follower) that a standing
            // would turn on the writes committed here.
            alone_cleared: AtomicBool::new(replicas_len != 2),
            shrink_seq: parking_lot::Mutex::new(shrink_seq_base()),
            start_leo,
            fed: dashmap::DashSet::new(),
            config,
            metrics,
        };
        leader.metrics.record_isr_size(leader.isr_size());
        // A leader that starts alone (a promotion, a restart) over a log with
        // a tail no replica ever acknowledged would otherwise keep it hidden
        // until the next ack or publish; the same rule a later ack applies.
        leader.recompute_hw();
        leader
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn org_id(&self) -> &str {
        &self.org_id
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn partition_id(&self) -> u32 {
        self.partition_id
    }

    pub fn local_node_id(&self) -> &str {
        &self.local_node_id
    }

    pub fn replicas(&self) -> &[String] {
        &self.replicas
    }

    pub fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::Acquire)
    }

    pub fn topic_generation(&self) -> u64 {
        self.topic_generation
    }

    pub fn acks(&self) -> Acks {
        self.acks
    }

    pub fn environment(&self) -> NodeEnvironment {
        self.environment
    }

    pub fn min_isr(&self) -> u32 {
        self.min_isr
    }

    pub fn config(&self) -> &LeaderConfig {
        &self.config
    }

    pub fn high_watermark(&self) -> u64 {
        self.partition.high_watermark()
    }

    /// `Partition::committed_offset` — what `recompute_hw` last found on a
    /// majority of the replica set.
    pub fn committed_offset(&self) -> u64 {
        self.partition.committed_offset()
    }

    /// `Partition::log_epoch` — the epoch this leader's own log was last
    /// written under, which is not its term until it has appended in it.
    pub fn log_epoch(&self) -> u32 {
        self.partition.log_epoch()
    }

    /// `Partition::record_epoch` — the epoch of this leader's last record,
    /// what a follower's log is reconciled against.
    pub fn record_epoch(&self) -> u32 {
        self.partition.record_epoch()
    }

    /// Where this leader's log holds the records of `epoch`
    /// (`Partition::epoch_span`).
    pub fn epoch_span(&self, epoch: u32) -> Option<(u64, u64)> {
        self.partition.epoch_span(epoch)
    }

    /// Where this leader's own term begins in its log: its first record in
    /// that term, or the claim it stamped on starting to serve
    /// (`GlueLeaderHandle::open_writes`). `None` before either.
    pub fn term_start(&self) -> Option<u64> {
        self.partition.epoch_start(self.epoch())
    }

    /// `Partition::epoch_at`: the epoch the record at `offset` was first
    /// written in — what a fed batch tells the follower to record.
    pub fn epoch_at(&self, offset: u64) -> u32 {
        self.partition.epoch_at(offset)
    }

    pub fn log_end_offset(&self) -> u64 {
        self.partition.log_end_offset()
    }

    pub fn open_reader(&self) -> PartitionReader {
        self.partition.open_reader()
    }

    pub fn subscribe_leo(&self) -> watch::Receiver<u64> {
        self.partition.subscribe_leo()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<IsrEvent> {
        self.events_tx.subscribe()
    }

    pub fn subscribe_offset_notes(&self) -> broadcast::Receiver<OffsetNote> {
        self.offset_notes_tx.subscribe()
    }

    /// Count of replicas currently in the ISR, this leader's own local
    /// replica included (a leader is always caught up with itself).
    pub fn isr_size(&self) -> u32 {
        1 + self.followers.iter().filter(|e| e.in_isr).count() as u32
    }

    pub fn isr_members(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.followers.len() + 1);
        v.push(self.local_node_id.clone());
        for e in self.followers.iter() {
            if e.in_isr {
                v.push(e.key().clone());
            }
        }
        v
    }

    /// K-M2-2: `true` once the ISR has shrunk below `min_isr` — the
    /// signal `preflight` (manager.rs) uses to refuse new writes with
    /// `NotEnoughReplicas` instead of silently degrading durability.
    pub fn below_min_isr(&self) -> bool {
        self.isr_size() < self.min_isr
    }

    /// Whether a majority of the replica set — this leader counted — has
    /// acknowledged within `replica_lag_max_ms`, judged at call time. Not
    /// read off `in_isr`: only the per-follower stream tasks update that, so
    /// a leader whose tasks are wedged would keep an ISR it no longer has.
    /// Here the acks simply stop arriving and the lease lapses on its own.
    pub fn has_quorum_lease(&self) -> bool {
        let now = Instant::now();
        let window = Duration::from_millis(self.config.replica_lag_max_ms);
        let fresh = self
            .followers
            .iter()
            .filter(|e| now.saturating_duration_since(e.last_ack_at) <= window)
            .count();
        1 + fresh >= availability_quorum(self.replicas.len())
    }

    /// Where to cut a follower whose log holds records an earlier
    /// incarnation of this leader wrote under the same term, `None` when this
    /// is not such a follower. A leader that restarts keeps its epoch and
    /// writes new records at the offsets its unflushed tail had; a follower
    /// that holds that tail has records at the same `(epoch, offset)` with
    /// other contents, which no comparison of epochs and offsets can tell
    /// from this chain's. This incarnation has written nothing to a follower
    /// it has not fed, so every record of the follower above `start_leo` and
    /// of this term predates it. A follower whose committed offset is above
    /// that holds acknowledged records this log lacks and is left to refuse
    /// the cut.
    ///
    /// A follower counts as fed only once nothing is left to cut: at once
    /// when no cut applies, otherwise when it acknowledged a position that
    /// shows the `Truncate` applied (`note_cut_applied`). A stream that drops
    /// in between leaves it unfed, and the next handshake cuts it again.
    fn previous_incarnation_cut(&self, node: &str, log: &LogPosition) -> Option<u64> {
        if self.fed.contains(node) {
            return None;
        }
        let cut = log.epoch == self.epoch()
            && log.leo > self.start_leo
            && log.committed <= self.start_leo;
        if !cut {
            self.fed.insert(node.to_string());
            return None;
        }
        Some(self.start_leo)
    }

    /// The follower acknowledged a position that shows this incarnation's
    /// `Truncate` was applied: it holds nothing of an earlier one any more.
    fn note_cut_applied(&self, node: &str) {
        self.fed.insert(node.to_string());
    }

    /// See `alone_cleared`.
    pub fn alone_cleared(&self) -> bool {
        self.alone_cleared.load(Ordering::SeqCst)
    }

    /// Clearing (`false`) is what a shrink does; granting (`true`) is the
    /// manager's verdict that the follower is out of the way. Granting wakes
    /// the writes that waited for it.
    pub fn set_alone_cleared(&self, cleared: bool) {
        if self.alone_cleared.swap(cleared, Ordering::SeqCst) != cleared {
            self.recompute_hw();
        }
    }

    /// At RF 2 with the follower out of the in-sync set and not yet cleared
    /// to commit alone: nothing is acknowledged by this leader's log alone
    /// (`acks=all`) and `hw` stays where it is.
    fn alone_blocked(&self) -> bool {
        self.replicas.len() == 2 && !self.alone_cleared() && self.isr_size() <= 1
    }

    /// A shrink at RF 2 revokes the permission to commit alone.
    fn note_shrunk(&self) {
        if self.replicas.len() == 2 {
            let mut seq = self.shrink_seq.lock();
            *seq += 1;
            self.alone_cleared.store(false, Ordering::SeqCst);
        }
    }

    /// The shrink counter: what an `IsrShrink` notice and the heartbeats name.
    /// It starts at a base derived from the clock, so the numbers of a leader
    /// that restarts at the same epoch all exceed those of the incarnation
    /// before (a follower's revocation outlives the restart).
    pub fn shrink_seq(&self) -> u64 {
        *self.shrink_seq.lock()
    }

    /// Grants the permission to commit alone only if no shrink was counted
    /// since `seq` was read. `true` when granted.
    pub fn grant_alone_if_unshrunk(&self, seq: u64) -> bool {
        let changed = {
            let current = self.shrink_seq.lock();
            if *current != seq {
                return false;
            }
            !self.alone_cleared.swap(true, Ordering::SeqCst)
        };
        if changed {
            self.recompute_hw();
        }
        true
    }

    /// The follower `node_id` is stopping and no longer serves or stands: at
    /// RF 2 it leaves the in-sync set and, nothing it could stand on being
    /// left, this leader may commit alone at once. `true` when granted.
    pub fn follower_leaving(&self, node_id: &str) -> bool {
        if self.replicas.len() != 2 || !self.replicas.iter().any(|r| r == node_id) {
            return false;
        }
        self.remove_follower(node_id, "follower stopping");
        self.grant_alone_if_unshrunk(self.shrink_seq())
    }

    pub fn follower_state(&self, node_id: &str) -> Option<FollowerState> {
        self.followers.get(node_id).map(|e| e.clone())
    }

    /// Adds (or re-adds, after a reconnect) a follower to this partition's
    /// bookkeeping, starting in the ISR — a just-accepted `Hello` already
    /// proved the connection is live, and a genuinely far-behind follower
    /// falls back out of the ISR on its own via `reconcile_follower` once
    /// its lag or ack-staleness crosses the configured threshold.
    ///
    /// `initial_leo`/`initial_hw` are clamped to this leader's own chain
    /// (see `record_ack`'s DIVERGENCE GUARD for why): the caller already
    /// truncated a too-far-ahead `initial_leo` down to the authority offset,
    /// and the clamp here covers the remaining callers/tests.
    pub fn register_follower(&self, node_id: impl Into<String>, initial_leo: u64, initial_hw: u64) {
        let node_id = node_id.into();
        let own_leo = self.partition.log_end_offset();
        let own_hw = self.partition.high_watermark();
        self.followers.insert(
            node_id.clone(),
            FollowerState {
                leo: initial_leo.min(own_leo),
                hw: initial_hw.min(own_hw),
                last_ack_at: Instant::now(),
                in_isr: true,
                lag_bytes: 0,
                log_epoch: None,
            },
        );
        let isr_size = self.isr_size();
        self.metrics.record_isr_size(isr_size);
        self.emit(IsrEvent::Attached { node_id, isr_size });
        self.recompute_hw();
    }

    /// Drops `node_id` from this leader's bookkeeping because its stream
    /// ended; `reason` is that end cause, carried on `IsrEvent::Detached`.
    pub fn remove_follower(&self, node_id: &str, reason: &str) {
        if self.followers.remove(node_id).is_some() {
            self.note_shrunk();
            let isr_size = self.isr_size();
            self.metrics.record_isr_size(isr_size);
            self.emit(IsrEvent::Detached {
                node_id: node_id.to_string(),
                reason: reason.to_string(),
                isr_size,
                min_isr: self.min_isr,
            });
            self.recompute_hw();
        }
    }

    /// Delivers one membership event to the diagnostic hook and to every
    /// `subscribe_events` receiver.
    fn emit(&self, event: IsrEvent) {
        if let Some(hook) = &self.config.isr_event_hook {
            hook(&event);
        }
        let _ = self.events_tx.send(event);
    }

    /// Applies one `Ack` frame's contents plus the feeder's freshly
    /// computed in-flight-bytes lag to `node_id`'s state, then reconciles
    /// ISR membership and recomputes the engine watermark. `leo`/`hw` are
    /// folded in with `max` rather than assigned outright as a guard
    /// against a stale/reordered `Ack` moving this follower's recorded
    /// state backwards (acks are sent on a cadence, not necessarily in
    /// strict lockstep with arrival order on every transport).
    ///
    /// DIVERGENCE GUARD (K-M2-1): a follower can never legitimately report
    /// an offset or watermark beyond this leader's own chain — everything
    /// it receives comes from this leader. Values past this leader's own
    /// `log_end_offset`/`high_watermark` mean the follower holds a
    /// DIVERGENT tail (records from a role it no longer has, or from a
    /// leader that was fenced mid-stream). Counting such a leo toward
    /// `recompute_hw` would mark data this leader's chain cannot back as
    /// committed (`Acks::Leader`'s hw is the max leo), and — measured in
    /// `tests/bus_replication_three_node.rs`'s truncate scenario — the
    /// resulting `hw` pushed back to the divergent follower makes the
    /// upcoming K-M2-1 `Truncate` illegal below that phantom watermark,
    /// wedging the replica at its divergent tail forever. The extra
    /// records remain the truncate's problem (`run_follower_stream`'s
    /// truncate-on-reopen), never the watermark's.
    pub fn record_ack(&self, node_id: &str, ack: &ReplAck, lag_bytes: u64) {
        let own_leo = self.partition.log_end_offset();
        let own_hw = self.partition.high_watermark();
        if let Some(mut fs) = self.followers.get_mut(node_id) {
            fs.leo = fs.leo.max(ack.follower_leo.min(own_leo));
            fs.hw = fs.hw.max(ack.follower_hw.min(own_hw));
            fs.last_ack_at = Instant::now();
            fs.lag_bytes = lag_bytes;
            if ack.follower_log_epoch.is_some() {
                fs.log_epoch = ack.follower_log_epoch;
            }
        }
        self.reconcile_follower(node_id);
        self.recompute_hw();
    }

    /// Updates lag bookkeeping outside of an `Ack` (called right after the
    /// feeder sends a batch), so a heartbeat-driven `reconcile_follower`
    /// can catch a runaway lag even during a long silence from that
    /// follower.
    pub fn set_follower_lag_bytes(&self, node_id: &str, lag_bytes: u64) {
        if let Some(mut fs) = self.followers.get_mut(node_id) {
            fs.lag_bytes = lag_bytes;
        }
        self.reconcile_follower(node_id);
    }

    /// Checks `node_id` against both ISR thresholds (PLAN-M2 §1b item 1)
    /// and flips membership (with an `IsrEvent`) if warranted. Idempotent:
    /// calling this repeatedly with unchanged state emits nothing beyond
    /// the `isr_size_min` gauge update.
    pub fn reconcile_follower(&self, node_id: &str) {
        let snapshot = match self.followers.get(node_id) {
            Some(e) => e.clone(),
            None => return,
        };
        self.metrics.record_lag_bytes(snapshot.lag_bytes);
        let since_ack = Instant::now().saturating_duration_since(snapshot.last_ack_at);
        let lag_too_high = snapshot.lag_bytes > self.config.replica_lag_max_bytes;
        let ack_stale = (since_ack.as_millis() as u64) > self.config.replica_lag_max_ms;

        if snapshot.in_isr && (lag_too_high || ack_stale) {
            if let Some(mut fs) = self.followers.get_mut(node_id) {
                fs.in_isr = false;
            }
            self.note_shrunk();
            let reason = if lag_too_high {
                IsrShrinkReason::LagBytes {
                    lag_bytes: snapshot.lag_bytes,
                    max_bytes: self.config.replica_lag_max_bytes,
                }
            } else {
                IsrShrinkReason::AckTimeout {
                    since_ms: since_ack.as_millis() as u64,
                    max_ms: self.config.replica_lag_max_ms,
                }
            };
            let isr_size = self.isr_size();
            self.metrics.record_isr_shrink();
            self.metrics.record_isr_size(isr_size);
            self.emit(IsrEvent::Shrink {
                node_id: node_id.to_string(),
                reason,
                isr_size,
                min_isr: self.min_isr,
            });
            self.recompute_hw();
        } else if !snapshot.in_isr && !lag_too_high && !ack_stale {
            if let Some(mut fs) = self.followers.get_mut(node_id) {
                fs.in_isr = true;
            }
            let isr_size = self.isr_size();
            self.metrics.record_isr_size(isr_size);
            self.emit(IsrEvent::Expand {
                node_id: node_id.to_string(),
                isr_size,
            });
            self.recompute_hw();
        } else {
            self.metrics.record_isr_size(self.isr_size());
        }
    }

    /// K-M2-5: records a consumer-group offset commit for replication.
    /// Fanned out via `subscribe_offset_notes` to every active follower
    /// stream, each of which coalesces its own copy on its own
    /// `offsets_coalesce_interval` tick — see the module doc.
    pub fn note_offset_commit(&self, group: impl Into<String>, offset: u64, attempts: u32) {
        let _ = self.offset_notes_tx.send(OffsetNote::Commit {
            group: group.into(),
            offset,
            attempts,
        });
    }

    pub fn note_offset_discarded(&self, offset: u64) {
        let _ = self.offset_notes_tx.send(OffsetNote::Discarded { offset });
    }

    /// A consumer-group row of this partition's topic was written: every open
    /// stream sends the rows again on its next coalescing tick.
    pub fn note_groups_changed(&self) {
        self.groups_gen.fetch_add(1, Ordering::SeqCst);
        let _ = self.offset_notes_tx.send(OffsetNote::GroupsChanged);
    }

    /// Makes every follower stream send its queued commits and group rows
    /// now, not at its next coalescing tick.
    pub fn request_offsets_flush(&self) {
        self.flush_tx.send_modify(|n| *n += 1);
    }

    /// Whether the stream to `follower` has sent the group rows as of every
    /// `note_groups_changed` so far. A follower with no stream never has.
    pub fn groups_flushed_to(&self, follower: &str) -> bool {
        let wanted = self.groups_gen.load(Ordering::SeqCst);
        match self.groups_applied.get(follower) {
            Some(applied) => *applied >= wanted,
            None => {
                self.groups_legacy.contains(follower)
                    && self
                        .groups_sent
                        .get(follower)
                        .is_some_and(|sent| *sent >= wanted)
            }
        }
    }

    fn mark_groups_applied(&self, follower: &str, generation: u64) {
        self.groups_applied
            .entry(follower.to_string())
            .and_modify(|g| *g = (*g).max(generation))
            .or_insert(generation);
    }

    fn mark_groups_sent(&self, follower: &str, generation: u64) {
        self.groups_sent.insert(follower.to_string(), generation);
    }

    pub fn set_group_snapshot(&self, lookup: GroupSnapshotLookup) {
        *self.group_snapshot.lock() = Some(lookup);
    }

    fn group_snapshot_lookup(&self) -> Option<GroupSnapshotLookup> {
        self.group_snapshot.lock().clone()
    }

    /// This leader's own `log_end_offset` plus every in-ISR follower's
    /// last-reported `leo` — the population `commit_offset_for`/
    /// `await_acks` compute an ack-level threshold against. Out-of-ISR
    /// followers are excluded: by definition they are not known to be
    /// caught up, so their reported offset cannot back a durability
    /// promise (K-M2-2's same reasoning, applied to ack accounting
    /// instead of the `min_isr` availability gate).
    fn isr_leos(&self) -> Vec<u64> {
        let mut v = Vec::with_capacity(self.followers.len() + 1);
        v.push(self.partition.log_end_offset());
        for e in self.followers.iter() {
            if e.in_isr {
                v.push(e.leo);
            }
        }
        v
    }

    /// `Acks::All` asks for every in-sync replica, but never fewer than
    /// `availability_quorum`: at RF ≥ 3 an ISR shrunk to the leader alone
    /// would otherwise let `hw` — what consumers read — run ahead of
    /// anything a failover keeps; at RF ≤ 2 one replica may act alone (the
    /// owner's availability decision, see that function). `Acks::Leader` makes no such promise by definition:
    /// its `hw` is the leader's own log, and a failover may take back what a
    /// consumer read. Replica reconciliation never relies on `hw` for that
    /// reason; it uses `committed_offset`, which is majority-derived for
    /// every level (`recompute_hw`).
    fn required_for(&self, acks: Acks, isr_len: usize) -> u32 {
        match acks {
            Acks::Leader => 1,
            Acks::Quorum => self.min_isr,
            Acks::All if self.alone_blocked() => 2,
            Acks::All => (isr_len as u32).max(availability_quorum(self.replicas.len()) as u32),
        }
    }

    /// The offset every record below which is on a majority of the replica
    /// set — counting every follower's acknowledged `leo`, in the ISR or not
    /// (an out-of-ISR follower is merely behind; what it acknowledged is
    /// still on its disk). Independent of `acks`.
    fn majority_offset(&self) -> u64 {
        let mut leos = Vec::with_capacity(self.followers.len() + 1);
        leos.push(self.partition.log_end_offset());
        leos.extend(self.followers.iter().map(|e| e.leo));
        Self::nth_largest(&leos, min_isr_required(self.replicas.len()) as u32)
    }

    /// Whether a majority of the replica set — this leader included — has
    /// logs naming this leader's term (`Partition::log_epoch`) that reach
    /// `start`, where the term begins: each is then this leader's chain up
    /// to `start`, and ranks above any log of an earlier term. Exactly this
    /// term: a log naming a later one follows another leader's chain.
    fn term_confirmed_by_majority(&self, start: u64) -> bool {
        let epoch = self.epoch();
        let own = usize::from(self.partition.log_epoch() == epoch);
        let followers = self
            .followers
            .iter()
            .filter(|f| f.leo >= start && f.log_epoch == Some(epoch))
            .count();
        own + followers >= min_isr_required(self.replicas.len())
    }

    /// The `n`-th largest value in `values` (1-based), or `0` if `n` is
    /// `0` or exceeds `values.len()` — "not enough replicas have reported
    /// anything yet" is representable as offset `0`, which is always safe
    /// to feed into `Partition::set_high_watermark` (monotonic, never
    /// moves the watermark backwards).
    fn nth_largest(values: &[u64], n: u32) -> u64 {
        if n == 0 || n as usize > values.len() {
            return 0;
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted[n as usize - 1]
    }

    /// The offset such that at least `required_for(acks)` in-ISR replicas
    /// (this leader included) have durably appended up to it.
    fn commit_offset_for(&self, acks: Acks) -> u64 {
        let leos = self.isr_leos();
        let required = self.required_for(acks, leos.len());
        Self::nth_largest(&leos, required)
    }

    /// Recomputes the engine-visible `high_watermark` from this leader's
    /// configured `acks` level and wakes every pending `await_acks` call.
    /// Called after every state change that could move either quantity
    /// (`record_ack`, `reconcile_follower`, follower add/remove).
    fn recompute_hw(&self) {
        // Raft's Figure 8: a majority holding records of an EARLIER term
        // does not commit them — a node with a later-term log can still win
        // an election over those replicas and replace them. Only once a
        // majority's logs name this leader's own term — a record of it, or
        // the claim a follower confirms when its log reaches where the term
        // begins — is everything before it safe; until then the committed
        // offset stays where the earlier terms left it.
        let term_start = self.term_start();
        if let Some(start) = term_start {
            let majority = self.majority_offset();
            if majority > start || self.term_confirmed_by_majority(start) {
                self.partition.set_committed_offset(majority);
            }
        }
        let mut candidate = self.commit_offset_for(self.acks);
        // The same rule for what consumers see. `acks=quorum|all` promise a
        // record read is a record kept; an earlier-term record on a majority
        // is not kept yet — the stale later-term log that can still win would
        // retract it after a consumer read it. So `hw` stays at the committed
        // offset until everything below this term's start is committed.
        // `acks=leader` promises nothing of the kind; at RF = 2 a lone
        // replica acts alone (`availability_quorum`), where waiting for a
        // majority would hide every record for as long as it is alone; and at
        // RF = 1 there is no other log to lose a race to.
        if self.acks != Acks::Leader
            && self.replicas.len() >= 3
            && term_start.is_none_or(|start| self.partition.committed_offset() < start)
        {
            candidate = candidate.min(self.partition.committed_offset());
        }
        let before = self.partition.high_watermark();
        // Alone at RF 2 without leave: what this leader appends is not
        // committed until the follower is known to be out of the way.
        if self.alone_blocked() {
            candidate = before;
        }
        let after = self.partition.set_high_watermark(candidate);
        // Only a real advance wakes anyone: `set_high_watermark` is a monotonic
        // `fetch_max`, so `after > before` can only mean this call moved it. Two
        // concurrent recomputes both waking is harmless — `feed` is idempotent
        // from the cursor it is given.
        if after > before {
            let _ = self.hw_tx.send(after);
        }
        self.ack_waiters.notify_all();
    }

    /// A change-notification for every advance of this leader's
    /// engine-visible `high_watermark`.
    ///
    /// Load-bearing until the wave-3 feed fix, redundant since it: the feeder
    /// used to read through `PartitionReader::fetch_raw_from_offset`, bounded by
    /// `high_watermark`, and `hw` is `recompute_hw`'s output — moved by ACKs and
    /// by ISR membership changes (`register_follower`, `remove_follower`, the
    /// staleness/lag shrink `reconcile_follower` does from a heartbeat tick) and,
    /// since then, by the first look of a publish waiting for its acks
    /// (`await_acks_blocking`), but never by the append itself. So `hw` advanced at moments `leo` had sat still,
    /// and a feeder armed only by `subscribe_leo` slept through them. Measured
    /// in that state (3-node harness, `acks=leader`, 6 sequential publishes):
    /// leader `hw == leo == 6`, both followers `leo == 0`, streams and
    /// heartbeats alive — the records simply had no wake left to carry them.
    ///
    /// `feed` now reads through `PartitionReader::fetch_raw_to_end_of_log`, so
    /// whatever is feedable is feedable the moment it is appended and
    /// `Partition::subscribe_leo` is the wake that carries the RECORDS. This
    /// wake carries the WATERMARK: an advance that no batch is left to carry
    /// is pushed to every follower as a bare heartbeat right away
    /// (`run_follower_stream`), instead of waiting up to a heartbeat interval.
    pub fn subscribe_hw(&self) -> watch::Receiver<u64> {
        self.hw_tx.subscribe()
    }

    /// Blocks the calling thread (up to `timeout`) until at least
    /// `required` in-ISR replicas have reached `next_offset`, the timeout
    /// elapses, or `close_ack_waiters` ends the wait — whichever comes
    /// first. `required` is used exactly as given: the caller
    /// (`manager.rs`'s `ReplicationCoordinator::await_acks`) turns an ack
    /// POLICY into a count itself, since it may honor a level other than
    /// this partition's configured one. `AckOutcome::hw` is always the
    /// engine's actual (topic-`acks`-paced) watermark.
    ///
    /// Must not run on a thread the acks themselves need: they are recorded
    /// by this partition's `run_follower_stream` tasks, so a caller on a
    /// Tokio worker hands that worker off first (`BusService::
    /// publish_async`'s `block_in_place`).
    pub fn await_acks_blocking(
        &self,
        next_offset: u64,
        required: u32,
        timeout: Duration,
    ) -> AckOutcome {
        let started = std::time::Instant::now();
        let deadline = started + timeout;
        // The caller has just appended `next_offset - 1` locally. With no
        // follower left in the ISR no ack will ever follow to move `hw`, so
        // the local append itself is the only moment to apply the ISR rule;
        // for every other state it is the same pure function of the same
        // inputs a later ack would compute, so the rule is not loosened.
        if !self.ack_waiters.is_closed() {
            self.recompute_hw();
        }
        loop {
            let seen = self.ack_waiters.generation();
            if let Some(outcome) = self.ack_outcome_if_ready(next_offset, required, started) {
                return outcome;
            }
            if self.ack_waiters.is_closed() || std::time::Instant::now() >= deadline {
                self.metrics.record_ack_wait(started.elapsed());
                return self.ack_outcome_now(next_offset, required);
            }
            self.ack_waiters.wait_for_change(seen, deadline);
        }
    }

    /// Fails every pending and future `await_acks_blocking` right away with
    /// the acks gathered so far — called when this leader's streams are
    /// stopped, after which no ack can ever arrive through it.
    pub fn close_ack_waiters(&self) {
        self.ack_waiters.close();
    }

    fn ack_outcome_now(&self, next_offset: u64, required: u32) -> AckOutcome {
        let leos = self.isr_leos();
        AckOutcome {
            acked_nodes: leos.iter().filter(|&&l| l >= next_offset).count() as u32,
            required,
            hw: self.partition.high_watermark(),
        }
    }

    /// `Some(outcome)` (recording the ack-wait latency sample) once
    /// `required` in-ISR replicas have reached `next_offset`; `None`
    /// otherwise (caller keeps waiting).
    fn ack_outcome_if_ready(
        &self,
        next_offset: u64,
        required: u32,
        started: std::time::Instant,
    ) -> Option<AckOutcome> {
        let leos = self.isr_leos();
        let offset = Self::nth_largest(&leos, required);
        if offset < next_offset {
            return None;
        }
        self.metrics.record_ack_wait(started.elapsed());
        Some(AckOutcome {
            acked_nodes: leos.iter().filter(|&&l| l >= next_offset).count() as u32,
            required,
            hw: self.partition.high_watermark(),
        })
    }
}

/// Bytes this stream has sent to its follower but that follower has not
/// yet confirmed via `Ack.follower_leo` — see the module doc's
/// "LAG-BYTES TRADE-OFF" note.
#[derive(Default)]
struct InFlightTracker {
    /// `(offset this batch advances the follower's leo to, wire bytes sent)`.
    pending: VecDeque<(u64, u64)>,
    total_bytes: u64,
}

impl InFlightTracker {
    fn record_sent(&mut self, upto_offset: u64, bytes: u64) {
        self.pending.push_back((upto_offset, bytes));
        self.total_bytes += bytes;
    }

    /// Drops every entry the follower has now confirmed, returning the
    /// remaining in-flight byte total.
    fn ack(&mut self, follower_leo: u64) -> u64 {
        while let Some(&(upto, bytes)) = self.pending.front() {
            if upto > follower_leo {
                break;
            }
            self.total_bytes = self.total_bytes.saturating_sub(bytes);
            self.pending.pop_front();
        }
        self.total_bytes
    }

    fn total(&self) -> u64 {
        self.total_bytes
    }
}

/// Transport/engine failures `run_follower_stream` has no protocol-level
/// recovery for — the caller (manager.rs) decides whether/when to
/// reconnect (backoff 500 ms -> 5 s per PLAN-M2 §1b item 2), except for
/// `Detached`, which means "tear this down for good, never retry" (PLAN-M2
/// §4.1 A5).
#[derive(Debug, thiserror::Error)]
pub enum FollowerStreamError {
    #[error("replication codec error: {0}")]
    Codec(#[from] ReplCodecError),
    #[error("follower rejected Hello: {0:?}")]
    Rejected(ReplReject),
    #[error("engine error: {0}")]
    Engine(BusError),
    #[error("partition detached")]
    Detached,
    #[error("the follower did not answer Hello within {0:?}")]
    HelloAckTimeout(Duration),
    #[error("requested offset {requested} is below the earliest retained offset {earliest}")]
    OffsetOutOfRange { requested: u64, earliest: u64 },
    #[error("index floor invariant violated: follower leo {expected}, batch base_offset {got}")]
    OffsetInvariant { expected: u64, got: u64 },
    #[error("expected {expected} frame, got {got}")]
    UnexpectedFrame {
        expected: &'static str,
        got: &'static str,
    },
}

// W5 review round 2 finding 2: `pub(crate)` (not private) so `router.rs`'s
// `route_stream` can log a decoded-but-unexpected frame's KIND without
// formatting the whole `ReplFrame` (a `Batch { bytes: Bytes }` variant can
// carry up to `MAX_FRAME_BYTES` = 16 MiB — see `router.rs`'s own call site).
pub(crate) fn frame_kind_name(frame: &ReplFrame) -> &'static str {
    match frame {
        ReplFrame::Hello(_) => "Hello",
        ReplFrame::HelloAck(_) => "HelloAck",
        ReplFrame::Batch { .. } => "Batch",
        ReplFrame::Ack(_) => "Ack",
        ReplFrame::Heartbeat(_) => "Heartbeat",
        ReplFrame::Truncate(_) => "Truncate",
        ReplFrame::LeoQuery(_) => "LeoQuery",
        ReplFrame::LeoReply(_) => "LeoReply",
        ReplFrame::Offsets(_) => "Offsets",
        ReplFrame::ProduceForward(_) => "ProduceForward",
        ReplFrame::ProduceForwardReply(_) => "ProduceForwardReply",
        ReplFrame::ConsumeForward(_) => "ConsumeForward",
        ReplFrame::ConsumeForwardReply(_) => "ConsumeForwardReply",
        ReplFrame::TransferForward(_) => "TransferForward",
        ReplFrame::TransferForwardReply(_) => "TransferForwardReply",
    }
}

/// Fetches and sends every batch from `follower_cursor` up to this
/// partition's `log_end_offset` — i.e. including everything not yet committed,
/// which is precisely the point: the follower's ACK of those bytes is what
/// moves `high_watermark` (PLAN-M2 §4.1 vs. the consumer rule of §4.2, see
/// `PartitionReader::fetch_raw_to_end_of_log`) — looping until that read
/// returns empty (fully caught up). Driven from the initial catch-up before
/// the main select loop and from every subsequent `subscribe_leo` (or
/// `subscribe_hw`) wakeup. Returns the follower's new cursor position.
#[allow(clippy::too_many_arguments)]
async fn feed<W: AsyncWrite + Unpin>(
    leader: &PartitionLeader,
    reader: &PartitionReader,
    writer: &mut W,
    mut follower_cursor: u64,
    producer_mark: &Option<ProducerMarkLookup>,
    inflight: &mut InFlightTracker,
    last_frame_sent_at: &mut Instant,
    hw_sent: &mut u64,
) -> Result<u64, FollowerStreamError> {
    loop {
        let batches = match reader
            .fetch_raw_to_end_of_log(follower_cursor, leader.config().batch_fetch_max_bytes)
        {
            Ok(b) => b,
            Err(BusError::PartitionDetached) => return Err(FollowerStreamError::Detached),
            Err(BusError::OffsetOutOfRange {
                requested,
                earliest,
                ..
            }) => {
                return Err(FollowerStreamError::OffsetOutOfRange {
                    requested,
                    earliest,
                })
            }
            Err(e) => return Err(FollowerStreamError::Engine(e)),
        };
        if batches.is_empty() {
            return Ok(follower_cursor);
        }
        for raw in &batches {
            let base_offset = raw.base_offset;
            // `fetch_raw_to_end_of_log`'s floor semantics only ever return a
            // batch starting AT OR BEFORE the requested offset (mirrors
            // `fetch_from_offset`'s own doc); a follower's `leo` only ever
            // advances by whole batches (it is fed nothing else), so it
            // must always land exactly on a batch boundary here. A
            // mismatch means the follower's reported `leo` and this
            // leader's log have diverged in a way `Ack` bookkeeping alone
            // cannot explain.
            if base_offset != follower_cursor {
                return Err(FollowerStreamError::OffsetInvariant {
                    expected: follower_cursor,
                    got: base_offset,
                });
            }
            let wire_len = raw.bytes.len() as u64;
            let meta = producer_mark
                .as_ref()
                .map(|f| f(base_offset))
                .unwrap_or_default();
            let header_hw = leader.high_watermark();
            let header = ReplBatchHeader {
                leader_epoch: leader.epoch(),
                base_offset,
                hw: header_hw,
                batch_len: wire_len as u32,
                producer: meta.producer,
                dedup_keys: Vec::new(),
                committed: Some(leader.committed_offset()),
                record_epoch: Some(leader.epoch_at(base_offset)),
            };
            write_frame(
                writer,
                &ReplFrame::Batch {
                    header,
                    bytes: raw.bytes.clone(),
                },
            )
            .await?;
            inflight.record_sent(raw.next_offset, wire_len);
            follower_cursor = raw.next_offset;
            *last_frame_sent_at = Instant::now();
            *hw_sent = (*hw_sent).max(header_hw);
        }
    }
}

/// Sends a bare `Heartbeat` and returns the `hw` it carried. It carries the
/// leader's live in-sync set too, which `isr_sent` records: the follower has
/// no other way to know it.
/// Where a leader handle's shrink numbering starts: the wall clock in
/// milliseconds shifted left 16 bits, which leaves 65k shrinks per incarnation
/// before one could reach the next millisecond's base.
fn shrink_seq_base() -> u64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    ms << 16
}

async fn send_heartbeat<W: AsyncWrite + Unpin>(
    writer: &mut W,
    leader: &PartitionLeader,
    isr_sent: &mut Vec<String>,
) -> Result<u64, FollowerStreamError> {
    // Read before the set and the log end it describes: a heartbeat that
    // carries a shrink's number shows the leader after that shrink.
    let shrink_seq = leader.shrink_seq();
    let hw = leader.high_watermark();
    let isr = leader.isr_members();
    write_frame(
        writer,
        &ReplFrame::Heartbeat(ReplHeartbeat {
            leader_epoch: leader.epoch(),
            hw,
            leader_leo: leader.log_end_offset(),
            committed: Some(leader.committed_offset()),
            epoch_start: leader.term_start(),
            isr: Some(isr.clone()),
            shrink_seq,
        }),
    )
    .await?;
    *isr_sent = isr;
    isr_sent.sort();
    Ok(hw)
}

/// Drives one leader<->follower replication stream to completion (PLAN-M2
/// §1b item 2): sends `Hello`, validates `HelloAck`, registers the
/// follower with `leader`, then loops feeding batches (driven by
/// `Partition::subscribe_leo`, with `PartitionLeader::subscribe_hw` pushing
/// watermark advances no batch carries — see that method's doc), reading
/// `Ack`s, sending heartbeats in
/// silence, coalescing `ReplOffsets`, and forwarding `Truncate` requests
/// from `truncate_rx` — plus cutting a replica that reports a `leo` ahead of
/// this node's own back to its authority at handshake (see the `Truncate`
/// call in the body) — until a terminal condition (`FollowerStreamError`)
/// ends the stream. The caller (manager.rs, agent EL) owns reconnect
/// backoff and is expected to call this again with a fresh transport for
/// every `Err` other than `Detached`.
/// How long a leader waits for the `HelloAck` of a stream it opened.
pub const HELLO_ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run_follower_stream<R, W>(
    leader: Arc<PartitionLeader>,
    follower_node_id: String,
    reader: R,
    mut writer: W,
    producer_mark: Option<ProducerMarkLookup>,
    handled_offsets: Option<HandledOffsetsLookup>,
    mut truncate_rx: mpsc::UnboundedReceiver<u64>,
) -> Result<(), FollowerStreamError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Subscribed before `Hello` is even sent, not after `HelloAck` comes
    // back: `note_offset_commit`/`note_offset_discarded` broadcast to
    // whoever is subscribed AT SEND TIME (PLAN-M2 §1b item 2's channel is
    // a fan-out, not a replay log), so subscribing any later would leave
    // a real window — between this stream starting and the handshake
    // finishing — where a commit noted during that window is silently
    // dropped for this follower instead of merely delayed, which K-M2-5
    // does not allow.
    let mut offset_notes_rx = leader.subscribe_offset_notes();

    let hello = ReplHello {
        instance_id: leader.instance_id().to_string(),
        org_id: leader.org_id().to_string(),
        topic: leader.topic().to_string(),
        partition: leader.partition_id(),
        leader_node_id: leader.local_node_id().to_string(),
        leader_epoch: leader.epoch(),
        replicas: leader.replicas().to_vec(),
        environment: leader.environment(),
        topic_generation: Some(leader.topic_generation()),
        bus_features: LOCAL_BUS_FEATURES,
    };
    write_frame(&mut writer, &ReplFrame::Hello(hello)).await?;

    // One buffered reader for the stream's whole life: the ack intake below
    // sits in a `select!`, so its read must survive cancellation (see
    // `FrameReader`'s doc).
    let mut frames = FrameReader::new(reader);
    // Bounded: a stream whose peer never answers (a connection that took the
    // handshake and then carried nothing) would otherwise sit here until the
    // QUIC idle timeout killed it, 40 s, and then wait out the reconnect
    // backoff again. The receiving side answers or drops a stream within
    // `FIRST_FRAME_TIMEOUT` + `ASSIGNMENT_AWAIT`.
    let first = tokio::time::timeout(HELLO_ACK_TIMEOUT, frames.next_frame())
        .await
        .map_err(|_| FollowerStreamError::HelloAckTimeout(HELLO_ACK_TIMEOUT))??;
    let ack = match first {
        ReplFrame::HelloAck(a) => a,
        other => {
            return Err(FollowerStreamError::UnexpectedFrame {
                expected: "HelloAck",
                got: frame_kind_name(&other),
            })
        }
    };
    if !ack.accepted {
        return Err(FollowerStreamError::Rejected(
            ack.reject.unwrap_or(ReplReject::NotAReplica),
        ));
    }

    // K-M2-1 truncate-on-divergence, the half a promotion cannot reach:
    // `execute_promotion_actions`'s `SendTruncate` only ever lands on a
    // replica that was ALREADY dialled by the time this node was promoted
    // AND answered the election's `LeoQuery` — which is precisely what the
    // plan's own motivating replica is NOT (the old leader, down during the
    // election, rejoining later). Its divergence instead first becomes
    // visible here, in what its ack reports, and it is cut back BEFORE the
    // first feed by the election's own rule (`LogPosition::kept_end`):
    //
    // - a log written under this leader's epoch is a prefix or an extension
    //   of this chain; anything above our own `leo` is outside it, and
    //   feeding from there would fetch nothing while our next real append
    //   arrives as an `OffsetGap` — an endless reconnect loop;
    // - a log written under ANOTHER epoch may differ from this chain below
    //   our `leo` as well (an old leader's tail at the offsets this term
    //   wrote its own records to), which no offset comparison can see, so it
    //   goes back to its own committed offset — every record below that is
    //   on a majority, this chain included — and is fed again from there.
    //   Unless this chain holds records of the follower's last epoch: those
    //   are one leader's, so it agrees with this chain up to where they end
    //   (`LogPosition::kept_end_in`), and only what lies past that goes. A
    //   replica that was ahead of its committed offset by records this leader
    //   holds too is not cut back to be fed what it had.
    //
    // Epochs here are RECORD epochs (`Partition::record_epoch`) on both
    // sides, the epoch the last record was written under — not the term a
    // Hello stamped, and not a claim (`tentaflow_bus::epochs`): a claim holds
    // no record to diverge, and the first record fed at its offset replaces
    // it. A follower from before `follower_log_epoch` is judged by offsets
    // alone, one from before `follower_committed` by its `hw`.
    //
    // A replica whose committed offset is past our `leo` holds acknowledged
    // records this leader lacks. It refuses the `Truncate`
    // (`BusError::TruncateBelowCommitted`, whatever its `hw`) and the stream
    // ends; refusing to paper over that is deliberate — obeying would lose
    // those records on every replica.
    let authority = LogPosition {
        epoch: leader.record_epoch(),
        leo: leader.log_end_offset(),
        committed: leader.committed_offset(),
    };
    let follower_log = LogPosition {
        epoch: ack.follower_log_epoch.unwrap_or(authority.epoch),
        leo: ack.follower_leo,
        committed: ack.follower_committed.unwrap_or(ack.follower_hw),
    };
    let mut kept = follower_log.kept_end_in(&authority, leader.epoch_span(follower_log.epoch));
    // While the follower still holds its old tail, its acknowledgements repeat
    // its handshake position: a different one means the cut was applied.
    let mut cut_unapplied = None;
    if let Some(to) = leader.previous_incarnation_cut(&follower_node_id, &follower_log) {
        kept = kept.min(to);
        if kept < ack.follower_leo {
            cut_unapplied = Some(ack.follower_leo);
        }
    }
    let follower_leo = if kept < ack.follower_leo {
        write_frame(
            &mut writer,
            &ReplFrame::Truncate(ReplTruncate {
                leader_epoch: leader.epoch(),
                to_offset: kept,
            }),
        )
        .await?;
        kept
    } else {
        ack.follower_leo
    };

    leader.register_follower(
        follower_node_id.clone(),
        follower_leo,
        ack.follower_hw.min(follower_leo),
    );

    let reader_handle = leader.open_reader();
    let mut leo_rx = leader.subscribe_leo();
    let mut hw_rx = leader.subscribe_hw();

    let mut inflight = InFlightTracker::default();
    let mut pending_commits: Vec<(String, u32, u64, u32)> = Vec::new();
    let mut pending_discards: BTreeSet<u64> = BTreeSet::new();
    queue_handled_offsets(
        &handled_offsets,
        reader_handle.earliest_offset(),
        &mut pending_discards,
    )
    .await;
    // A replica that was away (or never saw an idle group's last commit)
    // would otherwise keep a stale offset until the group commits again.
    let mut pending_groups: Option<Vec<ReplGroup>> = None;
    let mut groups_seen: u64 = 0;
    let mut flush_rx = leader.flush_tx.subscribe();
    queue_group_snapshot(
        &leader,
        true,
        &mut pending_commits,
        &mut pending_groups,
        &mut groups_seen,
    )
    .await;
    let mut last_frame_sent_at = Instant::now();
    // The highest `hw` any frame on this stream has carried: the follower
    // applies it monotonically, so anything at or below it is old news.
    let mut hw_sent: u64 = 0;
    // The in-sync set the follower was last told (sorted), so a change in it
    // reaches the follower on the next ISR check, not only in a silence.
    let mut isr_sent: Vec<String> = leader.isr_members();
    isr_sent.sort();
    let mut truncate_open = true;

    let heartbeat_interval = leader.config().heartbeat_interval;
    // ISR thresholds are re-checked on the heartbeat cadence even while
    // frames flow; the heartbeat itself is scheduled from the last frame
    // sent (below), not from this ticker.
    let mut isr_check_ticker = tokio::time::interval(heartbeat_interval);
    isr_check_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut offsets_ticker = tokio::time::interval(leader.config().offsets_coalesce_interval);
    offsets_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // `subscribe_leo` only fires on FUTURE advances — a follower joining
    // an already-ahead leader needs one catch-up feed before the select
    // loop starts, or it would wait for the next unrelated append.
    let mut follower_cursor = feed(
        &leader,
        &reader_handle,
        &mut writer,
        follower_leo,
        &producer_mark,
        &mut inflight,
        &mut last_frame_sent_at,
        &mut hw_sent,
    )
    .await?;
    leader.set_follower_lag_bytes(&follower_node_id, inflight.total());

    loop {
        // Deliberately NOT `biased`: `heartbeat_interval` and
        // `offsets_coalesce_interval` default to the same duration
        // (PLAN-M2 §1b, both 500 ms), so the timer arms become ready at
        // nearly the same wall-clock moment on every cycle. A `biased`
        // select checks arms top-to-bottom and would let the heartbeat
        // arm systematically win that race every time, starving
        // `ReplOffsets` forever whenever the two intervals coincide.
        // Unbiased `select!` picks fairly among whichever arms are ready —
        // which is exactly why the ack read below must be cancel-safe.
        let heartbeat_due = last_frame_sent_at + heartbeat_interval;
        tokio::select! {
            changed = leo_rx.changed() => {
                if changed.is_err() {
                    // The `Partition` (and every sender clone) is gone —
                    // nothing left to feed.
                    return Ok(());
                }
                follower_cursor = feed(
                    &leader, &reader_handle, &mut writer, follower_cursor,
                    &producer_mark, &mut inflight, &mut last_frame_sent_at, &mut hw_sent,
                ).await?;
                leader.set_follower_lag_bytes(&follower_node_id, inflight.total());
            }

            // The advance is usually the quorum ack of the batch this stream
            // just sent, so there is nothing new to feed and no frame would
            // carry the new `hw` until the next heartbeat: measured in the
            // three-process harness as leader `hw` 1000 at T, followers
            // applying it via the heartbeat at T+501 ms. A follower's `hw`
            // is what it may serve after a promotion and what it reports as
            // committed, so it is pushed now as a bare heartbeat — only when
            // no batch already carried it, so a stream that is still
            // catching up sends nothing extra.
            hw_changed = hw_rx.changed() => {
                if hw_changed.is_ok() {
                    follower_cursor = feed(
                        &leader, &reader_handle, &mut writer, follower_cursor,
                        &producer_mark, &mut inflight, &mut last_frame_sent_at, &mut hw_sent,
                    ).await?;
                    leader.set_follower_lag_bytes(&follower_node_id, inflight.total());
                    if leader.high_watermark() > hw_sent {
                        hw_sent = send_heartbeat(&mut writer, &leader, &mut isr_sent).await?;
                        last_frame_sent_at = Instant::now();
                    }
                }
            }

            frame = frames.next_frame() => {
                match frame? {
                    ReplFrame::Ack(a) => {
                        if cut_unapplied.is_some_and(|held| a.follower_leo != held) {
                            leader.note_cut_applied(&follower_node_id);
                            cut_unapplied = None;
                        }
                        let lag = inflight.ack(a.follower_leo);
                        leader.record_ack(&follower_node_id, &a, lag);
                        match a.groups_applied {
                            Some(applied) => {
                                leader.mark_groups_applied(&follower_node_id, applied)
                            }
                            None => {
                                leader.groups_legacy.insert(follower_node_id.clone());
                            }
                        }
                    }
                    other => {
                        return Err(FollowerStreamError::UnexpectedFrame {
                            expected: "Ack",
                            got: frame_kind_name(&other),
                        });
                    }
                }
            }

            note = offset_notes_rx.recv() => {
                match note {
                    Ok(OffsetNote::Commit { group, offset, attempts }) => {
                        pending_commits.push((group, leader.partition_id(), offset, attempts));
                    }
                    Ok(OffsetNote::Discarded { offset }) => {
                        pending_discards.insert(offset);
                    }
                    Ok(OffsetNote::GroupsChanged) => {
                        queue_group_snapshot(
                            &leader, false, &mut pending_commits, &mut pending_groups, &mut groups_seen,
                        )
                        .await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // K-M2-5 already tolerates a bounded replication
                        // delay; a burst that outran this channel's
                        // capacity just merges into the next coalesced
                        // frame instead of being replayed note-by-note. A
                        // later commit supersedes a lost one, a lost
                        // handled-record marker nothing would resend — the
                        // whole marker set goes out again instead.
                        queue_handled_offsets(
                            &handled_offsets,
                            reader_handle.earliest_offset(),
                            &mut pending_discards,
                        )
                        .await;
                        // The same goes for commits and group rows: the
                        // lost notes are not replayed, the state is re-read.
                        queue_group_snapshot(
                            &leader, true, &mut pending_commits, &mut pending_groups, &mut groups_seen,
                        )
                        .await;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // `leader` outlives this task in every real
                        // caller; treat this as "no more offset traffic"
                        // rather than tearing the stream down over it.
                    }
                }
            }

            to_offset = truncate_rx.recv(), if truncate_open => {
                match to_offset {
                    Some(to_offset) => {
                        write_frame(&mut writer, &ReplFrame::Truncate(ReplTruncate {
                            leader_epoch: leader.epoch(),
                            to_offset,
                        })).await?;
                        last_frame_sent_at = Instant::now();
                    }
                    None => truncate_open = false,
                }
            }

            _ = isr_check_ticker.tick() => {
                leader.reconcile_follower(&follower_node_id);
                let mut isr_now = leader.isr_members();
                isr_now.sort();
                if isr_now != isr_sent {
                    hw_sent = send_heartbeat(&mut writer, &leader, &mut isr_sent).await?;
                    last_frame_sent_at = Instant::now();
                }
            }

            // PLAN-M2 §1b "co 500 ms w ciszy": due exactly `heartbeat_interval`
            // after the last frame of any kind. A fixed ticker that skipped a
            // tick whenever a frame had gone out less than one interval ago
            // stretched real silences to almost two intervals.
            _ = tokio::time::sleep_until(heartbeat_due) => {
                hw_sent = send_heartbeat(&mut writer, &leader, &mut isr_sent).await?;
                last_frame_sent_at = Instant::now();
            }

            _ = offsets_ticker.tick() => {
                send_pending_offsets(
                    &leader, &mut writer, &mut pending_commits, &mut pending_discards,
                    &mut pending_groups, groups_seen, &mut last_frame_sent_at,
                ).await?;
                if pending_groups.is_none() {
                    leader.mark_groups_sent(&follower_node_id, groups_seen);
                }
            }

            changed = flush_rx.changed() => {
                if changed.is_ok() {
                    send_pending_offsets(
                        &leader, &mut writer, &mut pending_commits, &mut pending_discards,
                        &mut pending_groups, groups_seen, &mut last_frame_sent_at,
                    ).await?;
                    if pending_groups.is_none() {
                        leader.mark_groups_sent(&follower_node_id, groups_seen);
                    }
                }
            }
        }
    }
}

/// Sends the commits, handled-record markers and group rows a stream has
/// queued, in as many `Offsets` frames as the discards need.
async fn send_pending_offsets<W: AsyncWrite + Unpin>(
    leader: &Arc<PartitionLeader>,
    writer: &mut W,
    pending_commits: &mut Vec<(String, u32, u64, u32)>,
    pending_discards: &mut BTreeSet<u64>,
    pending_groups: &mut Option<Vec<ReplGroup>>,
    groups_seen: u64,
    last_frame_sent_at: &mut Instant,
) -> Result<(), FollowerStreamError> {
    if !pending_commits.is_empty() || !pending_discards.is_empty() || pending_groups.is_some() {
        let partition = leader.partition_id();
        let discards: Vec<(u32, u64)> = std::mem::take(pending_discards)
            .into_iter()
            .map(|offset| (partition, offset))
            .collect();
        let mut commits = std::mem::take(pending_commits);
        let mut groups_gen = if pending_groups.is_some() {
            groups_seen
        } else {
            0
        };
        let mut groups = pending_groups.take().unwrap_or_default();
        let mut rest = discards.as_slice();
        loop {
            let (chunk, tail) = rest.split_at(rest.len().min(DISCARDS_PER_OFFSETS_FRAME));
            write_frame(
                writer,
                &ReplFrame::Offsets(ReplOffsets {
                    leader_epoch: leader.epoch(),
                    commits: std::mem::take(&mut commits),
                    discarded: chunk.to_vec(),
                    groups: std::mem::take(&mut groups),
                    groups_gen: std::mem::take(&mut groups_gen),
                }),
            )
            .await?;
            rest = tail;
            if rest.is_empty() {
                break;
            }
        }
        *last_frame_sent_at = Instant::now();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    use bytes::Bytes;
    use tentaflow_bus::{BatchBuilder, Durability, HwTracking, RecordInput, RollPolicy};

    use tokio::io::AsyncWriteExt;

    use super::super::election::choose_candidate;
    use super::super::frames::{read_frame, ReplHelloAck};

    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_partition(label: &str) -> Partition {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tentaflow-repl-leader-test-{}-{}-{}",
            std::process::id(),
            label,
            n
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 8)
            .expect("open partition")
    }

    fn one_record_batch(epoch: u32) -> Bytes {
        let mut b = BatchBuilder::new(0, epoch);
        b.push(RecordInput::new(Bytes::from_static(b"x"), 0))
            .unwrap();
        b.build().unwrap()
    }

    const TEST_EPOCH: u32 = 7;

    fn make_leader(
        part: Partition,
        replicas: &[&str],
        acks: Acks,
        config: LeaderConfig,
    ) -> Arc<PartitionLeader> {
        Arc::new(PartitionLeader::new(
            "tentabus-00000001",
            "org-1",
            "topic-1",
            0,
            "leader",
            replicas.iter().map(|s| s.to_string()).collect(),
            TEST_EPOCH,
            0,
            acks,
            NodeEnvironment::Prod,
            part,
            config,
            Arc::new(LeaderMetrics::new()),
        ))
    }

    fn fast_config() -> LeaderConfig {
        LeaderConfig {
            heartbeat_interval: Duration::from_millis(30),
            offsets_coalesce_interval: Duration::from_millis(30),
            replica_lag_max_bytes: 64 * 1024 * 1024,
            replica_lag_max_ms: 5_000,
            batch_fetch_max_bytes: 1024 * 1024,
            ..LeaderConfig::default()
        }
    }

    type Half = (
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    );

    fn split_duplex() -> (Half, Half) {
        let (a, b) = tokio::io::duplex(1024 * 1024);
        (tokio::io::split(a), tokio::io::split(b))
    }

    // ===== handshake =====

    /// A peer that takes the stream and never answers frees it after the
    /// handshake bound, not after the transport's idle timeout.
    #[tokio::test(start_paused = true)]
    async fn a_follower_that_never_answers_hello_ends_the_stream_at_the_handshake_bound() {
        let part = temp_partition("silent-ack");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, _foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        assert!(matches!(
            read_frame(&mut foll_r).await.unwrap(),
            ReplFrame::Hello(_)
        ));
        let started = tokio::time::Instant::now();
        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(FollowerStreamError::HelloAckTimeout(_))),
            "{result:?}"
        );
        assert!(started.elapsed() <= HELLO_ACK_TIMEOUT + Duration::from_secs(1));
    }

    #[tokio::test]
    async fn hello_ack_handshake_accepts_and_registers_follower() {
        let part = temp_partition("handshake");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let hello = match read_frame(&mut foll_r).await.unwrap() {
            ReplFrame::Hello(h) => h,
            other => panic!("expected Hello, got {other:?}"),
        };
        assert_eq!(hello.org_id, "org-1");
        assert_eq!(hello.topic, "topic-1");
        assert_eq!(hello.partition, 0);
        assert_eq!(hello.leader_epoch, TEST_EPOCH);
        assert_eq!(hello.environment, NodeEnvironment::Prod);

        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        // Give the leader task a moment to process the accept and
        // register the follower before asserting on shared state.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let fs = leader.follower_state("f1").expect("follower registered");
        assert_eq!(fs.leo, 0);
        assert!(fs.in_isr);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn rejected_hello_ack_ends_the_stream_without_registering() {
        let part = temp_partition("reject");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: false,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: Some(ReplReject::StaleEpoch { have: TEST_EPOCH }),
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, FollowerStreamError::Rejected(ReplReject::StaleEpoch { have }) if have == TEST_EPOCH)
        );
        assert!(leader.follower_state("f1").is_none());
    }

    /// K-M2-1, handshake half: a replica that reports a `leo` past the
    /// leader's own is cut back to that authority BEFORE the first feed.
    /// Without it the follower's claimed `leo` becomes the feed cursor, the
    /// fetch returns nothing, and the leader's next real append arrives at
    /// that replica as an `OffsetGap` — the stream reconnects forever instead
    /// of converging. This is the only truncate path for a replica a
    /// promotion could not reach (`execute_promotion_actions` can only
    /// `SendTruncate` to a stream it already opened).
    #[tokio::test]
    async fn a_follower_ahead_of_the_leader_is_truncated_before_the_first_feed() {
        let part = temp_partition("rejoin-truncate");
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        assert_eq!(part.log_end_offset(), 2);

        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 5, // three records past the leader's authority
                follower_hw: 2,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for the divergence Truncate")
            .unwrap();
        match frame {
            ReplFrame::Truncate(t) => {
                assert_eq!(t.to_offset, 2, "cut back to the leader's own leo");
                assert_eq!(t.leader_epoch, TEST_EPOCH);
            }
            other => panic!("expected Truncate, got {other:?}"),
        }

        // Registered at the clamped offset, not the one it claimed — the
        // feed cursor and every hw computation downstream follow from this.
        let fs = leader
            .follower_state("f1")
            .expect("follower registered at handshake");
        assert_eq!(fs.leo, 2);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// An old leader's tail sits BELOW this leader's `leo`: the follower's
    /// log was written under another epoch, with records of its own at
    /// offsets this term filled differently. Offsets alone see nothing to
    /// cut; the log epoch sends it back to its own `hw`, to be fed again.
    #[tokio::test]
    async fn a_follower_of_another_epoch_is_cut_back_to_its_hw_even_below_the_leaders_leo() {
        let part = temp_partition("rejoin-epoch");
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        assert_eq!(part.log_end_offset(), 3);

        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 2,
                follower_hw: 1,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: Some(TEST_EPOCH - 1),
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for the divergence Truncate")
            .unwrap();
        match frame {
            ReplFrame::Truncate(t) => {
                assert_eq!(t.to_offset, 1, "cut back to the follower's own hw");
                assert_eq!(t.leader_epoch, TEST_EPOCH);
            }
            other => panic!("expected Truncate, got {other:?}"),
        }
        let fs = leader
            .follower_state("f1")
            .expect("follower registered at handshake");
        assert_eq!(fs.leo, 1);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// A replica whose last epoch is one this leader's chain holds keeps what
    /// it has of it: those records are the same ones, written by the same
    /// leader, and cutting them back to its committed offset only to feed them
    /// again reads as a loss that did not happen.
    #[tokio::test]
    async fn a_follower_ahead_of_its_hw_by_records_the_leader_holds_too_is_not_cut() {
        let kept = handshake_with_older_epoch_follower(2, 0).await;
        assert_eq!(kept, None, "records 0..2 are this chain's: nothing to cut");
    }

    #[tokio::test]
    async fn a_follower_with_records_past_its_epochs_end_in_the_leader_is_cut_to_that_end() {
        let kept = handshake_with_older_epoch_follower(3, 0).await;
        assert_eq!(kept, Some(2), "record 2 was another chain's");
    }

    /// The leader holds epochs `TEST_EPOCH - 2` at offsets 0..2 and
    /// `TEST_EPOCH` at 2..4. A follower whose last epoch is the first one
    /// reports `follower_leo` and `follower_committed`; the offset the leader
    /// cut it to, `None` when it sent no `Truncate`.
    async fn handshake_with_older_epoch_follower(
        follower_leo: u64,
        follower_committed: u64,
    ) -> Option<u64> {
        let part = temp_partition("rejoin-epoch-span");
        part.set_leader_epoch(TEST_EPOCH - 2).unwrap();
        for _ in 0..2 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        for _ in 0..2 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        assert_eq!(part.log_end_offset(), 4);

        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo,
                follower_hw: follower_committed,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: Some(TEST_EPOCH - 2),
                follower_committed: Some(follower_committed),
                bus_features: 0,
            }),
        )
        .await
        .unwrap();
        // Registered at what the handshake kept, before any feeding.
        let registered = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(fs) = leader.follower_state("f1") {
                    return fs.leo;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the follower was never registered");
        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
        (registered < follower_leo).then_some(registered)
    }

    /// A leader that restarts at the same epoch lost its unflushed tail and
    /// wrote new records at those offsets; a follower that holds the old tail
    /// has other contents at the same `(epoch, offset)`. It is cut to what the
    /// restarted leader started with — at its first handshake with this
    /// incarnation only: after that its records are this leader's own.
    #[tokio::test]
    async fn a_follower_holding_the_tail_a_restarted_leader_lost_is_cut_to_where_it_restarted() {
        let part = temp_partition("restart-same-epoch");
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        for _ in 0..2 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        // The restart: offsets 0..2 survived, the old tail at 2..4 did not.
        let leader = make_leader(part.clone(), &["leader", "f1"], Acks::Quorum, fast_config());
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        assert_eq!(part.log_end_offset(), 5);

        // The stream drops before the follower says it applied the cut: it
        // may still hold the old tail, so the next handshake cuts it again.
        let first = same_epoch_handshake(&leader, 4, None).await;
        assert_eq!(first, Some(2), "records 2..4 were the old incarnation's");
        let unproven = same_epoch_handshake(&leader, 4, None).await;
        assert_eq!(unproven, Some(2), "nothing showed the cut was applied");
        // An acknowledgement of a position below where the tail began shows it.
        let applied = same_epoch_handshake(&leader, 4, Some(2)).await;
        assert_eq!(applied, Some(2));
        let again = same_epoch_handshake(&leader, 4, None).await;
        assert_eq!(
            again, None,
            "on a reconnect the follower's records are this incarnation's"
        );
    }

    /// One handshake of `f1`, which reports `follower_leo` records all
    /// written under the leader's term; the offset the leader cut it to.
    /// `acked`: the position the follower acknowledges once it has the cut.
    async fn same_epoch_handshake(
        leader: &Arc<PartitionLeader>,
        follower_leo: u64,
        acked: Option<u64>,
    ) -> Option<u64> {
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: Some(TEST_EPOCH),
                follower_committed: Some(0),
                bus_features: 0,
            }),
        )
        .await
        .unwrap();
        let registered = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(fs) = leader.follower_state("f1") {
                    return fs.leo;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the follower was never registered");
        if let Some(position) = acked {
            write_frame(
                &mut foll_w,
                &ReplFrame::Ack(ReplAck {
                    leader_epoch: TEST_EPOCH,
                    follower_leo: position,
                    follower_hw: 0,
                    follower_log_epoch: Some(TEST_EPOCH),
                    groups_applied: None,
                }),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !leader.fed.contains("f1") {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the acknowledgement never reached the leader");
        }
        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
        leader.remove_follower("f1", "test");
        (registered < follower_leo).then_some(registered)
    }

    /// A re-elected leader's claim on its term holds no record: a follower
    /// whose last record shares the leader's last record's epoch is its
    /// chain as far as it goes and keeps every record. Reconciled against
    /// the claim instead, it would be cut back to its committed offset on
    /// every failover. The heartbeat then tells it where the term begins.
    #[tokio::test]
    async fn the_leaders_claim_does_not_cut_a_follower_on_its_chain() {
        // Sync appends block on the writer; off the runtime.
        let (part, leader) = tokio::task::spawn_blocking(re_elected_over_a_stale_later_term)
            .await
            .unwrap();
        leader.remove_follower("f2", "handshake below");
        assert_eq!(
            (part.log_epoch(), part.record_epoch()),
            (TEST_EPOCH, TEST_EPOCH - 2)
        );
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f2".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 3,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: Some(TEST_EPOCH - 2),
                follower_committed: Some(0),
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for the first frame")
            .unwrap();
        match frame {
            ReplFrame::Heartbeat(hb) => assert_eq!(hb.epoch_start, Some(3)),
            other => panic!("expected a Heartbeat, got {other:?}"),
        }
        assert_eq!(leader.follower_state("f2").expect("registered").leo, 3);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// The quorum lease lapses on its own once acks stop — no stream task
    /// has to notice and shrink the ISR first.
    #[tokio::test]
    async fn the_quorum_lease_needs_a_majority_of_recent_acks() {
        let part = temp_partition("quorum-lease");
        let mut config = fast_config();
        config.replica_lag_max_ms = 50;
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, config);
        assert!(
            !leader.has_quorum_lease(),
            "the leader alone is not a majority of three"
        );
        leader.register_follower("f1", 0, 0);
        assert!(leader.has_quorum_lease());
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            !leader.has_quorum_lease(),
            "a follower silent past the lag window no longer counts"
        );
    }

    /// `acks=all` over an ISR shrunk to the leader alone must not let `hw`
    /// — what consumers read — run past what a majority holds: a failover
    /// keeps only that.
    #[tokio::test]
    async fn acks_all_never_advances_hw_past_a_majority() {
        let part = temp_partition("acks-all-majority");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(
            part.clone(),
            &["leader", "f1", "f2"],
            Acks::All,
            fast_config(),
        );
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        leader.register_follower("f1", 0, 0);
        leader.remove_follower("f1", "stream ended");
        assert_eq!(
            leader.high_watermark(),
            0,
            "the leader alone is not a majority of three"
        );
    }

    /// The committed offset is majority-derived whatever `acks` says:
    /// `acks=leader` shows consumers everything the leader wrote, but only
    /// what a follower acknowledged too is committed — the bound replicas
    /// are reconciled to.
    #[tokio::test]
    async fn the_committed_offset_follows_a_majority_even_under_acks_leader() {
        let part = temp_partition("committed-acks-leader");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(
            part.clone(),
            &["leader", "f1", "f2"],
            Acks::Leader,
            fast_config(),
        );
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        leader.register_follower("f1", 1, 0);
        assert_eq!(leader.high_watermark(), 3);
        assert_eq!(leader.committed_offset(), 1);
        leader.register_follower("f2", 3, 0);
        assert_eq!(leader.committed_offset(), 3);
    }

    /// Figure 8: re-elected in a new term, this leader feeds a follower
    /// records of the earlier term. A majority holding them does not make
    /// them committed — a later-term log can still win over those replicas —
    /// until a record of the leader's own term is on a majority too.
    #[tokio::test]
    async fn earlier_term_records_are_not_committed_until_a_current_term_record_is() {
        let part = temp_partition("figure-eight");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH - 1).unwrap();
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(
            part.clone(),
            &["leader", "f1", "f2"],
            Acks::Quorum,
            fast_config(),
        );
        leader.register_follower("f1", 3, 0);
        assert_eq!(
            leader.committed_offset(),
            0,
            "earlier-term records on a majority are not committed by it"
        );

        part.append_batch_async(one_record_batch(1)).await.unwrap();
        leader.register_follower("f1", 4, 0);
        assert_eq!(leader.committed_offset(), 4);
    }

    /// Term 5 wrote 0..3 on the leader and on f2 but committed none of it;
    /// f1 was elected in term 6 while they were away and wrote records of
    /// its own at those offsets, then went away itself. The leader, back and
    /// re-elected in term 7 with f2, has written nothing in its term: a
    /// majority holds 0..3 and a follower acknowledges all of it.
    fn re_elected_over_a_stale_later_term() -> (Partition, Arc<PartitionLeader>) {
        let part = temp_partition("figure-eight-visibility");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH - 2).unwrap();
        for _ in 0..3 {
            part.append_batch(one_record_batch(1)).unwrap();
        }
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(
            part.clone(),
            &["leader", "f1", "f2"],
            Acks::Quorum,
            fast_config(),
        );
        // What the serving handle stamps when it starts serving.
        assert!(part.confirm_epoch(TEST_EPOCH, 3).unwrap());
        leader.register_follower("f2", 0, 0);
        (part, leader)
    }

    fn position(epoch: u32, leo: u64, committed: u64) -> LogPosition {
        LogPosition {
            epoch,
            leo,
            committed,
        }
    }

    fn elect(logs: &[(&str, LogPosition)]) -> Option<String> {
        let isr: Vec<String> = logs.iter().map(|(id, _)| id.to_string()).collect();
        let logs: Vec<(String, LogPosition)> = logs
            .iter()
            .map(|(id, log)| (id.to_string(), *log))
            .collect();
        choose_candidate(&isr, &logs, "none")
    }

    /// The reviewer's scenario: were `hw` to cover 0..3 now, a consumer
    /// would read them, the leader could die before a record of its own
    /// term reached a majority, and f1's term-6 log would win over f2's
    /// term-5 one and cut them. `acks=quorum` promises a record read is a
    /// record kept, so they stay hidden until committed.
    #[test]
    fn earlier_term_records_on_a_majority_stay_hidden_until_committed() {
        let (_part, leader) = re_elected_over_a_stale_later_term();
        leader.record_ack(
            "f2",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 3,
                follower_hw: 0,
                follower_log_epoch: Some(TEST_EPOCH - 2),
                groups_applied: None,
            },
            0,
        );
        assert_eq!(leader.committed_offset(), 0);
        let visible = leader.high_watermark();

        // The leader dies; f1 and f2 elect.
        let f1 = position(TEST_EPOCH - 1, 5, 0);
        let f2 = position(TEST_EPOCH - 2, 3, 0);
        assert_eq!(elect(&[("f1", f1), ("f2", f2)]).as_deref(), Some("f1"));
        let kept_on_f2 = f2.kept_end(&f1);
        assert!(
            visible <= kept_on_f2,
            "consumers saw offsets below {visible}, the winner keeps only {kept_on_f2} of them"
        );
    }

    /// The same partition left idle: nobody publishes, so no record of the
    /// leader's term ever lands. f2 confirming the term (its log ends where
    /// the term begins) is what commits 0..3 — and then f2 outranks f1, so
    /// what consumers now read is what any next leader keeps.
    #[test]
    fn a_majority_confirming_the_term_shows_earlier_term_records_without_a_publish() {
        let (_part, leader) = re_elected_over_a_stale_later_term();
        leader.record_ack(
            "f2",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 3,
                follower_hw: 0,
                follower_log_epoch: Some(TEST_EPOCH),
                groups_applied: None,
            },
            0,
        );
        assert_eq!(leader.committed_offset(), 3);
        assert_eq!(leader.high_watermark(), 3);

        let f1 = position(TEST_EPOCH - 1, 5, 0);
        let f2 = position(TEST_EPOCH, 3, 3);
        assert_eq!(elect(&[("f1", f1), ("f2", f2)]).as_deref(), Some("f2"));
    }

    /// A confirmation below where the term begins, of another term — an
    /// earlier one, or a later leader's — or from a follower that predates
    /// the field is no confirmation.
    #[test]
    fn only_a_confirmation_of_this_term_at_its_start_commits() {
        let (_part, leader) = re_elected_over_a_stale_later_term();
        for (leo, log_epoch) in [
            (2, Some(TEST_EPOCH)),
            (3, Some(TEST_EPOCH - 1)),
            (3, Some(TEST_EPOCH + 1)),
            (3, None),
        ] {
            leader.register_follower("f2", 0, 0);
            leader.record_ack(
                "f2",
                &ReplAck {
                    leader_epoch: TEST_EPOCH,
                    follower_leo: leo,
                    follower_hw: 0,
                    follower_log_epoch: log_epoch,
                    groups_applied: None,
                },
                0,
            );
            assert_eq!(
                (leader.committed_offset(), leader.high_watermark()),
                (0, 0),
                "leo {leo}, log epoch {log_epoch:?}"
            );
        }
    }

    /// `acks=leader` shows consumers the leader's own log by definition, and
    /// an RF=2 leader alone must not wait for a majority it cannot have.
    #[test]
    fn earlier_term_records_are_not_held_back_where_no_majority_is_promised() {
        let old_term_log = |label: &str| {
            let part = temp_partition(label);
            part.set_hw_tracking(HwTracking::Manual);
            part.set_leader_epoch(TEST_EPOCH - 2).unwrap();
            for _ in 0..3 {
                part.append_batch(one_record_batch(1)).unwrap();
            }
            part.set_leader_epoch(TEST_EPOCH).unwrap();
            part
        };
        let leader = make_leader(
            old_term_log("figure-eight-acks-leader"),
            &["leader", "f1", "f2"],
            Acks::Leader,
            fast_config(),
        );
        leader.register_follower("f2", 0, 0);
        assert_eq!(leader.high_watermark(), 3);

        let leader = make_leader(
            old_term_log("figure-eight-rf2"),
            &["leader", "f1"],
            Acks::All,
            fast_config(),
        );
        leader.register_follower("f1", 0, 0);
        leader.remove_follower("f1", "stream ended");
        assert!(leader.grant_alone_if_unshrunk(leader.shrink_seq()));
        assert_eq!(leader.high_watermark(), 3, "the lone RF=2 survivor serves");
    }

    // ===== feeding =====

    #[tokio::test]
    async fn batches_arrive_in_order_with_correct_offsets_and_epoch() {
        let part = temp_partition("order");
        // `append_batch`'s sync path blocks on the writer thread's ack via
        // `blocking_recv`, which panics when called from inside a Tokio
        // runtime (this is a `#[tokio::test]`) — the async twin is the
        // correct one to use here, matching `append_batch_async`'s own doc.
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        assert_eq!(part.log_end_offset(), 3);

        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        for expected_offset in 0u64..3 {
            let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
                .await
                .expect("timed out waiting for batch")
                .unwrap();
            match frame {
                ReplFrame::Batch { header, .. } => {
                    assert_eq!(header.base_offset, expected_offset);
                    assert_eq!(header.leader_epoch, TEST_EPOCH);
                }
                other => panic!("expected Batch, got {other:?}"),
            }
        }

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// The wave-3 feed blocker, unit-sized: on an `acks=quorum` partition every
    /// replicated partition runs `HwTracking::Manual` (`glue.rs`) and
    /// `recompute_hw` moves `hw` only from ACKs and ISR bookkeeping, so the
    /// leader's own appends NEVER commit anything by themselves. The feeder used
    /// to read through `PartitionReader::fetch_raw_from_offset` — bounded by
    /// `high_watermark` — and so sent nothing, which meant nothing was ever
    /// ACKed, which meant `hw` never moved: the loop closed over itself and a
    /// quorum topic replicated nothing that was ever published (three-node
    /// suite, `publish_through_leader_replicates_byte_identical_and_hw_follows`,
    /// `AckTimeout { acked: 1, required: 2 }` on the FIRST publish, 4/4 runs).
    ///
    /// `feed` reads `fetch_raw_to_end_of_log` now, so the assert that matters
    /// here is the uncomfortable-looking one: both batches must reach the
    /// follower while this leader's `hw` is still `0`. The second half closes
    /// the loop the other way — the follower's ACK of that uncommitted data is
    /// what commits it.
    #[tokio::test]
    async fn a_leader_appended_record_is_fed_before_any_high_watermark_advance() {
        let part = temp_partition("quorum-feed");
        part.set_hw_tracking(HwTracking::Manual);
        // Records of this leader's own term, as it writes them.
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        // `Acks::Quorum` over 3 replicas: `min_isr` is 2, and with only the
        // leader holding data `nth_largest([leo, ..zeros], 2)` is 0 — nothing
        // this test does before the ACK can commit anything.
        let leader = make_leader(
            part.clone(),
            &["leader", "f1", "late"],
            Acks::Quorum,
            fast_config(),
        );
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        part.append_batch_async(one_record_batch(1)).await.unwrap();
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        assert_eq!(part.log_end_offset(), 2);
        assert_eq!(
            part.high_watermark(),
            0,
            "precondition: a quorum leader has committed nothing on its own"
        );

        // A local append is the only wake this partition ever gets, and it must
        // be enough on its own.
        let mut fed = Vec::new();
        while fed.len() < 2 {
            let frame = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut foll_r))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "an uncommitted record was never fed (the wave-3 blocker): \
                         got {fed:?}, leo={}, hw={}",
                        part.log_end_offset(),
                        part.high_watermark(),
                    )
                })
                .unwrap();
            match frame {
                // Silence frames carry no data and are not evidence.
                ReplFrame::Heartbeat(_) => {}
                ReplFrame::Batch { header, .. } => fed.push(header.base_offset),
                other => panic!("expected Batch, got {other:?}"),
            }
        }
        assert_eq!(fed, vec![0, 1]);
        assert_eq!(
            part.high_watermark(),
            0,
            "the batches above must have been fed while still uncommitted"
        );

        // ...and the ACK they enable is what finally commits them.
        write_frame(
            &mut foll_w,
            &ReplFrame::Ack(ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 2,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            }),
        )
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while part.high_watermark() < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the follower's ACK of offset 2 never moved the leader's hw: hw={}",
                part.high_watermark()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(leader.high_watermark(), 2);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    // ===== ack accounting / hw =====

    #[test]
    fn hw_advances_for_leader_acks_immediately_from_local_leo() {
        let part = temp_partition("hw-leader");
        part.append_batch(one_record_batch(1)).unwrap();
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Leader, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        // acks=Leader only ever requires the leader's own leo — no ack
        // needed from either follower.
        assert_eq!(leader.high_watermark(), 2);
    }

    // NOTE on `commit_offset_for` vs `high_watermark()` below: this
    // engine build (tentaflow-bus, wave 0) still auto-advances
    // `high_watermark` to `log_end_offset` on every LOCAL append
    // regardless of any `ReplicationCoordinator` — gating that auto-bump
    // behind partition role is explicitly documented as out of scope for
    // this wave's frozen contract (`Partition::set_high_watermark`'s own
    // doc). `Partition::set_high_watermark` is monotonic (`fetch_max`), so
    // once the engine's own auto-bump has already pushed `high_watermark`
    // to `log_end_offset`, this leader's own (lower, not-yet-quorum-met)
    // candidate can never be observed through `high_watermark()` — it is
    // clamped away by a `fetch_max` against a value already higher. These
    // two tests exercise `commit_offset_for` (this file's actual quorum
    // computation) directly instead, which is unaffected by that engine
    // stub; `hw_advances_for_leader_acks_immediately_from_local_leo` above
    // does not need this because acks=Leader's threshold is always exactly
    // the leader's own leo, which coincides with the engine's auto-bumped
    // value.
    #[test]
    fn hw_advances_for_quorum_acks_after_min_isr_acks() {
        let part = temp_partition("hw-quorum");
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        assert_eq!(leader.min_isr(), 2);
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        // Nobody but the leader has reached offset 1 yet: quorum (2) is
        // not met.
        assert_eq!(leader.commit_offset_for(Acks::Quorum), 0);

        leader.record_ack(
            "f1",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 1,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            },
            0,
        );
        // Leader + f1 = 2 = min_isr: quorum met.
        assert_eq!(leader.commit_offset_for(Acks::Quorum), 1);
    }

    #[test]
    fn hw_advances_for_all_acks_only_after_every_isr_member_acks() {
        let part = temp_partition("hw-all");
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::All, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);

        leader.record_ack(
            "f1",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 1,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            },
            0,
        );
        assert_eq!(
            leader.commit_offset_for(Acks::All),
            0,
            "f2 has not acked yet"
        );

        leader.record_ack(
            "f2",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 1,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            },
            0,
        );
        assert_eq!(leader.commit_offset_for(Acks::All), 1);
    }

    // ===== await_acks =====

    #[test]
    fn await_acks_resolves_once_enough_followers_ack() {
        let part = temp_partition("await-resolve");
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);

        let leader2 = leader.clone();
        let acker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            leader2.record_ack(
                "f1",
                &ReplAck {
                    leader_epoch: TEST_EPOCH,
                    follower_leo: 1,
                    follower_hw: 0,
                    follower_log_epoch: None,
                    groups_applied: None,
                },
                0,
            );
        });

        let outcome = leader.await_acks_blocking(1, leader.min_isr(), Duration::from_secs(2));
        acker.join().unwrap();
        assert_eq!(outcome.hw, 1);
        assert_eq!(outcome.required, 2);
        assert!(outcome.acked_nodes >= 2);
    }

    /// A lone RF=2 leader that starts over a log whose tail no replica ever
    /// acknowledged keeps it hidden until the manager clears it (its follower
    /// out of the way, or having dropped its claim); the grant makes it
    /// readable at once, with no publish and no ack.
    #[test]
    fn a_lone_leader_starting_over_an_unreplicated_tail_exposes_it() {
        let part = temp_partition("lone-start");
        part.set_hw_tracking(HwTracking::Manual);
        part.append_batch(one_record_batch(1)).unwrap();
        part.append_batch(one_record_batch(1)).unwrap();
        let before = part.high_watermark();
        assert!(before < part.log_end_offset(), "the tail starts hidden");
        let leader = make_leader(part, &["leader", "f1"], Acks::Leader, fast_config());
        // Alone from the start, but not yet known to be: the manager clears it
        // once the follower is out of the way.
        assert!(!leader.alone_cleared());
        assert!(
            leader.high_watermark() < leader.log_end_offset(),
            "the tail stays hidden before the grant"
        );
        assert!(leader.grant_alone_if_unshrunk(leader.shrink_seq()));
        assert_eq!(
            leader.high_watermark(),
            leader.log_end_offset(),
            "the tail is readable"
        );
    }

    #[test]
    fn await_acks_times_out_when_not_enough_replicas_ack() {
        let part = temp_partition("await-timeout");
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);

        let outcome = leader.await_acks_blocking(1, leader.min_isr(), Duration::from_millis(50));
        // NOT asserting `outcome.hw == 0` here: `hw` is always the
        // engine's actual `high_watermark()`, and this build's engine
        // still auto-advances that to `log_end_offset` on every local
        // append regardless of quorum (see the NOTE above
        // `hw_advances_for_quorum_acks_after_min_isr_acks`) — the signal
        // this test actually cares about, "not enough replicas acked in
        // time", is `acked_nodes < required`.
        assert_eq!(outcome.required, 2);
        assert_eq!(
            outcome.acked_nodes, 1,
            "only the leader itself has reached offset 1"
        );
        assert!(outcome.acked_nodes < outcome.required);
    }

    /// The wait samples the change generation BEFORE its readiness check,
    /// so an ack that lands in the gap between that check and the sleep
    /// still wakes it. Many rounds of "ack from another thread while the
    /// waiter is entering the wait": with a permit-less wakeup a lost race
    /// shows up as a round that sits out its whole 5 s timeout.
    #[test]
    fn await_acks_never_misses_an_ack_racing_the_wait() {
        let part = temp_partition("await-race");
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        for round in 1..=200u64 {
            leader.partition.append_batch(one_record_batch(1)).unwrap();
            let leader2 = leader.clone();
            let acker = std::thread::spawn(move || {
                leader2.record_ack(
                    "f1",
                    &ReplAck {
                        leader_epoch: TEST_EPOCH,
                        follower_leo: round,
                        follower_hw: 0,
                        follower_log_epoch: None,
                        groups_applied: None,
                    },
                    0,
                );
            });
            let started = std::time::Instant::now();
            let outcome =
                leader.await_acks_blocking(round, leader.min_isr(), Duration::from_secs(5));
            acker.join().unwrap();
            assert!(outcome.acked_nodes >= 2, "round {round}: {outcome:?}");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "round {round}: the ack was missed and the wait ran to its timeout"
            );
        }
    }

    /// The lost-wakeup window itself, forced: the change lands after the
    /// waiter sampled the generation and checked readiness, but before it
    /// went to sleep. A permit-less notify (the old `notify_waiters` shape)
    /// would sleep through it to the deadline; the generation must end the
    /// wait at once.
    #[test]
    fn a_change_between_the_readiness_check_and_the_sleep_is_not_lost() {
        let waiters = AckWaiters::new();
        let seen = waiters.generation();
        // ... readiness checked here and found wanting ...
        waiters.notify_all();
        let started = std::time::Instant::now();
        waiters.wait_for_change(seen, started + Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the wait slept through a change that landed before it began"
        );
    }

    #[test]
    fn closing_ack_waiters_ends_a_pending_wait_without_the_quorum() {
        let part = temp_partition("await-close");
        part.append_batch(one_record_batch(1)).unwrap();
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        let leader2 = leader.clone();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            leader2.close_ack_waiters();
        });
        let started = std::time::Instant::now();
        let outcome = leader.await_acks_blocking(1, leader.min_isr(), Duration::from_secs(10));
        closer.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(outcome.acked_nodes, 1);
        assert!(outcome.acked_nodes < outcome.required);
    }

    // ===== ISR shrink/expand =====

    #[test]
    fn isr_shrinks_on_excess_lag_bytes_and_reports_the_reason() {
        let part = temp_partition("isr-lag");
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        let mut events = leader.subscribe_events();

        leader.set_follower_lag_bytes("f1", 65 * 1024 * 1024);

        let fs = leader.follower_state("f1").unwrap();
        assert!(!fs.in_isr);
        assert_eq!(leader.isr_size(), 2); // leader + f2

        let ev = events.try_recv().expect("shrink event");
        match ev {
            IsrEvent::Shrink {
                node_id,
                reason,
                isr_size,
                min_isr,
            } => {
                assert_eq!(node_id, "f1");
                assert!(matches!(reason, IsrShrinkReason::LagBytes { .. }));
                assert_eq!(isr_size, 2);
                assert_eq!(min_isr, 2);
            }
            other => panic!("expected Shrink, got {other:?}"),
        }
    }

    #[test]
    fn isr_shrinks_on_stale_ack_timeout() {
        let part = temp_partition("isr-timeout");
        let mut config = fast_config();
        config.replica_lag_max_ms = 10;
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        leader.register_follower("f1", 0, 0);

        std::thread::sleep(Duration::from_millis(30));
        leader.reconcile_follower("f1");

        let fs = leader.follower_state("f1").unwrap();
        assert!(!fs.in_isr);
    }

    /// `LeaderConfig::isr_event_hook` must observe every membership
    /// transition — stream attach/detach as well as lag-driven
    /// shrink/expand — with the real reason. This is the mechanism `benches/
    /// bus_replication.rs`'s `gate_p7` installs to make an ISR eviction
    /// visible during a measurement window (TentaBus 1C
    /// `P7-pipeline-flake-open`) — without this test, "the hook logged
    /// nothing" could equally mean "no shrink happened" or "the hook never
    /// fires", and those two readings point at opposite root causes. The
    /// detach case is the one the 2026-09-22 P7 run could not see: a torn
    /// down stream used to leave the ISR with no event at all.
    #[test]
    fn isr_event_hook_observes_every_membership_transition() {
        let seen: Arc<std::sync::Mutex<Vec<IsrEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let mut config = fast_config();
        config.isr_event_hook = Some(Arc::new(move |event: &IsrEvent| {
            sink.lock().unwrap().push(event.clone());
        }));

        let part = temp_partition("isr-hook");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        leader.register_follower("f1", 0, 0);

        leader.set_follower_lag_bytes("f1", 65 * 1024 * 1024);
        leader.record_ack(
            "f1",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 0,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            },
            0, // caught up again
        );
        leader.remove_follower("f1", "codec error: unknown frame kind");
        // Removing an already-removed follower is not a second transition.
        leader.remove_follower("f1", "again");

        let events = seen.lock().unwrap();
        assert_eq!(
            events.len(),
            4,
            "expected attach, shrink, expand, detach: {events:?}"
        );
        assert!(
            matches!(&events[0], IsrEvent::Attached { node_id, isr_size: 2 } if node_id == "f1"),
            "first event must be the attach: {:?}",
            events[0]
        );
        assert!(
            matches!(
                &events[1],
                IsrEvent::Shrink {
                    node_id,
                    reason: IsrShrinkReason::LagBytes { .. },
                    ..
                } if node_id == "f1"
            ),
            "second event must be the lag-bytes shrink: {:?}",
            events[1]
        );
        assert!(
            matches!(&events[2], IsrEvent::Expand { node_id, .. } if node_id == "f1"),
            "third event must be the expand: {:?}",
            events[2]
        );
        assert!(
            matches!(
                &events[3],
                IsrEvent::Detached { node_id, reason, isr_size: 1, min_isr: 2 }
                    if node_id == "f1" && reason.contains("unknown frame kind")
            ),
            "fourth event must be the detach with its cause: {:?}",
            events[3]
        );
    }

    #[test]
    fn isr_expands_once_a_shrunk_follower_catches_up() {
        let part = temp_partition("isr-expand");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.set_follower_lag_bytes("f1", 65 * 1024 * 1024);
        assert!(!leader.follower_state("f1").unwrap().in_isr);

        let mut events = leader.subscribe_events();
        leader.record_ack(
            "f1",
            &ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 0,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            },
            0, // caught up: no bytes in flight anymore
        );

        assert!(leader.follower_state("f1").unwrap().in_isr);
        let ev = events.try_recv().expect("expand event");
        assert!(matches!(ev, IsrEvent::Expand { node_id, .. } if node_id == "f1"));
    }

    #[test]
    fn below_min_isr_is_reported_once_isr_shrinks_past_the_threshold() {
        let part = temp_partition("isr-min");
        let leader = make_leader(part, &["leader", "f1", "f2"], Acks::Quorum, fast_config());
        assert_eq!(leader.min_isr(), 2);
        leader.register_follower("f1", 0, 0);
        leader.register_follower("f2", 0, 0);
        assert!(!leader.below_min_isr());

        leader.set_follower_lag_bytes("f1", 65 * 1024 * 1024);
        assert!(
            !leader.below_min_isr(),
            "isr=2 (leader+f2) still meets min_isr=2"
        );

        let mut events = leader.subscribe_events();
        leader.set_follower_lag_bytes("f2", 65 * 1024 * 1024);
        assert!(
            leader.below_min_isr(),
            "isr=1 (leader only) is below min_isr=2"
        );

        let ev = events.try_recv().expect("shrink event");
        match ev {
            IsrEvent::Shrink {
                isr_size, min_isr, ..
            } => {
                assert!(isr_size < min_isr);
            }
            other => panic!("expected Shrink, got {other:?}"),
        }
    }

    // ===== heartbeat / offsets / truncate =====

    #[tokio::test]
    async fn heartbeat_is_sent_in_silence_and_carries_the_current_epoch() {
        let part = temp_partition("heartbeat");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_millis(500), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for heartbeat")
            .unwrap();
        match frame {
            ReplFrame::Heartbeat(hb) => assert_eq!(hb.leader_epoch, TEST_EPOCH),
            other => panic!("expected Heartbeat, got {other:?}"),
        }

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// A quorum ack commits the batch the stream just sent, and nothing else
    /// is left to feed. The follower must learn the new `hw` at once, not a
    /// heartbeat interval later (measured before this: +501 ms at the
    /// production 500 ms interval). The interval here is a minute, so only
    /// the advance itself can deliver it within the deadline.
    #[tokio::test]
    async fn a_high_watermark_advance_reaches_the_follower_without_waiting_for_a_heartbeat() {
        let part = temp_partition("hw-push");
        part.set_hw_tracking(HwTracking::Manual);
        let config = LeaderConfig {
            heartbeat_interval: Duration::from_secs(60),
            ..fast_config()
        };
        let leader = make_leader(part.clone(), &["leader", "f1"], Acks::Quorum, config);
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        part.append_batch_async(one_record_batch(1)).await.unwrap();
        match tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for the batch")
            .unwrap()
        {
            ReplFrame::Batch { header, .. } => assert_eq!(header.hw, 0),
            other => panic!("expected Batch, got {other:?}"),
        }

        write_frame(
            &mut foll_w,
            &ReplFrame::Ack(ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 1,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut foll_r))
            .await
            .expect("the committed hw never reached the follower before the next heartbeat")
            .unwrap();
        match frame {
            ReplFrame::Heartbeat(hb) => assert_eq!(hb.hw, 1),
            other => panic!("expected Heartbeat carrying hw 1, got {other:?}"),
        }
        assert_eq!(part.high_watermark(), 1);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// ANALIZA-REPLIKACJA §3.3 H1, reproduced over the in-memory duplex: an
    /// `Ack` whose bytes arrive in two parts, with heartbeats (30 ms here)
    /// firing while only the first part is in. The unbiased `select!` drops
    /// the read future each time another arm wins; with a read that keeps
    /// its partial frame inside the future the ack was lost and the stream
    /// desynchronized. The ack must land and the stream must stay up.
    #[tokio::test]
    async fn an_ack_split_across_heartbeats_is_not_lost() {
        let part = temp_partition("split-ack");
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();
        // Keep draining the leader's side (the batch, then heartbeats) so
        // its writes never back up behind this test.
        let drain = tokio::spawn(async move { while read_frame(&mut foll_r).await.is_ok() {} });

        let mut ack = Vec::new();
        write_frame(
            &mut ack,
            &ReplFrame::Ack(ReplAck {
                leader_epoch: TEST_EPOCH,
                follower_leo: 1,
                follower_hw: 0,
                follower_log_epoch: None,
                groups_applied: None,
            }),
        )
        .await
        .unwrap();
        foll_w.write_all(&ack[..3]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        foll_w.write_all(&ack[3..]).await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if handle.is_finished() {
                panic!(
                    "the stream ended instead of reading the split ack: {:?}",
                    handle.await
                );
            }
            if leader.follower_state("f1").map(|f| f.leo) == Some(1) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the split ack never reached the leader's bookkeeping"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // `drain` holds the other half of the split stream, so dropping
        // `foll_w` alone would never close it; stop both tasks outright.
        drain.abort();
        handle.abort();
    }

    #[tokio::test]
    async fn offset_commits_are_coalesced_into_one_frame() {
        let part = temp_partition("offsets");
        // A much longer heartbeat than offsets-coalesce interval, so a
        // silence heartbeat can never race the Offsets frame this test
        // asserts on: PLAN-M2 §1b defaults both to 500 ms, and a fair
        // (non-`biased`) `select!` over two arms with identical periods
        // only makes a Heartbeat send LESS likely to win every cycle, not
        // impossible — this test isolates offset coalescing on its own
        // timeline instead of relying on that probability.
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        leader.note_offset_commit("group-a", 5, 1);
        leader.note_offset_commit("group-a", 6, 2);
        leader.note_offset_discarded(9);

        let frame = tokio::time::timeout(Duration::from_millis(500), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for Offsets")
            .unwrap();
        match frame {
            ReplFrame::Offsets(offsets) => {
                assert_eq!(offsets.leader_epoch, TEST_EPOCH);
                assert_eq!(
                    offsets.commits,
                    vec![
                        ("group-a".to_string(), 0, 5, 1),
                        ("group-a".to_string(), 0, 6, 2),
                    ]
                );
                assert_eq!(offsets.discarded, vec![(0, 9)]);
            }
            other => panic!("expected Offsets, got {other:?}"),
        }

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// A follower stream of a DLQ partition starts by sending every handled
    /// marker the partition already has, so a follower that was away when a
    /// marker was noted (the note reaches only the streams open then) still
    /// receives it.
    #[tokio::test]
    async fn a_follower_stream_starts_with_every_handled_marker() {
        let part = temp_partition("handled-resync");
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        // Written while no stream was open: no note will ever carry it.
        leader.note_offset_discarded(4);
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let lookup: HandledOffsetsLookup = Arc::new(|_earliest| vec![7, 4, 4]);

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(
                leader2,
                "f1".into(),
                leader_r,
                leader_w,
                None,
                Some(lookup),
                rx,
            )
            .await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(Duration::from_millis(500), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for Offsets")
            .unwrap();
        match frame {
            ReplFrame::Offsets(offsets) => {
                assert!(offsets.commits.is_empty());
                assert_eq!(offsets.discarded, vec![(0, 4), (0, 7)]);
            }
            other => panic!("expected Offsets, got {other:?}"),
        }

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    fn ack_hello(accepted_epoch: u32) -> ReplFrame {
        ReplFrame::HelloAck(ReplHelloAck {
            accepted: true,
            follower_leo: 0,
            follower_hw: 0,
            follower_epoch: accepted_epoch,
            environment: NodeEnvironment::Prod,
            reject: None,
            follower_log_epoch: None,
            follower_committed: None,
            bus_features: 0,
        })
    }

    async fn next_offsets<R: AsyncRead + Unpin>(reader: &mut R, what: &str) -> ReplOffsets {
        loop {
            let frame = tokio::time::timeout(Duration::from_millis(800), read_frame(reader))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap();
            if let ReplFrame::Offsets(offsets) = frame {
                return offsets;
            }
        }
    }

    fn group_row(group: &str, paused: bool) -> ReplGroup {
        ReplGroup {
            group: group.to_string(),
            commit_mode: "explicit".to_string(),
            paused,
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    /// A stream sends every group's committed offset of its partition and
    /// every group row of the topic when it opens, so a replica that was away
    /// (or never saw an idle group's last commit) is brought up to date; a
    /// later change of a row sends the rows alone.
    #[tokio::test]
    async fn a_follower_stream_starts_with_every_group_commit_and_row() {
        let part = temp_partition("group-snapshot");
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        leader.set_group_snapshot(Arc::new(|with_commits| GroupSnapshot {
            commits: if with_commits {
                vec![("g1".to_string(), 0, 5, 2)]
            } else {
                Vec::new()
            },
            groups: vec![group_row("g1", true)],
        }));
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(&mut foll_w, &ack_hello(TEST_EPOCH))
            .await
            .unwrap();

        let opened = next_offsets(&mut foll_r, "the snapshot").await;
        assert_eq!(opened.commits, vec![("g1".to_string(), 0, 5, 2)]);
        assert_eq!(opened.groups, vec![group_row("g1", true)]);

        leader.note_groups_changed();
        let changed = next_offsets(&mut foll_r, "the changed rows").await;
        assert!(
            changed.commits.is_empty(),
            "a row change resends no offsets"
        );
        assert_eq!(changed.groups, vec![group_row("g1", true)]);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// A pause or a resume noted just before a handover is on the follower
    /// when the leader stops, not after the next coalescing tick that never
    /// comes: a flush sends it at once and tells the leader it has.
    #[tokio::test]
    async fn a_flush_sends_a_changed_group_row_before_the_coalescing_tick() {
        let part = temp_partition("group-flush");
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        config.offsets_coalesce_interval = Duration::from_secs(3600);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        let paused = Arc::new(AtomicBool::new(false));
        let read = Arc::clone(&paused);
        leader.set_group_snapshot(Arc::new(move |_| GroupSnapshot {
            commits: Vec::new(),
            groups: vec![group_row("g1", read.load(Ordering::SeqCst))],
        }));
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(&mut foll_w, &ack_hello(TEST_EPOCH))
            .await
            .unwrap();
        // The first tick is immediate and sends the opening snapshot.
        let opened = next_offsets(&mut foll_r, "the snapshot").await;
        assert_eq!(opened.groups, vec![group_row("g1", false)]);

        paused.store(true, Ordering::SeqCst);
        leader.note_groups_changed();
        assert!(!leader.groups_flushed_to("f1"), "nothing has gone out yet");
        leader.request_offsets_flush();
        let changed = loop {
            // The note may still be on its way to the stream: ask again.
            leader.request_offsets_flush();
            if let Ok(frame) = tokio::time::timeout(
                Duration::from_millis(50),
                next_offsets(&mut foll_r, "the flushed rows"),
            )
            .await
            {
                break frame;
            }
        };
        assert_eq!(changed.groups, vec![group_row("g1", true)]);
        assert_eq!(changed.groups_gen, 1);
        assert!(
            !leader.groups_flushed_to("f1"),
            "written to the stream is not applied by the follower"
        );
        let ack = ReplAck {
            leader_epoch: TEST_EPOCH,
            follower_leo: 0,
            follower_hw: 0,
            follower_log_epoch: None,
            groups_applied: Some(changed.groups_gen),
        };
        write_frame(&mut foll_w, &ReplFrame::Ack(ack))
            .await
            .unwrap();
        for _ in 0..100 {
            if leader.groups_flushed_to("f1") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            leader.groups_flushed_to("f1"),
            "the follower's ack says applied"
        );

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    /// Commit notes the stream missed are not replayed; it reads the group
    /// state again (and so does a stream that merely opens).
    #[tokio::test]
    async fn a_stream_that_lost_notes_reads_the_group_state_again() {
        let part = temp_partition("group-lagged");
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        leader.set_group_snapshot(Arc::new(move |_with_commits| {
            counted.fetch_add(1, Ordering::SeqCst);
            GroupSnapshot::default()
        }));
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });
        let _hello = read_frame(&mut foll_r).await.unwrap();
        // More notes than the channel holds, before the stream reads any.
        for offset in 0..2_000u64 {
            leader.note_offset_commit("g1", offset, 0);
        }
        write_frame(&mut foll_w, &ack_hello(TEST_EPOCH))
            .await
            .unwrap();

        let drain = tokio::spawn(async move { while read_frame(&mut foll_r).await.is_ok() {} });
        let started = Instant::now();
        while reads.load(Ordering::SeqCst) < 2 && started.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "once when the stream opened, once after it lost notes"
        );

        // Both halves of the follower's end must go for the stream to see EOF.
        drain.abort();
        let _ = drain.await;
        drop(foll_w);
        let _ = handle.await;
    }

    /// A marker set larger than one frame goes out as several `Offsets`
    /// frames of at most `DISCARDS_PER_OFFSETS_FRAME` markers each, so it
    /// never reaches `MAX_FRAME_BYTES` and kills the stream.
    #[tokio::test]
    async fn a_large_marker_set_is_sent_in_bounded_frames() {
        let part = temp_partition("handled-chunks");
        let mut config = fast_config();
        config.heartbeat_interval = Duration::from_secs(10);
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, config);
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();
        let total = DISCARDS_PER_OFFSETS_FRAME as u64 + 10;
        let lookup: HandledOffsetsLookup = Arc::new(move |_earliest| (0..total).collect());

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(
                leader2,
                "f1".into(),
                leader_r,
                leader_w,
                None,
                Some(lookup),
                rx,
            )
            .await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let mut sizes = Vec::new();
        while sizes.iter().sum::<usize>() < total as usize {
            let frame = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut foll_r))
                .await
                .expect("timed out waiting for Offsets")
                .unwrap();
            match frame {
                ReplFrame::Offsets(offsets) => sizes.push(offsets.discarded.len()),
                other => panic!("expected Offsets, got {other:?}"),
            }
        }
        assert_eq!(sizes, vec![DISCARDS_PER_OFFSETS_FRAME, 10]);

        drop(foll_r);
        drop(foll_w);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn truncate_request_is_forwarded_with_the_current_epoch() {
        let part = temp_partition("truncate");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        tx.send(42).unwrap();
        let frame = tokio::time::timeout(Duration::from_millis(500), read_frame(&mut foll_r))
            .await
            .expect("timed out waiting for Truncate")
            .unwrap();
        match frame {
            ReplFrame::Truncate(t) => {
                assert_eq!(t.to_offset, 42);
                assert_eq!(t.leader_epoch, TEST_EPOCH);
            }
            other => panic!("expected Truncate, got {other:?}"),
        }

        drop(foll_r);
        drop(foll_w);
        drop(tx);
        let _ = handle.await;
    }

    // ===== detached partition =====

    #[tokio::test]
    async fn detached_partition_tears_the_stream_down_without_looping() {
        let part = temp_partition("detached");
        part.detach();
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, None, rx).await
        });

        let _hello = read_frame(&mut foll_r).await.unwrap();
        write_frame(
            &mut foll_w,
            &ReplFrame::HelloAck(ReplHelloAck {
                accepted: true,
                follower_leo: 0,
                follower_hw: 0,
                follower_epoch: TEST_EPOCH,
                environment: NodeEnvironment::Prod,
                reject: None,
                follower_log_epoch: None,
                follower_committed: None,
                bus_features: 0,
            }),
        )
        .await
        .unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("stream must end promptly, not loop")
            .unwrap();
        assert!(matches!(result, Err(FollowerStreamError::Detached)));
    }

    /// RF=2 availability: a leader whose only follower is gone keeps its
    /// quorum lease and keeps committing `acks=all` writes on its own ISR.
    /// RF=3 is unchanged (`the_quorum_lease_needs_a_majority_of_recent_acks`,
    /// `acks_all_never_advances_hw_past_a_majority`).
    #[tokio::test]
    async fn an_rf2_leader_alone_keeps_its_lease_and_its_acks_all_writes() {
        let part = temp_partition("rf2-alone");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(part.clone(), &["leader", "f1"], Acks::All, fast_config());
        for _ in 0..3 {
            part.append_batch_async(one_record_batch(1)).await.unwrap();
        }
        leader.register_follower("f1", 0, 0);
        leader.remove_follower("f1", "stream ended");
        assert!(leader.has_quorum_lease());
        // Alone, but not yet known to be alone: the follower may be standing
        // over this leader, so nothing is committed until the manager says
        // it is out of the way (`ReplicationManager::check_alone_rules`).
        assert!(!leader.alone_cleared());
        assert_eq!(leader.high_watermark(), 0);
        assert_eq!(leader.required_for(Acks::All, 1), 2);
        leader.set_alone_cleared(true);
        assert_eq!(leader.high_watermark(), 3);
        assert_eq!(leader.required_for(Acks::All, 1), 1);
    }
    /// A leader starts out alone, which is not yet known to be alone: its
    /// follower may hold a claim on this very term (a transfer's old leader, a
    /// restart's follower). At RF 3 nothing changes.
    #[tokio::test]
    async fn a_new_rf2_leader_commits_alone_only_once_it_is_cleared() {
        let part = temp_partition("rf2-new");
        part.set_hw_tracking(HwTracking::Manual);
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(part.clone(), &["leader", "f1"], Acks::All, fast_config());
        part.append_batch_async(one_record_batch(1)).await.unwrap();
        assert!(!leader.alone_cleared());
        assert_eq!(leader.high_watermark(), 0, "nothing is committed alone yet");
        assert!(leader.grant_alone_if_unshrunk(leader.shrink_seq()));
        assert_eq!(leader.high_watermark(), 1);

        let rf3 = make_leader(
            temp_partition("rf3-new"),
            &["leader", "f1", "f2"],
            Acks::All,
            fast_config(),
        );
        assert!(rf3.alone_cleared());
    }

    /// An acknowledgement is for the shrink it answered: after another shrink
    /// it grants nothing.
    #[tokio::test]
    async fn a_grant_for_an_earlier_shrink_is_refused_after_a_later_one() {
        let part = temp_partition("rf2-seq");
        part.set_leader_epoch(TEST_EPOCH).unwrap();
        let leader = make_leader(part, &["leader", "f1"], Acks::All, fast_config());
        leader.register_follower("f1", 0, 0);
        leader.remove_follower("f1", "stream ended");
        let first = leader.shrink_seq();
        leader.register_follower("f1", 0, 0);
        leader.remove_follower("f1", "stream ended again");
        assert!(leader.shrink_seq() > first);
        assert!(!leader.grant_alone_if_unshrunk(first));
        assert!(!leader.alone_cleared());
        assert!(leader.grant_alone_if_unshrunk(leader.shrink_seq()));
        assert!(leader.alone_cleared());
    }
}
