// =============================================================================
// File: bus/replication/follower.rs — M2 replication follower (PLAN-M2 §1b)
// =============================================================================
//
// Owns the follower half of exactly ONE leader<->follower stream for one
// (org, topic, partition): reads `frames.rs`'s wire frames off `reader`,
// drives the local `tentaflow_bus::Partition` and the local (K-M2-5)
// group-offset/DLQ-discard/producer-sequence stores, and writes `Ack`/
// `HelloAck`/`LeoReply` back on `writer`. Reconnection, ISR bookkeeping and
// election all live elsewhere (`manager.rs`/`election.rs`, agent EL) — this
// module's only job is one stream's lifecycle, ending it with a
// `FollowerExit` the caller uses to decide whether/how to reconnect.
//
// EXIT MODEL: `Ok(FollowerExit::_)` is a normal, expected end of THIS stream
// instance (protocol-level decision — reject a bad `Hello`, epoch fencing,
// an offset gap that needs a fresh `Hello`, a lease timeout, or the engine
// reporting the partition was detached). `Err(FollowerError)` is everything
// else: a codec/transport failure, an engine error this module has no
// specific handling for, or a local-store failure. The caller (manager.rs)
// is expected to treat every `Ok` variant except `Detached` as "reconnect
// with a fresh `Hello`", and `Detached` as "tear this stream down for good,
// no retry" (PLAN-M2 §1b, the N3-P1-1 class of bug this explicitly avoids).

use std::sync::Arc;
use std::time::Duration;

use tentaflow_bus::{BusError, Partition};
use tentaflow_protocol::environment::NodeEnvironment;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio::time::Instant;

use super::frames::{
    read_frame, write_frame, ReplAck, ReplCodecError, ReplFrame, ReplHello, ReplHelloAck,
    ReplLeoReply, ReplOffsets, ReplReject,
};
use crate::bus::dlq::{self, DiscardStore};
use crate::bus::groups::GroupOffsetStore;
use crate::bus::producer::{ProducerIdentity, ProducerSeqStore};
use crate::bus::BusServiceError;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Ack cadence / lease-watchdog tuning (PLAN-M2 §1b). Defaults match the
/// plan's own numbers; a test wanting a faster/slower cadence than
/// production constructs its own `FollowerConfig` rather than mutating
/// these constants.
#[derive(Debug, Clone, Copy)]
pub struct FollowerConfig {
    /// Send an `Ack` after this many `Batch` frames even if
    /// `ack_interval` has not elapsed yet.
    pub ack_every_n_batches: u32,
    /// Send an `Ack` after this much wall time even if fewer than
    /// `ack_every_n_batches` batches have landed since the last one.
    pub ack_interval: Duration,
    /// No `Heartbeat`/`Batch` within this long since the last one ends the
    /// stream with `FollowerExit::LeaseExpired` so the manager can start an
    /// election.
    pub leader_lease: Duration,
}

impl Default for FollowerConfig {
    fn default() -> Self {
        Self {
            ack_every_n_batches: 8,
            ack_interval: Duration::from_millis(500),
            leader_lease: Duration::from_millis(3000),
        }
    }
}

/// Per-(org, topic, partition) follower-side bookkeeping (PLAN-M2 §1b item
/// 1): the leader identity this stream is currently bound to, the epoch it
/// last confirmed, and the lease deadline the watchdog races against.
/// `run_follower_stream` owns one of these for the lifetime of a single
/// stream instance; it is also a plain public type so `manager.rs` (agent
/// EL, not yet written) can reuse the same bookkeeping shape for its own
/// cross-reconnect status view without duplicating the field set.
#[derive(Debug, Clone)]
pub struct PartitionFollower {
    pub config: FollowerConfig,
    leader_node_id: Option<String>,
    leader_epoch: u32,
    lease_deadline: Instant,
}

impl PartitionFollower {
    pub fn new(config: FollowerConfig) -> Self {
        let lease_deadline = Instant::now() + config.leader_lease;
        Self {
            config,
            leader_node_id: None,
            leader_epoch: 0,
            lease_deadline,
        }
    }

    pub fn leader_node_id(&self) -> Option<&str> {
        self.leader_node_id.as_deref()
    }

    pub fn leader_epoch(&self) -> u32 {
        self.leader_epoch
    }

    pub fn lease_deadline(&self) -> Instant {
        self.lease_deadline
    }

    /// Binds this bookkeeping to the leader/epoch a just-accepted `Hello`
    /// carried, and starts the lease clock.
    fn note_leader(&mut self, leader_node_id: String, leader_epoch: u32) {
        self.leader_node_id = Some(leader_node_id);
        self.leader_epoch = leader_epoch;
        self.refresh_lease();
    }

    /// Pushes the lease deadline `leader_lease` forward from now — called on
    /// every `Heartbeat`/`Batch` (PLAN-M2 §1b: those two frame kinds are the
    /// watchdog's only input; `Truncate`/`Offsets`/`LeoQuery` deliberately do
    /// not reset it, since a leader could in principle send only those while
    /// dead in every other respect).
    fn refresh_lease(&mut self) {
        self.lease_deadline = Instant::now() + self.config.leader_lease;
    }
}

/// What the manager already knows before a stream starts (PLAN-M2 §1b):
/// which (org, topic, partition) this stream replicates, and this node's
/// own id — needed to check "local node ∈ replicas" against the incoming
/// `Hello`.
#[derive(Debug, Clone)]
pub struct ExpectedLeader {
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    pub local_node_id: String,
}

/// The local (K-M2-5) stores a follower applies replicated group-offset/
/// DLQ-discard/producer-sequence state into. All three are already `Arc`-
/// wrapped by `BusService` (`bus/mod.rs`), so this is a plain struct of
/// clones rather than a trait — no test-double indirection is needed since
/// every field's real type already opens against a throwaway `fjall::
/// Database` in a temp dir cheaply (see this module's tests).
#[derive(Clone)]
pub struct FollowerStores {
    pub offsets: Arc<GroupOffsetStore>,
    pub discarded: Arc<DiscardStore>,
    pub producer_seq: Arc<ProducerSeqStore>,
}

/// How one `run_follower_stream` call ended (PLAN-M2 §1b). See the module
/// doc for the `Ok`/`Err` split this participates in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowerExit {
    /// The incoming `Hello` was refused; `HelloAck{accepted: false, ..}`
    /// carrying the same reason was already sent before returning.
    /// `leader_epoch` is the epoch the refused Hello carried — the caller
    /// (glue.rs's `FollowerRunner::last_hello_reject`) surfaces it so the
    /// manager can tell "the leader I follow was fenced by a NEWER leader
    /// while I was not looking" (the Hello carried that newer epoch) apart
    /// from a stale probe, without a second ledger round trip.
    HelloRejected {
        reject: ReplReject,
        leader_epoch: u32,
    },
    /// A `Batch` carried a `leader_epoch` older than this partition's own —
    /// the leader on the other end of this stream has been fenced out by a
    /// newer one and must not keep pushing.
    EpochFenced,
    /// A `Batch`'s `base_offset` did not match this partition's current
    /// `log_end_offset` (a dropped/reordered frame, or a missed `Truncate`).
    /// Nothing is sent back; the manager is expected to re-`Hello` this
    /// stream, which re-syncs both ends' offsets from scratch.
    OffsetGap { expected: u64, got: u64 },
    /// No `Heartbeat`/`Batch` arrived within `leader_lease_ms` of the last
    /// one — the manager should start an election.
    LeaseExpired,
    /// An engine call returned `PartitionDetached` (the topic/org was
    /// deleted mid-stream): teardown, never a retry loop (PLAN-M2 §4.1 A5,
    /// the N3-P1-1 class of bug).
    Detached,
}

/// Transport/engine/store failures `run_follower_stream` has no specific
/// protocol-level handling for — see the module doc for the split against
/// `FollowerExit`.
#[derive(Debug, thiserror::Error)]
pub enum FollowerError {
    #[error("replication codec error: {0}")]
    Codec(#[from] ReplCodecError),
    #[error("engine error: {0}")]
    Engine(#[from] BusError),
    #[error("local store error: {0}")]
    Store(#[from] BusServiceError),
    #[error("expected {expected} frame, got {got}")]
    UnexpectedFrame {
        expected: &'static str,
        got: &'static str,
    },
}

fn frame_kind_name(frame: &ReplFrame) -> &'static str {
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

/// Drives one follower-side replication stream to completion (PLAN-M2 §1b).
/// Reads the opening `Hello`, validates it (org/topic/partition match,
/// environment fencing, epoch monotonicity, replica-set membership),
/// replies `HelloAck`, then loops over `Batch`/`Heartbeat`/`Truncate`/
/// `Offsets`/`LeoQuery` until a protocol-level exit condition is hit
/// (`FollowerExit`) or a transport/engine/store error occurs
/// (`FollowerError`).
///
/// `config` is not part of PLAN-M2 §1b's literal signature (which lists
/// only `leader_lease_ms = 3000`/`ack_every_n_batches = 8`/
/// `ack_interval_ms = 500` as `PartitionFollower`'s fields) but is threaded
/// through explicitly here rather than hardcoded, so tests can shrink the
/// cadence without waiting out production timings and `manager.rs` can pass
/// `FollowerConfig::default()` for the plan's exact numbers.
///
/// `disconnect_hint` (M2 wave 2, agent G): the glue's bridge for
/// `FollowerRunner::mark_leader_disconnected` (`manager.rs`'s trait doc,
/// PLAN-M2 §1b's `IrohMeshEvent::PeerDisconnected` accelerator) — a single
/// `notify_one()` call ends this stream with `FollowerExit::LeaseExpired`
/// immediately instead of waiting out the full `leader_lease` timeout. The
/// real lease deadline (`follower.lease_deadline()`) stays authoritative
/// either way: this is strictly an earlier trigger for the SAME outcome,
/// never a different one, so no caller needs to distinguish "expired via
/// hint" from "expired via timeout". A caller with no transport-level
/// disconnect signal to forward (e.g. every test in this module) passes a
/// fresh `Arc::new(Notify::new())` nobody ever notifies — equivalent to
/// this parameter not existing.
///
/// Reads the opening `Hello` off `reader` itself. `manager.rs`'s
/// `accept_stream` already reads that SAME `Hello` first (to route the
/// stream to the right partition) before a `FollowerRunnerFactory` ever
/// gets to spawn this — calling this function from there would block
/// forever on a second `Hello` the leader never sends a second copy of
/// (wave-3, agent G2, the double-read bug this doc used to not mention).
/// `run_follower_stream_with_hello` below is the entry point for a caller
/// that has already consumed the `Hello` itself; this function is now
/// nothing more than "read one `Hello`, then delegate" — kept as a
/// separate public entry point for tests (every test in this module) and
/// any other direct caller that owns the WHOLE stream from byte zero.
pub async fn run_follower_stream<R, W>(
    mut reader: R,
    writer: W,
    partition: Arc<Partition>,
    stores: FollowerStores,
    local_env: NodeEnvironment,
    expected: ExpectedLeader,
    config: FollowerConfig,
    disconnect_hint: Arc<Notify>,
) -> Result<FollowerExit, FollowerError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = match read_frame(&mut reader).await? {
        ReplFrame::Hello(h) => h,
        other => {
            return Err(FollowerError::UnexpectedFrame {
                expected: "Hello",
                got: frame_kind_name(&other),
            })
        }
    };
    run_follower_stream_with_hello(
        hello,
        reader,
        writer,
        partition,
        stores,
        local_env,
        expected,
        config,
        disconnect_hint,
    )
    .await
}

/// `run_follower_stream`'s twin for a caller (`manager.rs`'s
/// `accept_stream` via `FollowerRunnerFactory::spawn`) that has already
/// read the stream's one-and-only `Hello` frame to decide routing, so this
/// function must NOT read another one off `reader` — the leader
/// (`leader.rs`'s `run_follower_stream`) sends exactly one `Hello` per
/// stream, and a second `read_frame` here would block forever waiting for
/// a frame that will never arrive (wave-3, agent G2's fix for the
/// double-Hello-read bug T1 found end-to-end in
/// `tests/bus_replication_three_node.rs`). Everything from the four
/// rejection checks onward is identical to `run_follower_stream`'s own
/// body — this function IS that body, just parameterized over an
/// already-known `hello` instead of reading it itself.
pub async fn run_follower_stream_with_hello<R, W>(
    hello: ReplHello,
    mut reader: R,
    mut writer: W,
    partition: Arc<Partition>,
    stores: FollowerStores,
    local_env: NodeEnvironment,
    expected: ExpectedLeader,
    config: FollowerConfig,
    disconnect_hint: Arc<Notify>,
) -> Result<FollowerExit, FollowerError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Four independent rejection checks, in the order PLAN-M2 §1b lists
    // them. Each sends its own `HelloAck{accepted: false, reject: Some(_)}`
    // and returns immediately — no partial acceptance.
    if hello.org_id != expected.org_id
        || hello.topic != expected.topic
        || hello.partition != expected.partition
    {
        return reject_hello(
            &mut writer,
            &partition,
            local_env,
            ReplReject::TopicUnknown,
            hello.leader_epoch,
        )
        .await;
    }
    if hello.environment != local_env {
        let reject = ReplReject::EnvironmentMismatch {
            theirs: hello.environment,
            ours: local_env,
        };
        return reject_hello(
            &mut writer,
            &partition,
            local_env,
            reject,
            hello.leader_epoch,
        )
        .await;
    }
    let current_epoch = partition.leader_epoch();
    if hello.leader_epoch < current_epoch {
        let reject = ReplReject::StaleEpoch {
            have: current_epoch,
        };
        return reject_hello(
            &mut writer,
            &partition,
            local_env,
            reject,
            hello.leader_epoch,
        )
        .await;
    }
    if !hello.replicas.iter().any(|r| *r == expected.local_node_id) {
        return reject_hello(
            &mut writer,
            &partition,
            local_env,
            ReplReject::NotAReplica,
            hello.leader_epoch,
        )
        .await;
    }

    partition.set_leader_epoch(hello.leader_epoch)?;
    let ack = ReplHelloAck {
        accepted: true,
        follower_leo: partition.log_end_offset(),
        follower_hw: partition.high_watermark(),
        follower_epoch: partition.leader_epoch(),
        environment: local_env,
        reject: None,
    };
    write_frame(&mut writer, &ReplFrame::HelloAck(ack)).await?;

    let mut follower = PartitionFollower::new(config);
    follower.note_leader(hello.leader_node_id, hello.leader_epoch);

    let mut batches_since_ack: u32 = 0;
    let mut last_ack_at = Instant::now();

    loop {
        tokio::select! {
            biased;
            frame = read_frame(&mut reader) => {
                match frame? {
                    ReplFrame::Batch { header, bytes } => {
                        match partition
                            .append_replicated_async(bytes, header.base_offset, header.leader_epoch)
                            .await
                        {
                            Ok(_) => {}
                            Err(BusError::PartitionDetached) => return Ok(FollowerExit::Detached),
                            Err(BusError::OffsetMismatch { expected, got }) => {
                                return Ok(FollowerExit::OffsetGap { expected, got })
                            }
                            Err(BusError::LeaderEpochStale { .. }) => {
                                return Ok(FollowerExit::EpochFenced)
                            }
                            Err(e) => return Err(FollowerError::Engine(e)),
                        }

                        if let Some(mark) = &header.producer {
                            // M2 wave 2 (agent G, `frames.rs`'s
                            // `ReplProducerMark::base_seq` doc): the
                            // producer's own idempotency sequence counter
                            // now rides separately from `base_offset` (the
                            // PARTITION offset the leader assigned this
                            // batch), closing the wave-1 gap this comment
                            // used to document (a same-epoch producer
                            // resuming after a failover with a real
                            // `base_seq` far below this partition's offset
                            // range no longer misfires as a false
                            // `Duplicate`). `record`'s own `offset` argument
                            // still takes `base_offset` — that call
                            // deliberately keys the dedup/fencing store's
                            // "last accepted at partition offset X" fact by
                            // the PARTITION offset, not the producer's
                            // sequence, since that offset is what a promoted
                            // follower's own reads are indexed by.
                            let identity = ProducerIdentity {
                                producer_id: mark.producer_id.clone(),
                                epoch: mark.epoch,
                                base_seq: mark.base_seq,
                            };
                            stores.producer_seq.record(
                                &expected.org_id,
                                &expected.topic,
                                expected.partition,
                                &identity,
                                mark.base_offset,
                            )?;
                        }

                        partition.set_high_watermark(header.hw);
                        follower.refresh_lease();
                        batches_since_ack += 1;
                        maybe_send_ack(
                            &mut writer,
                            &partition,
                            follower.leader_epoch(),
                            &mut batches_since_ack,
                            &mut last_ack_at,
                            &follower.config,
                        )
                        .await?;
                    }
                    ReplFrame::Heartbeat(hb) => {
                        partition.set_high_watermark(hb.hw);
                        follower.refresh_lease();
                        maybe_send_ack(
                            &mut writer,
                            &partition,
                            follower.leader_epoch(),
                            &mut batches_since_ack,
                            &mut last_ack_at,
                            &follower.config,
                        )
                        .await?;
                    }
                    ReplFrame::Truncate(t) => match partition.truncate_to_offset(t.to_offset) {
                        Ok(_new_leo) => {}
                        Err(BusError::PartitionDetached) => return Ok(FollowerExit::Detached),
                        Err(BusError::TruncateBelowHighWatermark { hw, to }) => {
                            tracing::warn!(
                                target: "bus::replication::follower",
                                hw, to, "refusing Truncate below high watermark"
                            );
                        }
                        Err(e) => return Err(FollowerError::Engine(e)),
                    },
                    ReplFrame::Offsets(offsets) => {
                        apply_offsets(&stores, &expected.org_id, &expected.topic, offsets)?;
                    }
                    ReplFrame::LeoQuery(_query) => {
                        let reply = ReplLeoReply {
                            leo: partition.log_end_offset(),
                            hw: partition.high_watermark(),
                            leader_epoch: partition.leader_epoch(),
                            in_isr: true,
                        };
                        write_frame(&mut writer, &ReplFrame::LeoReply(reply)).await?;
                    }
                    other => {
                        return Err(FollowerError::UnexpectedFrame {
                            expected: "Batch/Heartbeat/Truncate/Offsets/LeoQuery",
                            got: frame_kind_name(&other),
                        })
                    }
                }
            }
            _ = tokio::time::sleep_until(follower.lease_deadline()) => {
                return Ok(FollowerExit::LeaseExpired);
            }
            _ = disconnect_hint.notified() => {
                return Ok(FollowerExit::LeaseExpired);
            }
        }
    }
}

/// Sends the reject `HelloAck` for one of the four `Hello` validation
/// failures and returns the matching `FollowerExit` — factored out since
/// PLAN-M2 §1b's four checks otherwise repeat this pair verbatim.
async fn reject_hello<W: AsyncWrite + Unpin>(
    writer: &mut W,
    partition: &Partition,
    local_env: NodeEnvironment,
    reject: ReplReject,
    hello_leader_epoch: u32,
) -> Result<FollowerExit, FollowerError> {
    let ack = ReplHelloAck {
        accepted: false,
        follower_leo: partition.log_end_offset(),
        follower_hw: partition.high_watermark(),
        follower_epoch: partition.leader_epoch(),
        environment: local_env,
        reject: Some(reject.clone()),
    };
    write_frame(writer, &ReplFrame::HelloAck(ack)).await?;
    Ok(FollowerExit::HelloRejected {
        reject,
        leader_epoch: hello_leader_epoch,
    })
}

/// Sends an `Ack` once `ack_every_n_batches` batches or `ack_interval` of
/// wall time have passed since the last one (PLAN-M2 §1b ack cadence),
/// otherwise a no-op. Called after both `Batch` and `Heartbeat` — a
/// heartbeat-only quiet period still keeps the leader's ISR/lag bookkeeping
/// current.
async fn maybe_send_ack<W: AsyncWrite + Unpin>(
    writer: &mut W,
    partition: &Partition,
    leader_epoch: u32,
    batches_since_ack: &mut u32,
    last_ack_at: &mut Instant,
    config: &FollowerConfig,
) -> Result<(), FollowerError> {
    if *batches_since_ack < config.ack_every_n_batches
        && last_ack_at.elapsed() < config.ack_interval
    {
        return Ok(());
    }
    let ack = ReplAck {
        leader_epoch,
        follower_leo: partition.log_end_offset(),
        follower_hw: partition.high_watermark(),
    };
    write_frame(writer, &ReplFrame::Ack(ack)).await?;
    *batches_since_ack = 0;
    *last_ack_at = Instant::now();
    Ok(())
}

/// Applies one `ReplOffsets` frame (K-M2-5) to the local group-offset and
/// DLQ-discard stores. `org_id`/`topic` come from this stream's `Hello`
/// context — the frame itself carries neither (it is already scoped to one
/// (org, topic, partition) stream), only `group`/`partition`/`offset`/
/// `attempts` per commit and `partition`/`offset` per discard.
///
/// OFFSET MONOTONICITY (K-M2-1/K-M2-5 "never regress"): this calls
/// `GroupOffsetStore::commit`, which already rejects `offset < committed`
/// with `OffsetRegression` — exactly the guarantee this frame needs (the
/// leader's coalesced view can lag what this node already applied from an
/// earlier, more current frame, e.g. after a brief reconnect; it must never
/// move the local commit backwards). That rejection is swallowed here as an
/// expected no-op, not surfaced as a stream error. Deliberately NOT using
/// `force_commit`: that method exists specifically for
/// `BusService::reset_offset`'s admin-only, audited downward reset (see its
/// own doc) and bypassing the monotonicity guard here would defeat the
/// point of this comment.
///
/// ATTEMPTS (M2 wave 2, agent G — closes the wave-1 gap this comment used
/// to document): `attempts` in each commit tuple is the leader's absolute
/// per-(group, offset) failure count, which `GroupOffsetStore::
/// set_delivery_attempts` (an ABSOLUTE set, landed alongside this wiring)
/// applies directly — deliberately NOT `record_delivery_attempt` (an
/// increment-by-one built for the LOCAL delivery loop's own failures,
/// which would double-count a leader-computed total instead of replaying
/// it). `first_failed_at_ms` is passed as `None`: the wire frame
/// (`ReplOffsets`'s `commits` tuple) carries no timestamp, so
/// `set_delivery_attempts` stamps `0` per its own doc rather than this
/// module inventing a `now_ms()` that would misrepresent when the failure
/// actually first happened on the leader.
fn apply_offsets(
    stores: &FollowerStores,
    org_id: &str,
    topic: &str,
    frame: ReplOffsets,
) -> Result<(), FollowerError> {
    let now = now_ms();
    for (group, partition, offset, attempts) in frame.commits {
        match stores
            .offsets
            .commit(org_id, &group, topic, partition, offset, now)
        {
            Ok(()) => {}
            Err(BusServiceError::OffsetRegression { .. }) => {}
            Err(e) => return Err(FollowerError::Store(e)),
        }
        stores
            .offsets
            .set_delivery_attempts(org_id, &group, topic, partition, offset, attempts, None)?;
    }
    if !frame.discarded.is_empty() {
        let dlq_topic = dlq::dlq_topic_name(topic);
        for (partition, offset) in frame.discarded {
            stores
                .discarded
                .mark(org_id, &dlq_topic, partition, offset, now)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::Bytes;
    use fjall::Database;
    use tempfile::TempDir;
    use tentaflow_bus::{BatchBuilder, Durability, HwTracking, RecordInput, RollPolicy};

    use super::super::frames::{
        ReplBatchHeader, ReplHeartbeat, ReplHello, ReplLeoQuery, ReplProducerMark, ReplTruncate,
    };

    const ORG: &str = "org-1";
    const TOPIC: &str = "orders";
    const PART: u32 = 0;
    const LOCAL_NODE: &str = "node-local";
    const LEADER_NODE: &str = "node-leader";

    fn open_partition(dir: &std::path::Path) -> Arc<Partition> {
        Arc::new(
            Partition::open(dir, RollPolicy::default(), Durability::FsyncBatch, 8)
                .expect("open partition"),
        )
    }

    /// A fresh `FollowerStores` backed by its own temp-dir fjall
    /// `Database`, matching the pattern `bus/producer.rs`'s own tests use
    /// (`temp_db`) — the `TempDir` must outlive every use of the returned
    /// stores.
    fn open_stores() -> (TempDir, FollowerStores) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Database::builder(dir.path()).open().expect("open fjall db");
        let stores = FollowerStores {
            offsets: Arc::new(GroupOffsetStore::open(&db).unwrap()),
            discarded: Arc::new(DiscardStore::open(&db).unwrap()),
            producer_seq: Arc::new(ProducerSeqStore::open(&db).unwrap()),
        };
        (dir, stores)
    }

    fn expected() -> ExpectedLeader {
        ExpectedLeader {
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: PART,
            local_node_id: LOCAL_NODE.to_string(),
        }
    }

    fn hello(leader_epoch: u32, environment: NodeEnvironment) -> ReplHello {
        ReplHello {
            // This module's `run_follower_stream`/`run_follower_stream_with_hello`
            // never read `instance_id` themselves — that check lives one
            // layer up, in `ReplicationManager::accept_hello`
            // (plan-app-platform §1.6) — so any well-formed value works
            // here; kept consistent with the rest of the test suite.
            instance_id: "tentabus-00000001".to_string(),
            org_id: ORG.to_string(),
            topic: TOPIC.to_string(),
            partition: PART,
            leader_node_id: LEADER_NODE.to_string(),
            leader_epoch,
            replicas: vec![LEADER_NODE.to_string(), LOCAL_NODE.to_string()],
            environment,
        }
    }

    fn batch_frame(base_offset: u64, hw: u64, epoch: u32, payload: &'static str) -> ReplFrame {
        let mut builder = BatchBuilder::new(base_offset, 1);
        builder
            .push(RecordInput::new(Bytes::from_static(payload.as_bytes()), 0))
            .unwrap();
        let bytes = builder.build().unwrap();
        ReplFrame::Batch {
            header: ReplBatchHeader {
                leader_epoch: epoch,
                base_offset,
                hw,
                batch_len: bytes.len() as u32,
                producer: None,
                dedup_keys: vec![],
            },
            bytes,
        }
    }

    /// Fast cadence so tests observe an `Ack` after every batch/heartbeat
    /// without waiting out the 500 ms production interval.
    fn fast_config() -> FollowerConfig {
        FollowerConfig {
            ack_every_n_batches: 1,
            // Zero, not 500ms: several tests expect an `Ack` right after a
            // single `Heartbeat` with no batches in between, so the count
            // trigger (`ack_every_n_batches`) alone would not fire — the
            // interval trigger must fire unconditionally instead. Real
            // `Instant::elapsed()` is always >= `Duration::ZERO`, so this
            // makes every `maybe_send_ack` call send.
            ack_interval: Duration::ZERO,
            leader_lease: Duration::from_millis(3000),
        }
    }

    #[tokio::test]
    async fn successful_handshake_appends_three_batches_byte_identically_and_hw_follows_header() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition.clone(),
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let ack = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::HelloAck(a) => a,
            other => panic!("expected HelloAck, got {other:?}"),
        };
        assert!(ack.accepted);
        assert_eq!(ack.follower_leo, 0);
        assert_eq!(ack.follower_hw, 0);

        let payloads = ["one", "two", "three"];
        for (i, payload) in payloads.iter().enumerate() {
            let base = i as u64;
            write_frame(&mut leader, &batch_frame(base, base + 1, 1, payload))
                .await
                .unwrap();
            let ack = match read_frame(&mut leader).await.unwrap() {
                ReplFrame::Ack(a) => a,
                other => panic!("expected Ack, got {other:?}"),
            };
            assert_eq!(ack.follower_leo, base + 1);
            assert_eq!(ack.follower_hw, base + 1);
        }

        assert_eq!(partition.log_end_offset(), 3);
        assert_eq!(partition.high_watermark(), 3);

        // Byte-identical: read every record back through the engine's own
        // reader and compare payloads to what was sent.
        let reader = partition.open_reader();
        let view = reader.fetch_from_offset(0, 1024 * 1024).unwrap();
        let got: Vec<Vec<u8>> = view
            .into_iter()
            .flat_map(|batch| {
                batch
                    .records()
                    .map(|r| r.unwrap().payload.to_vec())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            got,
            payloads
                .iter()
                .map(|p| p.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );

        drop(leader);
        let exit = handle.await.unwrap();
        assert!(exit.is_err(), "leader dropped mid-stream: expect a transport error, not a clean FollowerExit: {exit:?}");
    }

    #[tokio::test]
    async fn environment_mismatch_is_rejected_and_stream_closes() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Test)),
        )
        .await
        .unwrap();
        let ack = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::HelloAck(a) => a,
            other => panic!("expected HelloAck, got {other:?}"),
        };
        assert!(!ack.accepted);
        assert_eq!(
            ack.reject,
            Some(ReplReject::EnvironmentMismatch {
                theirs: NodeEnvironment::Test,
                ours: NodeEnvironment::Prod,
            })
        );

        let exit = handle.await.unwrap().unwrap();
        assert_eq!(
            exit,
            FollowerExit::HelloRejected {
                reject: ReplReject::EnvironmentMismatch {
                    theirs: NodeEnvironment::Test,
                    ours: NodeEnvironment::Prod,
                },
                leader_epoch: 1,
            }
        );
    }

    #[tokio::test]
    async fn stale_epoch_hello_is_rejected() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        partition.set_leader_epoch(5).unwrap();
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(3, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let ack = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::HelloAck(a) => a,
            other => panic!("expected HelloAck, got {other:?}"),
        };
        assert!(!ack.accepted);
        assert_eq!(ack.reject, Some(ReplReject::StaleEpoch { have: 5 }));

        let exit = handle.await.unwrap().unwrap();
        assert_eq!(
            exit,
            FollowerExit::HelloRejected {
                reject: ReplReject::StaleEpoch { have: 5 },
                // The refused Hello's own epoch (3), not the partition's (5):
                // the manager uses this to tell "my leader was fenced by a
                // NEWER leader" apart from "a stale probe got refused".
                leader_epoch: 3,
            }
        );
    }

    #[tokio::test]
    async fn node_not_in_replicas_is_rejected() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        let mut h = hello(1, NodeEnvironment::Prod);
        h.replicas = vec![LEADER_NODE.to_string()]; // LOCAL_NODE not present
        write_frame(&mut leader, &ReplFrame::Hello(h))
            .await
            .unwrap();
        let ack = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::HelloAck(a) => a,
            other => panic!("expected HelloAck, got {other:?}"),
        };
        assert!(!ack.accepted);
        assert_eq!(ack.reject, Some(ReplReject::NotAReplica));

        let exit = handle.await.unwrap().unwrap();
        assert_eq!(
            exit,
            FollowerExit::HelloRejected {
                reject: ReplReject::NotAReplica,
                leader_epoch: 1,
            }
        );
    }

    #[tokio::test]
    async fn offset_gap_exits_with_offset_gap_variant() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        // Skips offset 0 and sends base_offset 5 first — a gap.
        write_frame(&mut leader, &batch_frame(5, 6, 1, "boom"))
            .await
            .unwrap();

        let exit = handle.await.unwrap().unwrap();
        assert_eq!(
            exit,
            FollowerExit::OffsetGap {
                expected: 0,
                got: 5
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_keeps_lease_alive_and_silence_expires_it_within_lease_ms() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        // Two heartbeats spaced 2s apart (< the 3s lease) must each refresh
        // the deadline instead of letting it expire.
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(2_000)).await;
            write_frame(
                &mut leader,
                &ReplFrame::Heartbeat(ReplHeartbeat {
                    leader_epoch: 1,
                    hw: 0,
                    leader_leo: 0,
                }),
            )
            .await
            .unwrap();
            let _ = read_frame(&mut leader).await.unwrap(); // Ack
        }

        // Now go silent — the lease (3s from the last heartbeat) must expire.
        let exit = tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("stream did not exit within 10s virtual time")
            .unwrap()
            .unwrap();
        assert_eq!(exit, FollowerExit::LeaseExpired);
    }

    /// M2 wave 2 (agent G): `disconnect_hint` (the glue's bridge for
    /// `FollowerRunner::mark_leader_disconnected`, `manager.rs`'s
    /// `IrohMeshEvent::PeerDisconnected` accelerator) must end the stream
    /// with the SAME `FollowerExit::LeaseExpired` the full 3 s timeout
    /// would eventually produce, without waiting anywhere near that long.
    #[tokio::test]
    async fn disconnect_hint_expires_the_lease_immediately() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);
        let disconnect_hint = std::sync::Arc::new(tokio::sync::Notify::new());

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            disconnect_hint.clone(),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        disconnect_hint.notify_one();

        let exit = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("disconnect_hint must end the stream well within the 3s lease")
            .unwrap()
            .unwrap();
        assert_eq!(exit, FollowerExit::LeaseExpired);
    }

    #[tokio::test]
    async fn truncate_below_hw_is_refused_and_stream_continues() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition.clone(),
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        write_frame(&mut leader, &batch_frame(0, 1, 1, "kept"))
            .await
            .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // Ack, hw=1

        write_frame(
            &mut leader,
            &ReplFrame::Truncate(ReplTruncate {
                leader_epoch: 1,
                to_offset: 0, // below hw=1
            }),
        )
        .await
        .unwrap();

        // The stream must still be alive: a LeoQuery gets answered.
        write_frame(
            &mut leader,
            &ReplFrame::LeoQuery(ReplLeoQuery {
                instance_id: "tentabus-00000001".to_string(),
                org_id: ORG.to_string(),
                topic: TOPIC.to_string(),
                partition: PART,
                known_epoch: 1,
            }),
        )
        .await
        .unwrap();
        let reply = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::LeoReply(r) => r,
            other => panic!("expected LeoReply, got {other:?}"),
        };
        assert_eq!(reply.leo, 1);
        assert_eq!(reply.hw, 1);
        assert!(reply.in_isr);

        assert_eq!(
            partition.high_watermark(),
            1,
            "truncate below hw must not move hw"
        );

        drop(leader);
        let _ = handle.await.unwrap(); // expect a transport error on EOF, not a hang
    }

    /// Every record payload currently in this partition, in offset order —
    /// read through the engine's own reader, the same way the replication
    /// feeder reads.
    fn stored_payloads(partition: &Partition) -> Vec<Vec<u8>> {
        partition
            .open_reader()
            .fetch_from_offset(0, 1024 * 1024)
            .unwrap()
            .into_iter()
            .flat_map(|batch| {
                batch
                    .records()
                    .map(|r| r.unwrap().payload.to_vec())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// K-M2-1 truncate-on-divergence, follower side: a replica whose log
    /// runs AHEAD of the new leader's authority (in practice a former leader
    /// carrying a tail that never reached quorum, rejoining after the
    /// failover) must really lose that tail and then keep replicating from
    /// the offset it was cut back to. `HwTracking::Manual` is not test
    /// scaffolding — `glue.rs` sets exactly that on every partition it hands
    /// a follower stream, and under the `FollowLeo` default `hw == leo`
    /// always, so a truncate there could only ever be the below-hw refusal
    /// the test above covers.
    #[tokio::test]
    async fn divergent_local_log_is_truncated_back_to_leader_authority_and_the_chain_resumes() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        partition.set_hw_tracking(HwTracking::Manual);
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition.clone(),
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(2, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        // Five records replicated, only the first three ever committed:
        // offsets 3..5 are this node's own un-replicated tail.
        for (payload, base) in [("a", 0u64), ("b", 1), ("c", 2), ("d", 3), ("e", 4)] {
            write_frame(
                &mut leader,
                &batch_frame(base, (base + 1).min(3), 2, payload),
            )
            .await
            .unwrap();
            let _ = read_frame(&mut leader).await.unwrap(); // Ack
        }
        assert_eq!(partition.log_end_offset(), 5);
        assert_eq!(partition.high_watermark(), 3);

        write_frame(
            &mut leader,
            &ReplFrame::Truncate(ReplTruncate {
                leader_epoch: 2,
                to_offset: 3, // the NEW leader's own leo, per K-M2-1
            }),
        )
        .await
        .unwrap();

        // `LeoQuery` doubles as the barrier here — the stream loop is
        // sequential, so its reply can only be built after the `Truncate`
        // ahead of it was applied.
        write_frame(
            &mut leader,
            &ReplFrame::LeoQuery(ReplLeoQuery {
                instance_id: "tentabus-00000001".to_string(),
                org_id: ORG.to_string(),
                topic: TOPIC.to_string(),
                partition: PART,
                known_epoch: 2,
            }),
        )
        .await
        .unwrap();
        let reply = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::LeoReply(r) => r,
            other => panic!("expected LeoReply, got {other:?}"),
        };
        assert_eq!(reply.leo, 3, "the divergent tail must be dropped, not kept");
        assert_eq!(
            reply.hw, 3,
            "hw is monotonic (K-M2-1) — committed data was never in the truncated tail"
        );
        assert_eq!(
            stored_payloads(&partition),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );

        // Replication resumes AT the offset the log was cut back to; an
        // un-truncated replica would have rejected this batch as a gap.
        write_frame(&mut leader, &batch_frame(3, 4, 2, "d2"))
            .await
            .unwrap();
        let ack = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::Ack(a) => a,
            other => panic!("expected Ack, got {other:?}"),
        };
        assert_eq!(ack.follower_leo, 4);
        assert_eq!(
            stored_payloads(&partition),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d2".to_vec()]
        );

        drop(leader);
        let _ = handle.await.unwrap(); // transport error on EOF, not a hang
    }

    #[tokio::test]
    async fn offsets_frame_is_applied_and_visible_and_never_regresses() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores.clone(),
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        write_frame(
            &mut leader,
            &ReplFrame::Offsets(ReplOffsets {
                leader_epoch: 1,
                commits: vec![("grp-a".to_string(), PART, 10, 0)],
                discarded: vec![(PART, 3)],
            }),
        )
        .await
        .unwrap();

        // Regressive commit right after — must be silently ignored, not
        // move the offset backwards.
        write_frame(
            &mut leader,
            &ReplFrame::Offsets(ReplOffsets {
                leader_epoch: 1,
                commits: vec![("grp-a".to_string(), PART, 4, 0)],
                discarded: vec![],
            }),
        )
        .await
        .unwrap();

        // Drive the loop forward with something that gets an Ack back, so
        // we know both Offsets frames above have already been processed
        // before we inspect the stores.
        write_frame(
            &mut leader,
            &ReplFrame::Heartbeat(ReplHeartbeat {
                leader_epoch: 1,
                hw: 0,
                leader_leo: 0,
            }),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // Ack

        assert_eq!(
            stores
                .offsets
                .committed_offset(ORG, "grp-a", TOPIC, PART)
                .unwrap(),
            10,
            "committed offset must stay at the forward value, never regress to 4"
        );
        assert!(stores
            .discarded
            .is_discarded(ORG, &dlq::dlq_topic_name(TOPIC), PART, 3)
            .unwrap());

        drop(leader);
        let _ = handle.await.unwrap();
    }

    /// M2 wave 2 (agent G): `ReplOffsets.commits`' `attempts` field must
    /// land in `GroupOffsetStore` via `set_delivery_attempts` (an absolute
    /// set) — closes the wave-1 "not replicated" gap this file's `apply_
    /// offsets` doc used to describe. Verified indirectly through
    /// `record_delivery_attempt` (the store's only public reader for this
    /// counter): calling it once right after applying the frame must
    /// return `attempts + 1`, proving the frame's `attempts` was actually
    /// persisted rather than silently dropped.
    #[tokio::test]
    async fn offsets_frame_attempts_are_applied_via_set_delivery_attempts() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores.clone(),
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        write_frame(
            &mut leader,
            &ReplFrame::Offsets(ReplOffsets {
                leader_epoch: 1,
                commits: vec![("grp-b".to_string(), PART, 20, 5)],
                discarded: vec![],
            }),
        )
        .await
        .unwrap();

        // Drive the loop forward with something that gets an Ack back, so
        // we know the Offsets frame above has already been processed
        // before we inspect the store.
        write_frame(
            &mut leader,
            &ReplFrame::Heartbeat(ReplHeartbeat {
                leader_epoch: 1,
                hw: 0,
                leader_leo: 0,
            }),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // Ack

        let info = stores
            .offsets
            .record_delivery_attempt(ORG, "grp-b", TOPIC, PART, 20, 1_000)
            .unwrap();
        assert_eq!(
            info.attempts, 6,
            "the replicated attempts=5 must already be persisted, so one more \
             local failure brings it to 6, not 1"
        );

        drop(leader);
        let _ = handle.await.unwrap();
    }

    #[tokio::test]
    async fn leo_query_is_answered() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        write_frame(
            &mut leader,
            &ReplFrame::LeoQuery(ReplLeoQuery {
                instance_id: "tentabus-00000001".to_string(),
                org_id: ORG.to_string(),
                topic: TOPIC.to_string(),
                partition: PART,
                known_epoch: 1,
            }),
        )
        .await
        .unwrap();
        let reply = match read_frame(&mut leader).await.unwrap() {
            ReplFrame::LeoReply(r) => r,
            other => panic!("expected LeoReply, got {other:?}"),
        };
        assert_eq!(reply.leo, 0);
        assert_eq!(reply.hw, 0);
        assert_eq!(reply.leader_epoch, 1);
        assert!(reply.in_isr);

        drop(leader);
        let _ = handle.await.unwrap();
    }

    #[tokio::test]
    async fn partition_detached_tears_down_the_stream_immediately() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition.clone(),
            stores,
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        partition.detach();

        write_frame(&mut leader, &batch_frame(0, 1, 1, "after-detach"))
            .await
            .unwrap();

        let exit = handle.await.unwrap().unwrap();
        assert_eq!(exit, FollowerExit::Detached);
    }

    /// The producer mark, when present on a `Batch`, must land in
    /// `ProducerSeqStore` so a promoted follower has a (best-effort) view of
    /// this producer's last accepted state — see `apply_offsets`'s sibling
    /// doc on `Batch` handling for the documented `base_seq`/`base_offset`
    /// caveat this test does not exercise (same epoch, first batch only).
    #[tokio::test]
    async fn producer_mark_on_a_batch_is_recorded() {
        let part_dir = tempfile::tempdir().unwrap();
        let partition = open_partition(part_dir.path());
        let (_store_dir, stores) = open_stores();
        let (mut leader, follower_io) = tokio::io::duplex(64 * 1024);
        let (follower_reader, follower_writer) = tokio::io::split(follower_io);

        let handle = tokio::spawn(run_follower_stream(
            follower_reader,
            follower_writer,
            partition,
            stores.clone(),
            NodeEnvironment::Prod,
            expected(),
            fast_config(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        write_frame(
            &mut leader,
            &ReplFrame::Hello(hello(1, NodeEnvironment::Prod)),
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // HelloAck

        let mut builder = BatchBuilder::new(0, 1);
        builder
            .push(RecordInput::new(Bytes::from_static(b"x"), 0))
            .unwrap();
        let bytes = builder.build().unwrap();
        write_frame(
            &mut leader,
            &ReplFrame::Batch {
                header: ReplBatchHeader {
                    leader_epoch: 1,
                    base_offset: 0,
                    hw: 1,
                    batch_len: bytes.len() as u32,
                    producer: Some(ReplProducerMark {
                        producer_id: "p-1".to_string(),
                        epoch: 2,
                        // Deliberately far from `base_offset` (0) below:
                        // proves the follower keys `ProducerIdentity::
                        // base_seq` off `base_seq`, not `base_offset` (the
                        // wave-1 gap `frames.rs`'s `ReplProducerMark::
                        // base_seq` doc and this file's `Batch` handling
                        // doc both describe).
                        base_offset: 0,
                        base_seq: 42,
                    }),
                    dedup_keys: vec![],
                },
                bytes,
            },
        )
        .await
        .unwrap();
        let _ = read_frame(&mut leader).await.unwrap(); // Ack

        let identity = ProducerIdentity {
            producer_id: "p-1".to_string(),
            epoch: 2,
            base_seq: 42,
        };
        assert_eq!(
            stores
                .producer_seq
                .check(ORG, TOPIC, PART, &identity)
                .unwrap(),
            crate::bus::producer::CheckOutcome::Duplicate { original_offset: 0 },
            "the recorded mark must make a replay of the same (epoch, seq) a Duplicate"
        );

        drop(leader);
        let _ = handle.await.unwrap();
    }
}
