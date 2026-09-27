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

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    write_frame, FrameReader, ReplAck, ReplBatchHeader, ReplCodecError, ReplFrame, ReplHeartbeat,
    ReplHello, ReplOffsets, ReplProducerMark, ReplReject, ReplTruncate,
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
}

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
    /// Wakes every feeder task whenever `recompute_hw` actually moves the
    /// engine-visible `high_watermark` — see `subscribe_hw`'s doc.
    hw_tx: watch::Sender<u64>,
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
        let min_isr = compute_min_isr(replicas.len().max(1));
        // Seeded with the current `hw` so a subscription created later only
        // fires on a FUTURE advance — the same contract
        // `Partition::subscribe_leo` gives its own receivers.
        let hw_tx = watch::channel(partition.high_watermark()).0;
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
            hw_tx,
            config,
            metrics,
        };
        leader.metrics.record_isr_size(leader.isr_size());
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
    /// staleness/lag shrink `reconcile_follower` does from a heartbeat tick) and
    /// NEVER by a local append. So `hw` advanced at moments `leo` had sat still,
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

/// Sends a bare `Heartbeat` and returns the `hw` it carried.
async fn send_heartbeat<W: AsyncWrite + Unpin>(
    writer: &mut W,
    leader: &PartitionLeader,
) -> Result<u64, FollowerStreamError> {
    let hw = leader.high_watermark();
    write_frame(
        writer,
        &ReplFrame::Heartbeat(ReplHeartbeat {
            leader_epoch: leader.epoch(),
            hw,
            leader_leo: leader.log_end_offset(),
            committed: Some(leader.committed_offset()),
            epoch_start: leader.term_start(),
        }),
    )
    .await?;
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
pub async fn run_follower_stream<R, W>(
    leader: Arc<PartitionLeader>,
    follower_node_id: String,
    reader: R,
    mut writer: W,
    producer_mark: Option<ProducerMarkLookup>,
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
    };
    write_frame(&mut writer, &ReplFrame::Hello(hello)).await?;

    // One buffered reader for the stream's whole life: the ack intake below
    // sits in a `select!`, so its read must survive cancellation (see
    // `FrameReader`'s doc).
    let mut frames = FrameReader::new(reader);
    let ack = match frames.next_frame().await? {
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
    let kept = follower_log.kept_end(&authority);
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
    let mut pending_discards: Vec<(u32, u64)> = Vec::new();
    let mut last_frame_sent_at = Instant::now();
    // The highest `hw` any frame on this stream has carried: the follower
    // applies it monotonically, so anything at or below it is old news.
    let mut hw_sent: u64 = 0;
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
                        hw_sent = send_heartbeat(&mut writer, &leader).await?;
                        last_frame_sent_at = Instant::now();
                    }
                }
            }

            frame = frames.next_frame() => {
                match frame? {
                    ReplFrame::Ack(a) => {
                        let lag = inflight.ack(a.follower_leo);
                        leader.record_ack(&follower_node_id, &a, lag);
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
                        pending_discards.push((leader.partition_id(), offset));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // K-M2-5 already tolerates a bounded replication
                        // delay; a burst that outran this channel's
                        // capacity just merges into the next coalesced
                        // frame instead of being replayed note-by-note.
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
            }

            // PLAN-M2 §1b "co 500 ms w ciszy": due exactly `heartbeat_interval`
            // after the last frame of any kind. A fixed ticker that skipped a
            // tick whenever a frame had gone out less than one interval ago
            // stretched real silences to almost two intervals.
            _ = tokio::time::sleep_until(heartbeat_due) => {
                hw_sent = send_heartbeat(&mut writer, &leader).await?;
                last_frame_sent_at = Instant::now();
            }

            _ = offsets_ticker.tick() => {
                if !pending_commits.is_empty() || !pending_discards.is_empty() {
                    write_frame(&mut writer, &ReplFrame::Offsets(ReplOffsets {
                        leader_epoch: leader.epoch(),
                        commits: std::mem::take(&mut pending_commits),
                        discarded: std::mem::take(&mut pending_discards),
                    })).await?;
                    last_frame_sent_at = Instant::now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

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

    #[tokio::test]
    async fn hello_ack_handshake_accepts_and_registers_follower() {
        let part = temp_partition("handshake");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (_tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f2".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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

    #[tokio::test]
    async fn truncate_request_is_forwarded_with_the_current_epoch() {
        let part = temp_partition("truncate");
        let leader = make_leader(part, &["leader", "f1"], Acks::Quorum, fast_config());
        let ((leader_r, leader_w), (mut foll_r, mut foll_w)) = split_duplex();
        let (tx, rx) = mpsc::unbounded_channel();

        let leader2 = leader.clone();
        let handle = tokio::spawn(async move {
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
            run_follower_stream(leader2, "f1".into(), leader_r, leader_w, None, rx).await
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
        assert_eq!(leader.high_watermark(), 3);
    }
}
