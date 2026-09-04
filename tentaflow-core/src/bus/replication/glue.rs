// =============================================================================
// File: bus/replication/glue.rs — M2 wave-2 factories (PLAN-M2 §1e, agent G)
// =============================================================================
//
// Wires RL's `PartitionLeader`/`run_follower_stream` (`leader.rs`) and RF's
// `run_follower_stream` (`follower.rs`) into `manager.rs`'s narrow
// `LeaderHandleFactory`/`FollowerRunnerFactory`/`ReplAudit` traits — the
// three traits `ReplicationManager` (agent EL) was built to accept
// concrete implementations of without ever depending on this file's types
// directly.
//
// `PartitionProvider` (below) is the CONTRACT with agent S: `bus/mod.rs`'s
// `BusService` implements it (wave 2, concurrent with this file), and
// `init.rs` receives it as `Arc<dyn PartitionProvider>`. Nothing in this
// file depends on `BusService` by name — only on this trait — so the two
// files compile independently and only need to agree on this shape.
//
// DIAL DIRECTION / RECONNECT OWNERSHIP (reconciling PLAN-M2 §1b item 2's
// "per replica stream task with reconnect/backoff 500 ms -> 5 s" against
// `manager.rs`'s own documented dial model): `ReplicationManager` opens
// each replica's INITIAL stream itself (`Transport::open_stream`, once,
// synchronously within `apply_assignment`/`execute_promotion_actions`)
// and hands the already-open `(BusRecv, BusSend)` pair to
// `LeaderHandleFactory::spawn`. Reconnect on a LATER failure is this
// file's job, not the manager's: `GlueLeaderHandle` owns one supervisor
// task per follower that runs `leader::run_follower_stream` to completion,
// and on any exit other than a terminal one (`Ok(())` — the partition's
// `Partition` is gone — or `Err(Detached)`) re-dials via the SAME
// `Transport` with backoff (500 ms, doubling, capped at 5 s, reset on a
// successful `HelloAck`). This is the only reading under which "per
// replica stream task with reconnect/backoff" and "the manager opens
// streams" (both literally true per their respective sources) are
// consistent: the manager opens the FIRST stream; every stream after that
// is this file's supervisor loop re-dialing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tentaflow_protocol::environment::NodeEnvironment;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use tentaflow_bus::{HwTracking, Partition};

use crate::bus::replication::assignment::PartitionAssignment;
use crate::bus::replication::follower::{
    self, ExpectedLeader, FollowerConfig, FollowerExit, FollowerStores,
};
use crate::bus::replication::frames::{ReplHello, ReplProducerMark, ReplReject};
use crate::bus::replication::leader::{
    self, FollowerStreamError, LeaderConfig, OutboundBatchMeta, PartitionLeader, ProducerMarkLookup,
};
use crate::bus::replication::manager::{
    BusRecv, BusSend, FollowerRunner, FollowerRunnerFactory, LeaderHandle, LeaderHandleFactory,
    ReplAudit, Transport,
};
use crate::bus::replication::metrics::LeaderMetrics;
use crate::bus::topics::Acks;
use crate::bus::{AckOutcome, ReplError, ReplicaLagInfo};
use crate::db::DbPool;

/// Reconnect backoff floor/ceiling (PLAN-M2 §1b item 2's own numbers) for
/// a leader-side follower-stream supervisor task.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Mirrors `dispatch/bus.rs`'s private `BUS_FAILOVER_AUDIT_ACTION` (agent
/// P) byte-for-byte — that constant is not `pub` (it lives in a different
/// crate-internal module this file must not otherwise depend on), so the
/// two are kept in sync by contract, not by a shared symbol. Covered by
/// this file's own `failover_audit_row_matches_the_dispatch_contract` test
/// below, which parses the row this module writes the same way `dispatch/
/// bus.rs`'s `parse_audit_kv`/`failover_events_from_audit` do.
const BUS_FAILOVER_AUDIT_ACTION: &str = "bus.leader.failover";

// ===== Contract with agent S ================================================

/// This node's bridge from `bus::replication` into `BusService`'s engine
/// handles and local (K-M2-5) stores — the ONLY thing standing between
/// this crate-internal module and `bus::mod::BusService`, so the two can
/// be implemented/tested independently. `BusService` implements this
/// trait (agent S, wave 2, concurrent with this file); `init.rs` receives
/// an `Arc<dyn PartitionProvider>` from whoever wires replication up
/// after mesh startup.
pub trait PartitionProvider: Send + Sync {
    /// plan-app-platform §7 W4: the TentaBus instance this provider (and
    /// every partition it opens) belongs to. W5 review finding D1/finding 8
    /// (round 2): `ReplicationInitConfig` now takes its own explicit
    /// `instance_id: BusInstanceId` field rather than deriving one from
    /// this provider — `init` VALIDATES the two agree (bails if they do
    /// not) instead of reading this as the sole source of truth. This
    /// getter remains the one real source of instance id `ReplicationManagerConfig`
    /// itself is built from (`init.rs` copies `cfg.instance_id`, already
    /// checked against this value, into it).
    fn instance_id(&self) -> &str;
    /// Opens (or returns the already-open, shared) engine handle for one
    /// partition. Cheap to call repeatedly — `Partition` is `Arc`-backed
    /// (`tentaflow-bus`'s own doc) and `BusService` is expected to cache
    /// handles itself (PLAN-M2 §1e's `partition_handle_lru`), so this is
    /// never the first place a handle gets opened from cold.
    fn partition(&self, org: &str, topic: &str, partition: u32) -> Result<Partition, ReplError>;
    /// The local group-offset/DLQ-discard/producer-sequence stores a
    /// follower stream applies `Offsets`/`Batch.producer` into
    /// (`follower::FollowerStores`'s own doc).
    fn follower_stores(&self) -> FollowerStores;
    /// Producer-idempotency side channel for one outbound batch
    /// (`leader::OutboundBatchMeta`'s doc, K-M2-6): `None` when the batch
    /// at `base_offset` was not published with a `producer_id` (or the
    /// lookup has already aged the entry out).
    fn producer_mark_for(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        base_offset: u64,
    ) -> Option<ReplProducerMark>;
    /// The topic's configured ack level (`bus::topics::Acks`), driving
    /// `PartitionLeader`'s engine-visible `high_watermark` pacing
    /// (`PartitionLeader::new`'s own doc). `None` when the topic is
    /// unknown to this node at spawn time — the caller falls back to
    /// `Acks::Quorum` (PLAN §7.1's own default for RF > 1).
    fn topic_acks(&self, org: &str, topic: &str) -> Option<Acks>;
}

// ===== Leader-side glue ======================================================

/// `LeaderHandleFactory` impl over `leader::PartitionLeader` +
/// `leader::run_follower_stream`. One instance per node, shared across
/// every partition this node leads (mirrors `metrics::LeaderMetrics`'s own
/// "node-wide gauges" shape — `metrics` here is one `Arc` handed to every
/// `PartitionLeader` this factory spawns).
pub struct GlueLeaderFactory {
    local_node_id: String,
    local_env: NodeEnvironment,
    provider: Arc<dyn PartitionProvider>,
    transport: Arc<dyn Transport>,
    config: LeaderConfig,
    metrics: Arc<LeaderMetrics>,
}

impl GlueLeaderFactory {
    pub fn new(
        local_node_id: impl Into<String>,
        local_env: NodeEnvironment,
        provider: Arc<dyn PartitionProvider>,
        transport: Arc<dyn Transport>,
        config: LeaderConfig,
        metrics: Arc<LeaderMetrics>,
    ) -> Self {
        Self {
            local_node_id: local_node_id.into(),
            local_env,
            provider,
            transport,
            config,
            metrics,
        }
    }
}

impl LeaderHandleFactory for GlueLeaderFactory {
    fn spawn(
        &self,
        assignment: &PartitionAssignment,
        replica_streams: Vec<(String, BusRecv, BusSend)>,
    ) -> Result<Box<dyn LeaderHandle>, ReplError> {
        self.spawn_with_epoch_mode(assignment, replica_streams, EpochStamp::Sync)
    }

    /// Promotion-path entry point (`ReplicationManager::
    /// execute_promotion_actions`): identical to `spawn` except the local
    /// `leader_epoch` stamp is DEFERRED off the caller. The promotion runs
    /// on the manager's async task; `set_leader_epoch` round-trips the
    /// engine's writer thread (a blocking wait that persists meta), and
    /// blocking on it there can stall the accept path / materialization
    /// poll while peers wait out the election. The deferred stamp runs on
    /// a spawned blocking task and is awaited by every follower-stream
    /// supervisor before its first dial, so no wire frame can leave before
    /// the engine agrees with the leader-side epoch. See
    /// `LeaderHandleFactory::spawn_deferred`'s doc for why the wire
    /// protocol itself is indifferent to the timing.
    fn spawn_deferred(
        &self,
        assignment: &PartitionAssignment,
        replica_streams: Vec<(String, BusRecv, BusSend)>,
    ) -> Result<Box<dyn LeaderHandle>, ReplError> {
        self.spawn_with_epoch_mode(assignment, replica_streams, EpochStamp::Deferred)
    }
}

/// Whether `spawn_with_epoch_mode` stamps the partition's `leader_epoch`
/// synchronously before returning (`Sync` — the startup/assignment-poll
/// path, unchanged behavior) or defers it to a spawned task with the
/// supervisors awaiting it (`Deferred` — the promotion path).
#[derive(Clone, Copy)]
enum EpochStamp {
    Sync,
    Deferred,
}

impl GlueLeaderFactory {
    fn spawn_with_epoch_mode(
        &self,
        assignment: &PartitionAssignment,
        replica_streams: Vec<(String, BusRecv, BusSend)>,
        epoch_stamp: EpochStamp,
    ) -> Result<Box<dyn LeaderHandle>, ReplError> {
        let partition =
            self.provider
                .partition(&assignment.org_id, &assignment.topic, assignment.partition)?;
        // Becoming leader: this partition's `high_watermark` is now driven
        // by ack-quorum bookkeeping (`PartitionLeader::recompute_hw`), not
        // the engine's own M1 `FollowLeo` default (PLAN-M2 §1a's
        // `HwTracking` contract). Under `Deferred` both engine stamps move
        // into `ensure_leader_epoch_stamped` below — the caller must not
        // block on the writer thread here (see `spawn_deferred`'s doc).
        let deferred_epoch = match epoch_stamp {
            EpochStamp::Sync => {
                partition.set_hw_tracking(HwTracking::Manual);
                partition
                    .set_leader_epoch(assignment.leader_epoch)
                    .map_err(|e| ReplError::Internal(format!("set_leader_epoch: {e}")))?;
                None
            }
            EpochStamp::Deferred => Some(assignment.leader_epoch),
        };

        let acks = self
            .provider
            .topic_acks(&assignment.org_id, &assignment.topic)
            .unwrap_or(Acks::Quorum);

        let leader = Arc::new(PartitionLeader::new(
            assignment.instance_id.clone(),
            assignment.org_id.clone(),
            assignment.topic.clone(),
            assignment.partition,
            self.local_node_id.clone(),
            assignment.replicas.clone(),
            assignment.leader_epoch,
            acks,
            self.local_env,
            partition.clone(),
            self.config,
            Arc::clone(&self.metrics),
        ));

        let shared = Arc::new(GlueLeaderShared {
            leader,
            partition,
            replica_count: assignment.replicas.len().max(1),
            stopped: AtomicBool::new(false),
            truncate_senders: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
            deferred_epoch: Mutex::new(deferred_epoch),
            stamp_lock: tokio::sync::Mutex::new(()),
            stamped: AtomicBool::new(false),
        });

        // Kick the deferred stamp off immediately so a leader with no
        // live followers (or one whose followers are slow to dial) still
        // gets its engine epoch stamped without waiting for a supervisor
        // to come around; the supervisors' own `ensure_leader_epoch_
        // stamped` below dedupes against this via the `stamped` flag.
        if deferred_epoch.is_some() {
            let stamp_shared = Arc::clone(&shared);
            tokio::spawn(async move {
                stamp_shared.ensure_leader_epoch_stamped().await;
            });
        }

        // Every OTHER replica gets a supervisor — the reconnect loop is
        // the ONLY dialer (see `manager.rs`'s NON-NEGOTIABLE note:
        // `apply_assignment` and the promotion path hand NO streams, so a
        // dead peer's dial can never delay the registry insert). A handed
        // stream covers its supervisor's first dial attempt; a replica
        // with no handed stream gets a stream-less supervisor that dials
        // via the transport itself. Self is never a target — the leader
        // does not dial itself.
        let mut no_stream_replicas: Vec<String> = assignment
            .replicas
            .iter()
            .filter(|r| **r != self.local_node_id)
            .cloned()
            .collect();
        for (node_id, recv, send) in replica_streams {
            no_stream_replicas.retain(|r| *r != node_id);
            spawn_follower_supervisor(
                Arc::clone(&shared),
                node_id,
                Some((recv, send)),
                Arc::clone(&self.provider),
                Arc::clone(&self.transport),
                assignment.org_id.clone(),
                assignment.topic.clone(),
                assignment.partition,
            );
        }
        for node_id in no_stream_replicas {
            spawn_follower_supervisor(
                Arc::clone(&shared),
                node_id,
                None,
                Arc::clone(&self.provider),
                Arc::clone(&self.transport),
                assignment.org_id.clone(),
                assignment.topic.clone(),
                assignment.partition,
            );
        }

        Ok(Box::new(GlueLeaderHandle(shared)))
    }
}

struct GlueLeaderShared {
    leader: Arc<PartitionLeader>,
    partition: Partition,
    replica_count: usize,
    stopped: AtomicBool,
    /// Current sender for each follower's `Truncate` request channel —
    /// swapped on every reconnect (a fresh `mpsc` channel per attempt,
    /// since `run_follower_stream` consumes its receiver by value and a
    /// finished attempt's receiver cannot be reused). A `send_truncate`
    /// call that races a reconnect (lands between the old attempt ending
    /// and the new one registering its sender) is simply dropped — an
    /// acceptable loss for a rare, admin/promotion-triggered, retried-by-
    /// nature request (K-M2-1 truncate targets are re-derived from a
    /// fresh `LeoQuery` on the next election attempt if this one is
    /// missed).
    truncate_senders: Mutex<HashMap<String, mpsc::UnboundedSender<u64>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// `Some(epoch)` while the `Deferred` epoch stamp (see `spawn_deferred`)
    /// has not been applied to the local partition yet; `None` once done or
    /// when the synchronous stamp path was used.
    deferred_epoch: Mutex<Option<u32>>,
    /// Serializes the deferred stamp so exactly one caller (the immediate
    /// task or the first supervisor) performs it while the others wait.
    stamp_lock: tokio::sync::Mutex<()>,
    stamped: AtomicBool,
}

impl GlueLeaderShared {
    /// Applies the deferred `HwTracking::Manual` + `leader_epoch` stamp to
    /// the local partition exactly once. Idempotent and safe to call from
    /// every supervisor: the first caller performs the (blocking) engine
    /// round trip on a blocking-pool thread, concurrent callers wait on
    /// the lock and then observe `stamped`.
    async fn ensure_leader_epoch_stamped(&self) {
        if self.stamped.load(Ordering::SeqCst) {
            return;
        }
        let _guard = self.stamp_lock.lock().await;
        if self.stamped.swap(true, Ordering::SeqCst) {
            return;
        }
        let epoch = match self.deferred_epoch.lock().take() {
            Some(e) => e,
            None => return,
        };
        let partition = self.partition.clone();
        let result = tokio::task::spawn_blocking(move || {
            partition.set_hw_tracking(HwTracking::Manual);
            partition.set_leader_epoch(epoch)
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "replication: deferred set_leader_epoch failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "replication: deferred epoch stamp task failed");
            }
        }
    }
}

fn producer_mark_lookup(
    provider: Arc<dyn PartitionProvider>,
    org: String,
    topic: String,
    partition: u32,
) -> ProducerMarkLookup {
    Arc::new(move |base_offset| OutboundBatchMeta {
        producer: provider.producer_mark_for(&org, &topic, partition, base_offset),
    })
}

/// One follower's reconnect-supervised stream lifecycle (module doc's
/// "DIAL DIRECTION / RECONNECT OWNERSHIP" section): runs `leader::
/// run_follower_stream` to completion, then either stops for good
/// (`Ok(())` — the `Partition`'s `subscribe_leo` sender is gone; or
/// `Err(Detached)`) or re-dials `node_id` via `transport` with backoff and
/// tries again. `initial` is the already-open stream `LeaderHandleFactory::
/// spawn` was handed for the first attempt only; every attempt after that
/// opens its own via `transport.open_stream`.
#[allow(clippy::too_many_arguments)]
fn spawn_follower_supervisor(
    shared: Arc<GlueLeaderShared>,
    node_id: String,
    initial: Option<(BusRecv, BusSend)>,
    provider: Arc<dyn PartitionProvider>,
    transport: Arc<dyn Transport>,
    org_id: String,
    topic: String,
    partition_id: u32,
) {
    let task_shared = Arc::clone(&shared);
    let task = tokio::spawn(async move {
        let shared = task_shared;
        let mut backoff = RECONNECT_BACKOFF_MIN;
        let mut pending = initial;
        loop {
            if shared.stopped.load(Ordering::SeqCst) {
                return;
            }
            // Deferred promotion stamp (see `GlueLeaderFactory::
            // spawn_deferred`): no wire frame may leave this node before
            // its own partition recognizes the new leader epoch. On the
            // synchronous path this is a no-op (already stamped in
            // `spawn_with_epoch_mode`).
            shared.ensure_leader_epoch_stamped().await;
            let (recv, send) = match pending.take() {
                Some(streams) => streams,
                None => match transport.open_stream(&node_id).await {
                    Ok(streams) => streams,
                    Err(e) => {
                        tracing::debug!(
                            node_id = %node_id, error = %e,
                            "replication: leader reconnect dial failed, backing off"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                        continue;
                    }
                },
            };

            let (truncate_tx, truncate_rx) = mpsc::unbounded_channel();
            shared
                .truncate_senders
                .lock()
                .insert(node_id.clone(), truncate_tx);
            let mark_lookup = producer_mark_lookup(
                Arc::clone(&provider),
                org_id.clone(),
                topic.clone(),
                partition_id,
            );

            let result = leader::run_follower_stream(
                Arc::clone(&shared.leader),
                node_id.clone(),
                recv,
                send,
                Some(mark_lookup),
                truncate_rx,
            )
            .await;
            shared.leader.remove_follower(&node_id);
            shared.truncate_senders.lock().remove(&node_id);

            match result {
                Ok(()) => return,
                Err(FollowerStreamError::Detached) => return,
                Err(e) => {
                    if shared.stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    tracing::debug!(
                        node_id = %node_id, error = %e,
                        "replication: leader follower-stream ended, reconnecting"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                }
            }
        }
    });
    shared.tasks.lock().push(task);
}

struct GlueLeaderHandle(Arc<GlueLeaderShared>);

impl LeaderHandle for GlueLeaderHandle {
    fn isr(&self) -> Vec<String> {
        self.0.leader.isr_members()
    }

    /// T1's finding (4): every OTHER replica not currently in the live
    /// ISR, with a human-readable reason (K-M2-2/PLAN-M2 §1f's
    /// `BusReplicaLagWire`) — computed live from `PartitionLeader`'s own
    /// per-follower bookkeeping (`follower_state`) rather than cached off
    /// a transient `IsrEvent`, so a subscriber that misses one broadcast
    /// still sees the correct CURRENT reason on the next `snapshot()`
    /// call. A replica this leader has never seen a `Hello` from (or
    /// whose stream already ended and was `remove_follower`'d) has no
    /// `FollowerState` at all — reported as `"disconnected"` rather than
    /// silently omitted, since it is still a member of the replica set.
    fn lagging(&self) -> Vec<ReplicaLagInfo> {
        let leader = &self.0.leader;
        let config = leader.config();
        leader
            .replicas()
            .iter()
            .filter(|node_id| node_id.as_str() != leader.local_node_id())
            .filter_map(|node_id| match leader.follower_state(node_id) {
                Some(fs) if fs.in_isr => None,
                Some(fs) => {
                    let lag_ms = tokio::time::Instant::now()
                        .saturating_duration_since(fs.last_ack_at)
                        .as_millis() as u64;
                    let reason = if fs.lag_bytes > config.replica_lag_max_bytes {
                        format!(
                            "lag_bytes={} exceeds max_bytes={}",
                            fs.lag_bytes, config.replica_lag_max_bytes
                        )
                    } else {
                        format!(
                            "ack_stale_ms={} exceeds max_ms={}",
                            lag_ms, config.replica_lag_max_ms
                        )
                    };
                    Some(ReplicaLagInfo {
                        node_id: node_id.clone(),
                        lag_bytes: fs.lag_bytes,
                        lag_ms,
                        reason,
                    })
                }
                None => Some(ReplicaLagInfo {
                    node_id: node_id.clone(),
                    lag_bytes: 0,
                    lag_ms: 0,
                    reason: "disconnected".to_string(),
                }),
            })
            .collect()
    }

    fn high_watermark(&self) -> u64 {
        self.0.leader.high_watermark()
    }

    fn log_end_offset(&self) -> u64 {
        self.0.leader.log_end_offset()
    }

    /// Blocks the CALLING (non-async) thread on `PartitionLeader::
    /// await_acks_required`, an async fn, per PLAN-M2 §1e's
    /// `ReplicationCoordinator::await_acks` being a synchronous trait
    /// method (`bus/mod.rs`, wired from `BusService::publish`'s own
    /// synchronous call path).
    ///
    /// Deliberately does NOT use `tokio::task::block_in_place` +
    /// `Handle::block_on` (an earlier version of this method did): that
    /// pairing is documented elsewhere as tokio's sanctioned pattern for
    /// driving an async call from sync code inside a worker, but in
    /// practice, when the calling task is itself one of a small, fully
    /// busy worker pool (this method's own caller loop, plus this
    /// partition's follower-stream supervisor tasks all competing for the
    /// SAME `multi_thread` runtime), it has been observed to leave the
    /// wait's own future starved of a worker to run on — no panic, no
    /// error, just an indefinite stall. Rather than depend on worker-pool
    /// headroom this method has no way to guarantee, the wait always runs
    /// on a genuinely independent OS thread with its own throwaway
    /// runtime, and this thread blocks on a plain `std::sync::mpsc`
    /// channel (no Tokio awareness at all, so it can never trip any
    /// "blocking inside a runtime" panic either). Costs one OS thread per
    /// call — on `publish`'s hot path (PLAN-M2 §1e) that is real overhead,
    /// flagged here as a known follow-up (a small persistent thread pool
    /// instead of spawning fresh each time), but correctness comes first.
    fn await_acks(&self, next_offset: u64, required: u32, timeout: Duration) -> AckOutcome {
        let leader = Arc::clone(&self.0.leader);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("await_acks: build dedicated runtime");
            let outcome = rt.block_on(leader.await_acks_required(next_offset, required, timeout));
            let _ = tx.send(outcome);
        });
        rx.recv().unwrap_or(AckOutcome {
            acked_nodes: 0,
            required,
            hw: 0,
        })
    }

    fn note_offset_commit(&self, group: &str, _partition: u32, offset: u64, attempts: u32) {
        self.0.leader.note_offset_commit(group, offset, attempts);
    }

    fn send_truncate(&self, node: &str, to_offset: u64) {
        if let Some(tx) = self.0.truncate_senders.lock().get(node) {
            let _ = tx.send(to_offset);
        }
    }

    fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        for task in self.0.tasks.lock().drain(..) {
            task.abort();
        }
        // PLAN-M2 §1e item 1: only a single-replica (RF=1) partition
        // reverts to the engine's own M1 `FollowLeo` default on stop — a
        // multi-replica partition stopping here means a NEW leader is
        // about to (or already did) take over feeding, and that new
        // leader's own `spawn` will set `HwTracking::Manual` again; there
        // is no window where reverting to `FollowLeo` here is correct for
        // RF>1 (it would let the engine's own local-append auto-bump race
        // ahead of whatever quorum state the new leader inherits).
        if self.0.replica_count <= 1 {
            self.0.partition.set_hw_tracking(HwTracking::FollowLeo);
        }
        // `init::stop`'s "flush" half: best-effort — a failed flush here
        // just means the NEXT periodic `hw_persist_interval`/roll/shutdown
        // flush (`tentaflow-bus`'s own `meta.rs` doc) picks it up instead;
        // never worth failing shutdown over.
        let _ = self.0.partition.flush_meta();
    }
}

// ===== Follower-side glue ====================================================

/// `FollowerRunnerFactory` impl over `follower::run_follower_stream`. One
/// instance per node.
pub struct GlueFollowerFactory {
    local_node_id: String,
    local_env: NodeEnvironment,
    provider: Arc<dyn PartitionProvider>,
    config: FollowerConfig,
}

impl GlueFollowerFactory {
    pub fn new(
        local_node_id: impl Into<String>,
        local_env: NodeEnvironment,
        provider: Arc<dyn PartitionProvider>,
        config: FollowerConfig,
    ) -> Self {
        Self {
            local_node_id: local_node_id.into(),
            local_env,
            provider,
            config,
        }
    }
}

impl FollowerRunnerFactory for GlueFollowerFactory {
    fn spawn(
        &self,
        assignment: &PartitionAssignment,
        hello: ReplHello,
        leader_recv: BusRecv,
        leader_send: BusSend,
    ) -> Result<Box<dyn FollowerRunner>, ReplError> {
        let partition =
            self.provider
                .partition(&assignment.org_id, &assignment.topic, assignment.partition)?;
        // Becoming (or staying) a follower: `high_watermark` follows the
        // leader's `Batch.hw`/`Heartbeat.hw`, never the engine's own local
        // `FollowLeo` auto-bump (PLAN-M2 §1a `HwTracking` contract).
        partition.set_hw_tracking(HwTracking::Manual);
        let partition = Arc::new(partition);
        let stores = self.provider.follower_stores();
        let expected = ExpectedLeader {
            org_id: assignment.org_id.clone(),
            topic: assignment.topic.clone(),
            partition: assignment.partition,
            local_node_id: self.local_node_id.clone(),
        };

        let disconnect_hint = Arc::new(Notify::new());
        let lease_expired = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let last_reject = Arc::new(Mutex::new(None::<ReplReject>));

        let task_partition = Arc::clone(&partition);
        let task_disconnect_hint = Arc::clone(&disconnect_hint);
        let task_lease_expired = Arc::clone(&lease_expired);
        let task_last_reject = Arc::clone(&last_reject);
        let local_env = self.local_env;
        let config = self.config;
        let node_id = assignment.leader_node_id.clone();

        let task = tokio::spawn(async move {
            // `hello` is the SAME `Hello` `ReplicationManager::accept_stream`
            // already read off `leader_recv` to route the stream here —
            // `run_follower_stream_with_hello`, not `run_follower_stream`,
            // so this never reads a second `Hello` the leader is never
            // going to send (wave-3, agent G2's fix for the double-read
            // bug T1 found end-to-end).
            let exit = follower::run_follower_stream_with_hello(
                hello,
                leader_recv,
                leader_send,
                task_partition,
                stores,
                local_env,
                expected,
                config,
                task_disconnect_hint,
            )
            .await;
            match &exit {
                Ok(FollowerExit::LeaseExpired) => {
                    task_lease_expired.store(true, Ordering::SeqCst);
                }
                Ok(FollowerExit::HelloRejected { reject, .. }) => {
                    // Surfaced via `FollowerRunner::last_hello_reject` for
                    // the manager's accept path and diagnostics (P8: the
                    // rejected Hello's own epoch tells the manager whether
                    // its own claim is now stale).
                    *task_last_reject.lock() = Some(reject.clone());
                    tracing::debug!(
                        leader = %node_id,
                        reject = ?reject,
                        "replication: follower stream rejected the leader's Hello"
                    );
                }
                // A stream that DIES is the same fact as a lease that expires,
                // stated sooner: the leader half of this bidi pair is gone, and
                // this node is still a `Follower` for this assignment, so the
                // authority it was following no longer exists to refresh
                // anything. Without this the flag stayed `false` — it was only
                // ever set on `Ok(LeaseExpired)`, which requires a still-alive
                // stream to wait out `leader_lease_ms` — while a graceful leader
                // stop closes the stream and produces `Err(transport EOF)`
                // instead, so `check_leases` (the ONLY election trigger) found
                // nothing due and a stopped leader was never replaced. Measured
                // as both surviving replicas still at
                // `Follower { leader_node_id: "A", epoch: 1 }` for the whole
                // election budget after A's own `shutdown()`.
                //
                // This is a wake-up, not a verdict: `check_leases` still gates
                // on `role == Follower` + still-in-ISR + `PromotionState::Idle`,
                // and an intentional teardown never reaches here — `stop()`
                // aborts this task before the match runs. `Detached`,
                // `EpochFenced`, `HelloRejected` and `OffsetGap` stay
                // deliberately quiet (topic gone, a newer leader already exists,
                // the refusal is the answer, and the manager re-`Hello`s a gap
                // — none of those is a failover).
                Err(e) => {
                    task_lease_expired.store(true, Ordering::SeqCst);
                    tracing::warn!(leader = %node_id, error = %e, "replication: follower stream error");
                }
                Ok(other) => {
                    tracing::debug!(leader = %node_id, exit = ?other, "replication: follower stream ended");
                }
            }
        });

        Ok(Box::new(GlueFollowerRunner {
            partition,
            disconnect_hint,
            lease_expired,
            last_reject,
            stopped,
            task: Mutex::new(Some(task)),
        }))
    }
}

struct GlueFollowerRunner {
    partition: Arc<Partition>,
    disconnect_hint: Arc<Notify>,
    lease_expired: Arc<AtomicBool>,
    last_reject: Arc<Mutex<Option<ReplReject>>>,
    #[allow(dead_code)]
    // reserved: mirrors `GlueLeaderShared::stopped`'s shape for symmetry/future use.
    stopped: Arc<AtomicBool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl FollowerRunner for GlueFollowerRunner {
    fn leo(&self) -> u64 {
        self.partition.log_end_offset()
    }

    fn hw(&self) -> u64 {
        self.partition.high_watermark()
    }

    fn lease_expired(&self) -> bool {
        self.lease_expired.load(Ordering::SeqCst)
    }

    fn mark_leader_disconnected(&self) {
        self.disconnect_hint.notify_one();
    }

    fn last_hello_reject(&self) -> Option<ReplReject> {
        self.last_reject.lock().clone()
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
        // See `GlueLeaderHandle::stop`'s identical best-effort flush note.
        let _ = self.partition.flush_meta();
    }
}

// ===== Audit ==================================================================

/// Real `ReplAudit`: writes `bus.leader.failover` rows to `repository::
/// log_audit` per the EXACT contract `dispatch/bus.rs`'s
/// `BUS_FAILOVER_AUDIT_ACTION` doc specifies (agent P). `transfer`/
/// `evicted` are deliberately NO-OPS — see each method's own doc for why
/// making them real would double-write, not add coverage.
pub struct AuditLogReplAudit {
    db: DbPool,
    local_node_id: String,
}

impl AuditLogReplAudit {
    pub fn new(db: DbPool, local_node_id: impl Into<String>) -> Self {
        Self {
            db,
            local_node_id: local_node_id.into(),
        }
    }
}

impl ReplAudit for AuditLogReplAudit {
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
    ) {
        let details = format!(
            "org_id={org} partition={partition} from_node={from} from_epoch={from_epoch} \
             to_epoch={to_epoch} duration_ms={duration_ms} reason={reason}",
            from = from_node.unwrap_or("-"),
        );
        let _ = crate::db::repository::log_audit(
            &self.db,
            None,
            None,
            BUS_FAILOVER_AUDIT_ACTION,
            Some(topic),
            Some(&details),
            None,
            Some(to_node),
        );
        let _ = &self.local_node_id; // reserved for a future node-scoped variant of this row
    }

    /// NO-OP. `dispatch/bus.rs`'s `leader_transfer_v1` admin handler
    /// (agent P) already writes exactly one `bus.leader.transfer` row per
    /// call, unconditionally, right after `ReplicationCoordinator::
    /// transfer_leader` returns `Ok` — and `ReplicationManager::
    /// transfer_leader` (`manager.rs`) is that handler's ONLY production
    /// call path (there is no autonomous/coordinator-initiated transfer
    /// trigger anywhere in this build, unlike `failover`, whose sole
    /// trigger — a lease-expiry election — has no RPC-layer writer at
    /// all). Making this real would therefore double every transfer's
    /// audit trail, not add a row for a path that currently has none.
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

    /// NO-OP, same reasoning as `transfer` above: `dispatch/environment.rs`'s
    /// `evict_node_from_replica_sets_on_environment_change` hook (agent L)
    /// already writes exactly one `bus.replica.evicted_env_change` row per
    /// call (its own doc: "One audit entry per call, not per partition"),
    /// and that hook's `coordinator.evict_node_from_replica_sets(..)` call
    /// is `ReplicationManager::evict_node_from_replica_sets`'s ONLY
    /// production caller in this build. `dispatch/environment.rs` has its
    /// own passing tests asserting `audit_count(.., "bus.replica.evicted_
    /// env_change") == 1` per environment switch; making this real would
    /// silently turn that into `2` the moment a real `ReplicationManager`
    /// (rather than a fake coordinator) is wired into that test's fixture.
    fn evicted(&self, _node_id: &str, _reason: &str, _count: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap as StdHashMap;

    use crate::bus::dlq::DiscardStore;
    use crate::bus::groups::GroupOffsetStore;
    use crate::bus::producer::ProducerSeqStore;
    use tentaflow_bus::{Durability, RollPolicy};
    use tokio::io::split;

    fn parse_audit_kv(details: &str) -> StdHashMap<&str, &str> {
        details
            .split_whitespace()
            .filter_map(|tok| tok.split_once('='))
            .collect()
    }

    // ---- fakes ---------------------------------------------------------

    /// One NODE's `PartitionProvider`: its own temp-dir-backed engine
    /// partitions and fjall stores, keyed by (org, topic, partition) —
    /// enough to stand in for `BusService` (agent S, the real implementor)
    /// without depending on it at all (that crate-internal circular
    /// dependency is exactly what `PartitionProvider` exists to avoid, per
    /// this file's module doc). A real cluster is N separate `BusService`
    /// processes, each with its own on-disk partition directories under
    /// its own `TENTAFLOW_HOME` — this fake mirrors that by giving EACH
    /// simulated node (leader, f1, f2 below) its OWN `FakeNodeProvider`
    /// instance, never a shared one: sharing one across nodes would make
    /// "replication" trivially true (leader and follower reading the same
    /// underlying directory) instead of actually exercising the wire path.
    struct FakeNodeProvider {
        _dirs: Vec<tempfile::TempDir>,
        partitions: Mutex<StdHashMap<(String, String, u32), Partition>>,
        stores: FollowerStores,
        acks: Mutex<StdHashMap<(String, String), Acks>>,
    }

    impl FakeNodeProvider {
        fn new() -> Arc<Self> {
            let store_dir = tempfile::tempdir().expect("temp dir");
            let db = fjall::Database::builder(store_dir.path())
                .open()
                .expect("open fjall db");
            let stores = FollowerStores {
                offsets: Arc::new(GroupOffsetStore::open(&db).unwrap()),
                discarded: Arc::new(DiscardStore::open(&db).unwrap()),
                producer_seq: Arc::new(ProducerSeqStore::open(&db).unwrap()),
            };
            Arc::new(Self {
                _dirs: vec![store_dir],
                partitions: Mutex::new(StdHashMap::new()),
                stores,
                acks: Mutex::new(StdHashMap::new()),
            })
        }
    }

    impl PartitionProvider for FakeNodeProvider {
        fn instance_id(&self) -> &str {
            "tentabus-00000001"
        }

        fn partition(
            &self,
            org: &str,
            topic: &str,
            partition: u32,
        ) -> Result<Partition, ReplError> {
            let key = (org.to_string(), topic.to_string(), partition);
            let mut guard = self.partitions.lock();
            if let Some(p) = guard.get(&key) {
                return Ok(p.clone());
            }
            let dir = tempfile::tempdir().expect("temp dir");
            // `Durability::Os` (no explicit fsync): this fake exercises
            // replication WIRING/logic, not the engine's own durability
            // guarantees (`tentaflow-bus`'s own tests own that) — 100+
            // sequential, individually-awaited appends across 3 partitions
            // per `leader_and_two_followers_...` below would otherwise pay
            // for 100+ real fsyncs serially, at the mercy of whatever else
            // is contending for this host's disk at the time.
            let part = Partition::open(dir.path(), RollPolicy::default(), Durability::Os, 8)
                .map_err(|e| ReplError::Internal(e.to_string()))?;
            // Leaked deliberately: the partition directory must outlive
            // this fake's own lifetime for the duration of the test
            // process, and `Partition::open` already holds an exclusive
            // lock on it — the OS reclaims the tmp dir at process exit
            // like every other test in this crate that does this.
            std::mem::forget(dir);
            guard.insert(key, part.clone());
            Ok(part)
        }

        fn follower_stores(&self) -> FollowerStores {
            self.stores.clone()
        }

        fn producer_mark_for(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _base_offset: u64,
        ) -> Option<ReplProducerMark> {
            None
        }

        fn topic_acks(&self, org: &str, topic: &str) -> Option<Acks> {
            self.acks
                .lock()
                .get(&(org.to_string(), topic.to_string()))
                .copied()
        }
    }

    /// A `Transport` that always fails — enough to prove a leader-side
    /// supervisor backs off instead of busy-looping, without a real
    /// second endpoint to dial.
    struct DeadTransport;

    #[async_trait::async_trait]
    impl Transport for DeadTransport {
        async fn open_stream(&self, _node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
            Err(ReplError::Internal("dead transport".into()))
        }
    }

    fn assignment(
        replicas: &[&str],
        leader: &str,
        isr: &[&str],
        epoch: u32,
    ) -> PartitionAssignment {
        PartitionAssignment {
            instance_id: "tentabus-00000001".to_string(),
            org_id: "org-1".to_string(),
            topic: "orders".to_string(),
            partition: 0,
            leader_node_id: leader.to_string(),
            replicas: replicas.iter().map(|s| s.to_string()).collect(),
            isr: isr.iter().map(|s| s.to_string()).collect(),
            leader_epoch: epoch,
            updated_at_ms: 0,
        }
    }

    fn fast_leader_config() -> LeaderConfig {
        LeaderConfig {
            heartbeat_interval: Duration::from_millis(30),
            offsets_coalesce_interval: Duration::from_millis(30),
            replica_lag_max_bytes: 64 * 1024 * 1024,
            replica_lag_max_ms: 5_000,
            batch_fetch_max_bytes: 1024 * 1024,
        }
    }

    fn fast_follower_config() -> FollowerConfig {
        FollowerConfig {
            ack_every_n_batches: 1,
            ack_interval: Duration::from_millis(20),
            leader_lease: Duration::from_millis(3_000),
        }
    }

    type Half = (
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    );

    fn duplex_pair() -> ((BusRecv, BusSend), Half) {
        let (a, b) = tokio::io::duplex(4 * 1024 * 1024);
        let (ar, aw) = split(a);
        let leader_side: (BusRecv, BusSend) = (Box::new(ar), Box::new(aw));
        (leader_side, split(b))
    }

    async fn publish_n(partition: &Partition, n: u64, payload: &'static str) {
        for _ in 0..n {
            let mut b = tentaflow_bus::BatchBuilder::new(partition.log_end_offset(), 1);
            b.push(tentaflow_bus::RecordInput::new(
                bytes::Bytes::from_static(payload.as_bytes()),
                0,
            ))
            .unwrap();
            let bytes = b.build().unwrap();
            partition.append_batch_async(bytes).await.unwrap();
        }
    }

    /// 1 leader + 2 followers, EACH with its own `FakeNodeProvider` (own
    /// partition directory — see that fake's own doc for why sharing one
    /// across nodes would make this test meaningless), wired over
    /// `tokio::io::duplex`: 100 batches published through the leader's own
    /// partition handle must land byte-identically on both followers' own
    /// partitions, and `await_acks` (via `LeaderHandle`, the same trait
    /// bridge `manager.rs` calls) must resolve once quorum is met.
    ///
    /// This is the 100-batch e2e that wave 2 could not get to terminate, and
    /// wave 3 spent two paragraphs explaining why it could not PASS at all
    /// (kept as history, because both diagnoses were load-bearing for the fix):
    ///
    /// - wave 2 blamed host starvation — budgets of 10 s/30 s exceeded with
    ///   near-zero process CPU time, ~20 threads each parked on a correctly
    ///   armed primitive, three other `cargo test` processes on 10 cores
    ///   (`uptime` load 15-231 during that session). That reading was right
    ///   about the scheduler and wrong about the cause.
    /// - the actual blocker (`acks=quorum` feed-path defect, T1 wave 3):
    ///   `FakeNodeProvider::topic_acks` has no entry for `orders`, so
    ///   `spawn`'s `unwrap_or(Acks::Quorum)` picked the one acks level whose
    ///   target state was unreachable — the `high_watermark`-bounded feeder
    ///   sent nothing, so the two followers sat at `leo == 0`, so `hw` stayed
    ///   `nth_largest(isr_leos, 2) == 0`, so `acked_nodes >= 2` at offset 100
    ///   never existed at any load. Shrinking the ISR was no way out either
    ///   (`required_for(Acks::Quorum, _)` is `min_isr` from the ASSIGNMENT
    ///   size). Fixed the way it was diagnosed: `tentaflow-bus` gained the
    ///   `log_end_offset`-bounded `PartitionReader::fetch_raw_to_end_of_log`
    ///   and `leader::feed()` reads through it.
    ///
    /// Un-ignored on that fix, and the starvation note is retired too: the run
    /// that closed it finished in 1.29 s on this same contended host (load
    /// 8-11, swap still recovering), so the budgets were never the problem —
    /// the unreachable target state was. `tests/bus_replication_three_node.rs`'s
    /// module doc carries the same defect at cluster scale.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn leader_and_two_followers_replicate_100_batches_and_await_acks_resolves() {
        const BATCHES: u64 = 100;
        let provider_l = FakeNodeProvider::new();
        let provider_f1 = FakeNodeProvider::new();
        let provider_f2 = FakeNodeProvider::new();
        let a = assignment(&["l", "f1", "f2"], "l", &["l", "f1", "f2"], 1);

        let leader_factory = GlueLeaderFactory::new(
            "l",
            NodeEnvironment::Prod,
            Arc::clone(&provider_l) as Arc<dyn PartitionProvider>,
            Arc::new(DeadTransport) as Arc<dyn Transport>,
            fast_leader_config(),
            Arc::new(LeaderMetrics::new()),
        );
        let follower_factory_f1 = GlueFollowerFactory::new(
            "f1",
            NodeEnvironment::Prod,
            Arc::clone(&provider_f1) as Arc<dyn PartitionProvider>,
            fast_follower_config(),
        );
        let follower_factory_f2 = GlueFollowerFactory::new(
            "f2",
            NodeEnvironment::Prod,
            Arc::clone(&provider_f2) as Arc<dyn PartitionProvider>,
            fast_follower_config(),
        );

        let ((f1_leader_side_recv, f1_leader_side_send), f1_follower_half) = duplex_pair();
        let ((f2_leader_side_recv, f2_leader_side_send), f2_follower_half) = duplex_pair();

        let leader_partition = provider_l.partition("org-1", "orders", 0).unwrap();

        // Every `*Factory::spawn` below stamps `Partition::set_leader_epoch`/
        // `flush_meta`, which round-trip through that partition's OWN
        // writer OS thread via a blocking `std::sync::mpsc` wait
        // (`tentaflow-bus`'s `send_and_wait_via_writer_thread` doc) —
        // routed through `spawn_blocking` here rather than called inline
        // on this test's own top-level task, so that wait always runs on
        // a genuine blocking-pool thread instead of competing with
        // whatever this test's async task happens to be doing at the time.
        let a_for_leader = a.clone();
        let leader_handle = tokio::task::spawn_blocking(move || {
            leader_factory.spawn(
                &a_for_leader,
                vec![
                    ("f1".to_string(), f1_leader_side_recv, f1_leader_side_send),
                    ("f2".to_string(), f2_leader_side_recv, f2_leader_side_send),
                ],
            )
        })
        .await
        .expect("leader spawn task must not panic")
        .expect("leader handle spawn");

        // This test bypasses `ReplicationManager::accept_stream` entirely
        // (module doc), so it must do accept_stream's own job of reading
        // the leader's opening `Hello` off the wire itself before handing
        // the (now Hello-consumed) stream to `FollowerRunnerFactory::
        // spawn` — exactly the contract `accept_stream` follows in
        // production (wave-3, agent G2's fix for the double-Hello-read
        // bug: `spawn` itself must NOT read a second one).
        let (mut f1_follower_recv, f1_follower_send) = f1_follower_half;
        let hello_f1 = match crate::bus::replication::frames::read_frame(&mut f1_follower_recv)
            .await
            .expect("f1: read Hello")
        {
            crate::bus::replication::frames::ReplFrame::Hello(h) => h,
            other => panic!("f1: expected Hello, got {other:?}"),
        };
        let a_for_f1 = a.clone();
        tokio::task::spawn_blocking(move || {
            follower_factory_f1.spawn(
                &a_for_f1,
                hello_f1,
                Box::new(f1_follower_recv),
                Box::new(f1_follower_send),
            )
        })
        .await
        .expect("f1 follower spawn task must not panic")
        .expect("f1 follower runner spawn");

        let (mut f2_follower_recv, f2_follower_send) = f2_follower_half;
        let hello_f2 = match crate::bus::replication::frames::read_frame(&mut f2_follower_recv)
            .await
            .expect("f2: read Hello")
        {
            crate::bus::replication::frames::ReplFrame::Hello(h) => h,
            other => panic!("f2: expected Hello, got {other:?}"),
        };
        let a_for_f2 = a.clone();
        tokio::task::spawn_blocking(move || {
            follower_factory_f2.spawn(
                &a_for_f2,
                hello_f2,
                Box::new(f2_follower_recv),
                Box::new(f2_follower_send),
            )
        })
        .await
        .expect("f2 follower spawn task must not panic")
        .expect("f2 follower runner spawn");

        tokio::time::timeout(
            Duration::from_secs(30),
            publish_n(&leader_partition, BATCHES, "hello-m2"),
        )
        .await
        .expect("publish_n must not hang");

        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let outcome =
                    leader_handle.await_acks(u64::from(BATCHES), 2, Duration::from_millis(500));
                if outcome.acked_nodes >= 2 {
                    return outcome;
                }
            }
        })
        .await
        .expect("await_acks must resolve once quorum is met");
        assert!(outcome.acked_nodes >= 2);
        assert_eq!(outcome.required, 2);

        // Byte-identical: both followers' own (independent) partitions
        // must show the exact same records the leader published, once
        // caught up.
        let f1_part = provider_f1.partition("org-1", "orders", 0).unwrap();
        let f2_part = provider_f2.partition("org-1", "orders", 0).unwrap();
        // The catch-up budget is DERIVED, not a magic constant — the fixed
        // 5 s window flaked under load (measured 25/100 and 48/100 at
        // expiry). Derivation: each batch's end-to-end cost is bounded by
        // the finest timer in the replication loop (the follower's
        // `ack_interval`, 20 ms here — one ack cadence per leo wake ->
        // duplex feed -> append), and the load history of this test shows
        // per-batch bursts reaching ~5x that cadence before the scheduler
        // lets the feeder run. Budget = a 2 s base (stream setup, ack
        // warm-up, worst scheduling burst) + 5 ack-intervals per batch.
        const CATCHUP_BASE: Duration = Duration::from_secs(2);
        const ACK_INTERVALS_PER_BATCH: u32 = 5;
        let catchup_budget = CATCHUP_BASE
            + fast_follower_config().ack_interval
                * (ACK_INTERVALS_PER_BATCH * u32::try_from(BATCHES).expect("batch count fits u32"));
        let deadline = tokio::time::Instant::now() + catchup_budget;
        loop {
            if f1_part.log_end_offset() >= BATCHES && f2_part.log_end_offset() >= BATCHES {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "followers never caught up within {catchup_budget:?}                  (2 s base + 5 ack intervals per batch): f1_leo={} f2_leo={}",
                f1_part.log_end_offset(),
                f2_part.log_end_offset(),
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        fn read_all_payloads(partition: &Partition) -> Vec<Vec<u8>> {
            partition
                .open_reader()
                .fetch_from_offset(0, 16 * 1024 * 1024)
                .unwrap()
                .into_iter()
                .flat_map(|b| {
                    b.records()
                        .map(|r| r.unwrap().payload.to_vec())
                        .collect::<Vec<_>>()
                })
                .collect()
        }
        let leader_records = read_all_payloads(&leader_partition);
        let f1_records = read_all_payloads(&f1_part);
        let f2_records = read_all_payloads(&f2_part);
        assert_eq!(leader_records.len(), BATCHES as usize);
        assert_eq!(
            leader_records, f1_records,
            "f1 must be byte-identical to the leader"
        );
        assert_eq!(
            leader_records, f2_records,
            "f2 must be byte-identical to the leader"
        );

        leader_handle.stop();
    }

    /// `GlueLeaderFactory::spawn` must set `HwTracking::Manual` and the
    /// requested `leader_epoch` on the local partition before returning.
    #[tokio::test]
    async fn spawn_stamps_leader_epoch_and_manual_hw_tracking() {
        let cluster = FakeNodeProvider::new();
        let a = assignment(&["l"], "l", &["l"], 9);
        let factory = GlueLeaderFactory::new(
            "l",
            NodeEnvironment::Prod,
            Arc::clone(&cluster) as Arc<dyn PartitionProvider>,
            Arc::new(DeadTransport) as Arc<dyn Transport>,
            fast_leader_config(),
            Arc::new(LeaderMetrics::new()),
        );
        let handle = factory.spawn(&a, vec![]).expect("spawn");
        let part = cluster.partition("org-1", "orders", 0).unwrap();
        assert_eq!(part.leader_epoch(), 9);
        assert_eq!(part.hw_tracking(), HwTracking::Manual);

        // RF=1: `stop()` must revert to `FollowLeo` (PLAN-M2 §1e item 1).
        handle.stop();
        assert_eq!(part.hw_tracking(), HwTracking::FollowLeo);
    }

    /// `GlueFollowerFactory::spawn` must set `HwTracking::Manual` on the
    /// local partition, and `mark_leader_disconnected` must make
    /// `lease_expired()` observably `true` promptly (the same signal
    /// `manager.rs`'s `check_leases` polls).
    #[tokio::test]
    async fn follower_spawn_sets_manual_hw_and_disconnect_hint_flips_lease_expired() {
        let cluster = FakeNodeProvider::new();
        let mut a = assignment(&["l", "f1"], "l", &["l", "f1"], 1);
        a.partition = 1; // distinct partition id so this test's cluster entry cannot collide with others
        let factory = GlueFollowerFactory::new(
            "f1",
            NodeEnvironment::Prod,
            Arc::clone(&cluster) as Arc<dyn PartitionProvider>,
            fast_follower_config(),
        );
        let ((mut leader_recv, _leader_send), (recv, send)) = duplex_pair();
        // `hello` is passed directly to `spawn` now (mirroring
        // `ReplicationManager::accept_stream`'s own contract: it already
        // read this exact `Hello` off the wire before routing here, so
        // `spawn`/`run_follower_stream_with_hello` must not read a second
        // one) rather than written over the wire for an internal read —
        // this test constructs the SAME value a real leader would send.
        let hello = crate::bus::replication::frames::ReplHello {
            instance_id: a.instance_id.clone(),
            org_id: a.org_id.clone(),
            topic: a.topic.clone(),
            partition: a.partition,
            leader_node_id: a.leader_node_id.clone(),
            leader_epoch: a.leader_epoch,
            replicas: a.replicas.clone(),
            environment: NodeEnvironment::Prod,
        };
        let runner = factory
            .spawn(&a, hello, Box::new(recv), Box::new(send))
            .expect("follower runner spawn");

        // `mark_leader_disconnected` only has any effect once the stream
        // is past its `HelloAck` reply and inside the main select loop
        // where `disconnect_hint` is watched (`follower.rs`'s own doc) —
        // wait for that reply before asserting anything below.
        match crate::bus::replication::frames::read_frame(&mut leader_recv)
            .await
            .expect("read HelloAck")
        {
            crate::bus::replication::frames::ReplFrame::HelloAck(ack) => {
                assert!(ack.accepted, "HelloAck must be accepted: {ack:?}")
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }

        let part = cluster.partition("org-1", "orders", 1).unwrap();
        assert_eq!(part.hw_tracking(), HwTracking::Manual);
        assert!(!runner.lease_expired());

        runner.mark_leader_disconnected();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !runner.lease_expired() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "lease_expired never flipped"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runner.stop();
    }

    /// `AuditLogReplAudit::failover` must produce EXACTLY the `dispatch/
    /// bus.rs` `BUS_FAILOVER_AUDIT_ACTION` contract's details format —
    /// parsed the same way `dispatch/bus.rs`'s own `parse_audit_kv` would.
    #[test]
    fn failover_audit_row_matches_the_dispatch_contract() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        let audit = AuditLogReplAudit::new(db.clone(), "node-b");
        audit.failover(
            "org-1",
            "orders",
            3,
            Some("node-a"),
            "node-b",
            5,
            6,
            42,
            "lease_expired",
        );

        let rows = crate::db::repository::list_audit_logs(
            &db,
            &crate::db::models::AuditLogFilters {
                action: Some(BUS_FAILOVER_AUDIT_ACTION.to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .expect("list_audit_logs");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.resource.as_deref(), Some("orders"));
        assert_eq!(row.node_id.as_deref(), Some("node-b"));
        let details = row.details.clone().unwrap_or_default();
        let kv = parse_audit_kv(&details);
        assert_eq!(kv.get("org_id"), Some(&"org-1"));
        assert_eq!(kv.get("partition"), Some(&"3"));
        assert_eq!(kv.get("from_node"), Some(&"node-a"));
        assert_eq!(kv.get("from_epoch"), Some(&"5"));
        assert_eq!(kv.get("to_epoch"), Some(&"6"));
        assert_eq!(kv.get("duration_ms"), Some(&"42"));
        assert_eq!(kv.get("reason"), Some(&"lease_expired"));
    }

    /// `from_node = None` (no prior leader for this partition) must be the
    /// literal `-` token the contract specifies, not an empty string or a
    /// missing key.
    #[test]
    fn failover_audit_row_uses_dash_for_no_prior_leader() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        let audit = AuditLogReplAudit::new(db.clone(), "node-a");
        audit.failover(
            "org-1",
            "orders",
            0,
            None,
            "node-a",
            0,
            1,
            5,
            "lease_expired",
        );

        let rows = crate::db::repository::list_audit_logs(
            &db,
            &crate::db::models::AuditLogFilters {
                action: Some(BUS_FAILOVER_AUDIT_ACTION.to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .expect("list_audit_logs");
        let details = rows[0].details.clone().unwrap_or_default();
        assert_eq!(parse_audit_kv(&details).get("from_node"), Some(&"-"));
    }

    /// `transfer`/`evicted` must not write anything — see their own doc
    /// comments for why (avoiding a double row against `dispatch/bus.rs`/
    /// `dispatch/environment.rs`'s own writers).
    #[test]
    fn transfer_and_evicted_are_no_ops() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        let audit = AuditLogReplAudit::new(db.clone(), "node-a");
        audit.transfer("org-1", "orders", 0, "node-a", "node-b", 2);
        audit.evicted("node-a", "env_change", 3);

        let rows = crate::db::repository::list_audit_logs(
            &db,
            &crate::db::models::AuditLogFilters::default(),
            0,
            10,
        )
        .expect("list_audit_logs");
        assert!(
            rows.is_empty(),
            "no-op ReplAudit methods must write nothing"
        );
    }
}
