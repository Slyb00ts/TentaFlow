// ===== File: benches/bus_replication.rs — TentaBus M2 replication gates ====
// (SUM/tentabus/PLAN-M2.md §1g / PLAN.md §5.2: P6, P7, P9)
//
// Same methodology as `benches/bus_path.rs` (M1): `harness = false`, own
// warmup/measure loops (`criterion_group!`/`criterion_main!` kept only for
// the `cargo bench --bench bus_replication -- --noplot` CLI, no gate
// registers a Criterion measurement of its own — see `bus_path.rs`'s module
// doc for why). Sample counts are REDUCED from `bus_path.rs`'s (n=300) to
// keep this bench's wall time reasonable on a host already shared with
// other agents' builds/tests — each gate says its own n. (P6 was raised to
// n=1000 for the final-tree pass, and P7 grew a pipelined window next to
// the sequential one — see the two gate fns.)
//
// THREE real `BusService` + `ReplicationManager` instances, wired over an
// in-memory `tokio::io::duplex` transport (this file's own `DuplexTransport`)
// instead of real iroh/QUIC — deterministic, no network stack, no port
// binding races. `bus/replication/manager.rs`'s own module doc calls this
// pattern out explicitly: `Transport` exists "so nothing else in this file
// depends on `iroh` types" and its own unit tests already drive it this way
// (`FakeTransport`).
//
// WHY `DuplexTransport::open_stream` HANDS EACH FOLLOWER STREAM STRAIGHT TO
// A `GlueFollowerFactory` (rather than through a follower-side
// `ReplicationManager`): this bench builds ONE leader-side
// `ReplicationManager` and needs two live follower replication/ack streams
// to exercise the quorum path — it never publishes or consumes THROUGH a
// follower node, so a follower's own `ReplicationManager` (role
// bookkeeping, `install_accept_handler`, the accept routing) is not needed
// here; only its `GlueFollowerFactory` is. `open_stream` therefore mirrors
// the essential half of `manager.rs::accept_stream`: read the leader's one
// `ReplHello` off the follower-facing stream, then hand that already-
// consumed stream plus the `hello` straight to that follower's
// `GlueFollowerFactory::spawn` (exactly how `glue.rs`'s own unit tests
// drive a `GlueFollowerFactory`), whose `run_follower_stream_with_hello`
// writes the `HelloAck` and feeds from there. This relies on agent G2's
// wave-3 fix (now landed): the old double-`Hello`-read bug meant a follower
// joining this way re-read a `Hello` that never arrives and never ACKed, so
// every `acks=quorum` publish timed out (`acked=1, required=2`). The Hello
// is read inside a spawned task, not inline in `open_stream`, because the
// leader writes it only AFTER `open_stream` returns and it drives its own
// half via `leader::run_follower_stream` — an inline read would deadlock
// against a leader still waiting for the stream back. A full routing
// through each follower's `ReplicationManager::accept_stream` (the
// production shape) is covered by the process chaos test over real iroh,
// not this in-process bench.
//
// WHAT USED TO BLOCK P6/P7 HERE (circular wait; fixed in wave 3 by agent G2,
// before the measurements recorded in `SUM/tentabus/M2-WYNIKI.md`): a leader
// under `acks=quorum` fed replicas through `leader::feed()`, which read via
// `fetch_raw_from_offset` — bounded at `high_watermark` — while
// `GlueLeaderFactory::spawn` runs the leader's partition under
// `HwTracking::Manual` and `PartitionLeader::recompute_hw` derives hw from the
// in-sync replicas' offsets (`required = 2` of RF=3). Followers start at leo 0,
// so hw was 0, so the feed sent nothing, so nothing ever ACKed. `acks=leader`
// masked it (required=1 is met by the leader's own leo, so hw == leo), which is
// why the pre-existing green replication tests never caught it. `feed()` now
// reads through `PartitionReader::fetch_raw_to_end_of_log`, bound at
// `log_end_offset`, which is the correct bound for a replica feed.
// Note `wait_for_live_isr` proves the handshake completed BEFORE a publish is
// attempted, so a `BLOCKED` from the publish loop is the replication path
// itself, never bench setup.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};
use parking_lot::Mutex;
use tokio::io::split;

use tentaflow_core::bus::replication::assignment::PartitionAssignment;
use tentaflow_core::bus::replication::follower::FollowerConfig;
use tentaflow_core::bus::replication::frames::{self, ReplFrame};
use tentaflow_core::bus::replication::glue::{
    AuditLogReplAudit, GlueFollowerFactory, GlueLeaderFactory,
};
use tentaflow_core::bus::replication::leader::LeaderConfig;
use tentaflow_core::bus::replication::manager::{
    AssignmentStore, BusRecv, BusSend, FollowerRunnerFactory, LedgerAdmission, ReplicationManager,
    ReplicationManagerConfig, Transport,
};
use tentaflow_core::bus::replication::metrics::LeaderMetrics;
use tentaflow_core::bus::topics::{Acks, DurabilityClass, TopicOptions};
use tentaflow_core::bus::{
    self, BusCallContext, PublishBatch, PublishRecord, ReplError, ReplicationCoordinator,
};
use tentaflow_core::sync::ledger::OperationId;
use tentaflow_protocol::environment::NodeEnvironment;

mod support;
use support::{bench_world, now_ms, pseudo_random_bytes, LatencyReport};

const TOPIC: &str = "bench.replication";
const RECORD_BYTES: usize = 1024;
const BATCH_RECORDS: usize = 1000; // PLAN-M2 §1g: 1 KiB records, ~1 MiB batch.
const DUPLEX_BUF: usize = 4 * 1024 * 1024;
/// Upper bound for the Hello/HelloAck handshake that puts the followers into
/// the leader's LIVE ISR. Generous (the exchange is two in-memory frame
/// round trips) because the cost of guessing low is a spurious panic.
const WAIT_ISR_BUDGET: Duration = Duration::from_secs(10);

// ===== Fakes for the two ledger-facing traits (never exercised: this bench
// never runs an election — assignment is applied directly, once, up front)

struct NeverCalledLedger;
impl LedgerAdmission for NeverCalledLedger {
    fn admitted_by(&self, _op_id: OperationId) -> Vec<String> {
        panic!("bus_replication bench: LedgerAdmission::admitted_by should never be called — no election is exercised");
    }
}

struct NeverProposedAssignments;
impl AssignmentStore for NeverProposedAssignments {
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
    fn propose(&self, _assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
        panic!("bus_replication bench: AssignmentStore::propose should never be called — the initial assignment is applied directly via ReplicationManager::apply_assignment, not proposed through a ledger")
    }
}

/// Same-process fake ledger for P9's "RF=3 through the fake ledger with
/// immediate majority" wording (PLAN-M2 §1g's own phrase): `propose`
/// records the assignment locally and reports it as immediately
/// acknowledged by every OTHER replica — the honest best case for how fast
/// assignment propagation itself can be once the ledger round trip is not
/// the bottleneck (a real multi-process ledger round trip is measured
/// separately by the process chaos test, not this in-process bench).
struct FakeLedgerWithImmediateMajority {
    replicas: Vec<String>,
}
impl LedgerAdmission for FakeLedgerWithImmediateMajority {
    fn admitted_by(&self, _op_id: OperationId) -> Vec<String> {
        self.replicas.clone()
    }
}
struct FakeAssignmentProposer;
impl AssignmentStore for FakeAssignmentProposer {
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
    fn propose(&self, _assignment: PartitionAssignment) -> Result<OperationId, ReplError> {
        Ok(OperationId::from_hash([0u8; 32]))
    }
}

struct NoopAudit;
impl tentaflow_core::bus::replication::manager::ReplAudit for NoopAudit {
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
        _from_node: &str,
        _to_node: &str,
        _epoch: u32,
    ) {
    }
    fn evicted(&self, _node_id: &str, _reason: &str, _count: u32) {}
}

/// In-memory `Transport`: `open_stream(peer)` opens a `tokio::io::duplex`,
/// returning one half to the caller (always the leader in this bench — no
/// election, no follower ever calls `open_stream`) and driving the other
/// half as `peer`'s follower: read the leader's `Hello`, then hand the
/// already-consumed stream to that peer's `GlueFollowerFactory::spawn` (see
/// module doc). Every spawned `FollowerRunner` handle is kept alive in
/// `_keepalive` (shared via `Arc`, since the read+spawn runs in a task) for
/// the `DuplexTransport`'s own lifetime — nothing else holds a reference to
/// it, and `FollowerRunner`'s contract does not promise the underlying task
/// survives a dropped handle.
struct DuplexTransport {
    assignment: PartitionAssignment,
    followers: HashMap<String, Arc<GlueFollowerFactory>>,
    _keepalive: Arc<Mutex<Vec<Box<dyn tentaflow_core::bus::replication::manager::FollowerRunner>>>>,
}

#[async_trait::async_trait]
impl Transport for DuplexTransport {
    async fn open_stream(&self, node_id: &str) -> Result<(BusRecv, BusSend), ReplError> {
        let factory = self
            .followers
            .get(node_id)
            .ok_or_else(|| ReplError::Internal(format!("DuplexTransport: unknown peer {node_id}")))?
            .clone();
        let assignment = self.assignment.clone();
        let keepalive = self._keepalive.clone();
        let (ours, theirs) = tokio::io::duplex(DUPLEX_BUF);
        let (our_recv, our_send) = split(ours);
        let (their_recv, their_send) = split(theirs);
        // Consume the leader's one `Hello` off the follower-facing half, then
        // hand the stream (and that `hello`) to the follower factory — the
        // essential routing of `manager.rs::accept_stream`, minus the
        // registry/env bookkeeping this bench does not need. Runs in a task
        // because the leader only writes the `Hello` after `open_stream`
        // returns (see module doc); reading it inline would deadlock.
        let peer_tag = node_id.to_string();
        tokio::spawn(async move {
            let mut their_recv = their_recv;
            let hello = match frames::read_frame(&mut their_recv).await {
                Ok(ReplFrame::Hello(h)) => h,
                other => {
                    eprintln!("bus_replication bench: follower {peer_tag} got no Hello: {other:?}");
                    return;
                }
            };
            match factory.spawn(
                &assignment,
                hello,
                Box::new(their_recv),
                Box::new(their_send),
            ) {
                Ok(runner) => keepalive.lock().push(runner),
                Err(e) => {
                    eprintln!("bus_replication bench: follower {peer_tag} spawn failed: {e}");
                }
            }
        });
        Ok((Box::new(our_recv), Box::new(our_send)))
    }
}

fn topic_options() -> TopicOptions {
    TopicOptions {
        partitions: Some(1),
        replication_factor: Some(3),
        acks: Some(Acks::Quorum),
        durability_class: Some(DurabilityClass::Standard),
        ..Default::default()
    }
}

fn publish_batch(seed: u64) -> PublishBatch {
    PublishBatch {
        partition: Some(0),
        producer: None,
        records: (0..BATCH_RECORDS)
            .map(|i| PublishRecord {
                key: None,
                headers: Vec::new(),
                payload: Bytes::from(pseudo_random_bytes(
                    RECORD_BYTES,
                    seed.wrapping_add(i as u64),
                )),
                timestamp_ms: now_ms(),
                schema_id: 0,
            })
            .collect(),
    }
}

/// Polls the leader's live replication state until every entry of `expected`
/// is in the partition's ISR; `Err(detail)` after `WAIT_ISR_BUDGET`.
///
/// Required because `ReplicationManager::apply_assignment` only *starts* the
/// replication: the leader dials each replica, writes its `Hello`, and
/// `PartitionLeader::register_follower` (which is what sets `in_isr = true`)
/// runs later, on the supervisor task, once that replica's `HelloAck` comes
/// back. Without this wait the first `publish` lands while the ISR still
/// holds only the leader and `preflight` refuses it with
/// `NotEnoughReplicas { isr: 1 }` — correct K-M2-2 behaviour (quorum writes
/// must not silently degrade to `acks=leader`), but it measures nothing.
/// `isr` here is `LeaderHandle::isr()` (live), not the static
/// `PartitionAssignment.isr` the bench handed in.
async fn wait_for_live_isr(
    manager: &ReplicationManager,
    org: &str,
    topic: &str,
    expected: &[&str],
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_ISR_BUDGET;
    loop {
        let snap = manager.snapshot(org, Some(topic));
        let partition = snap.partitions.first();
        let isr: Vec<String> = partition.map(|p| p.isr.clone()).unwrap_or_default();
        if expected.iter().all(|e| isr.iter().any(|m| m == e)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let lagging = partition.map(|p| p.lagging.clone()).unwrap_or_default();
            let reason = partition.and_then(|p| p.unavailable_reason.as_ref());
            return Err(format!(
                "live ISR never reached {expected:?} within {WAIT_ISR_BUDGET:?} \
                 (isr={isr:?}, lagging={lagging:?}, unavailable={reason:?})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Reports "this gate produced NO measurement" and returns, instead of
/// panicking out of the whole binary. A panic in the first gate would hide
/// every later gate's verdict — for a results report an explicit BLOCKED line
/// is strictly more honest than a silent absence, and the detail distinguishes
/// "gate measured something and failed" from "gate never got to run".
fn gate_blocked(gate: &str, stage: &str, detail: &str) {
    eprintln!("{gate}: NO MEASUREMENT — {stage}: {detail}");
    eprintln!("{gate} verdict: BLOCKED (root-cause note: SUM/tentabus/M2-WYNIKI.md)");
}

/// Builds three isolated `BusService`s (leader "node-a", followers "node-b"/
/// "node-c" — `support::bench_world`'s own tempdir/db per instance, M1's
/// established bench pattern), a real leader-side `ReplicationManager`
/// wired over `DuplexTransport`, and applies the RF=3/quorum assignment
/// directly (no election, no ledger round trip — see module doc). Returns
/// the leader's `BenchWorld` (still holding its `_tmp`/`db` guards) plus its
/// `BusCallContext`, with `set_replication` already installed.
struct ReplicatedTrio {
    leader: support::BenchWorld,
    _follower_b: support::BenchWorld,
    _follower_c: support::BenchWorld,
    _manager: Arc<ReplicationManager>,
    _transport: Arc<DuplexTransport>,
}

/// `Err(detail)` when the leader never sees every replica enter its live ISR
/// — the gates can measure nothing then, and `gate_blocked` says why.
async fn build_replicated_trio(
    label: &str,
    follower_config: FollowerConfig,
) -> Result<ReplicatedTrio, String> {
    let leader = bench_world(&format!("{label}-a"));
    let follower_b = bench_world(&format!("{label}-b"));
    let follower_c = bench_world(&format!("{label}-c"));

    for world in [&leader, &follower_b, &follower_c] {
        world
            .svc
            .create_topic(&world.ctx, TOPIC, topic_options())
            .expect("create_topic");
    }

    let assignment = PartitionAssignment {
        instance_id: leader.svc.instance_id().to_string(),
        org_id: leader.ctx.org_id.clone(),
        topic: TOPIC.to_string(),
        partition: 0,
        leader_node_id: "node-a".to_string(),
        replicas: vec![
            "node-a".to_string(),
            "node-b".to_string(),
            "node-c".to_string(),
        ],
        isr: vec![
            "node-a".to_string(),
            "node-b".to_string(),
            "node-c".to_string(),
        ],
        leader_epoch: 1,
        updated_at_ms: now_ms(),
    };

    let mut followers: HashMap<String, Arc<GlueFollowerFactory>> = HashMap::new();
    followers.insert(
        "node-b".to_string(),
        Arc::new(GlueFollowerFactory::new(
            "node-b",
            NodeEnvironment::Prod,
            follower_b.svc.clone()
                as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
            follower_config.clone(),
        )),
    );
    followers.insert(
        "node-c".to_string(),
        Arc::new(GlueFollowerFactory::new(
            "node-c",
            NodeEnvironment::Prod,
            follower_c.svc.clone()
                as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
            follower_config,
        )),
    );

    let transport = Arc::new(DuplexTransport {
        assignment: assignment.clone(),
        followers,
        _keepalive: Arc::new(Mutex::new(Vec::new())),
    });

    let metrics = Arc::new(LeaderMetrics::new());
    let leader_factory = Arc::new(GlueLeaderFactory::new(
        "node-a",
        NodeEnvironment::Prod,
        leader.svc.clone() as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
        transport.clone() as Arc<dyn Transport>,
        LeaderConfig::default(),
        metrics,
    ));
    // Follower factory/audit are required `ReplicationManagerConfig` fields
    // even on the leader (a real node can hold BOTH roles across different
    // partitions) — never exercised here (this bench's leader never becomes
    // a follower of anything), hence the panic-on-call fake.
    let unused_follower_factory: Arc<
        dyn tentaflow_core::bus::replication::manager::FollowerRunnerFactory,
    > = Arc::new(GlueFollowerFactory::new(
        "node-a",
        NodeEnvironment::Prod,
        leader.svc.clone() as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
        FollowerConfig::default(),
    ));
    let audit = Arc::new(AuditLogReplAudit::new(leader.db.clone(), "node-a"));

    let manager = ReplicationManager::new(ReplicationManagerConfig {
        instance_id: leader.svc.instance_id().to_string(),
        local_node_id: "node-a".to_string(),
        local_env: NodeEnvironment::Prod,
        transport: transport.clone() as Arc<dyn Transport>,
        ledger: Arc::new(NeverCalledLedger),
        assignments: Arc::new(NeverProposedAssignments),
        leader_factory,
        follower_factory: unused_follower_factory,
        audit,
        leo_query_timeout: tentaflow_core::bus::replication::election::LEO_QUERY_TIMEOUT,
        majority_await_timeout: tentaflow_core::bus::replication::election::MAJORITY_AWAIT_TIMEOUT,
    });

    manager.apply_assignment(assignment).await;
    leader
        .svc
        .set_replication(manager.clone() as Arc<dyn ReplicationCoordinator>);

    wait_for_live_isr(
        &manager,
        &leader.ctx.org_id,
        TOPIC,
        &["node-a", "node-b", "node-c"],
    )
    .await
    .map_err(|e| format!("{label}: {e}"))?;

    Ok(ReplicatedTrio {
        leader,
        _follower_b: follower_b,
        _follower_c: follower_c,
        _manager: manager,
        _transport: transport,
    })
}

/// PLAN.md §5.2 P6: p99 publish->ACK, `acks=quorum`, RF=3, class `standard`
/// — measured ONLY in `standard` (PLAN-M2 §1g's own honesty note: `critical`
/// would fold local fsync latency on top, making the ≤15 ms target
/// physically unreachable and the gate dishonest, mirroring M1-WYNIKI.md's
/// P5 redefinition). `FollowerConfig{ack_every_n_batches: 1, ack_interval:
/// ZERO}` — an isolated, low-rate publish must not wait out the PRODUCTION
/// DEFAULT coalescing window (8 batches / 500 ms, `follower.rs`'s own
/// `Default` — batched for THROUGHPUT, not per-call latency) or this gate
/// would measure the coalescing window, not the replication path.
fn gate_p6(_c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let trio = match build_replicated_trio(
            "p6",
            FollowerConfig {
                ack_every_n_batches: 1,
                ack_interval: Duration::ZERO,
                ..FollowerConfig::default()
            },
        )
        .await
        {
            Ok(trio) => trio,
            Err(e) => {
                gate_blocked("P6", "trio never reached a quorum-ready ISR", &e);
                return;
            }
        };
        let ctx = trio.leader.ctx.clone();
        let svc = trio.leader.svc.clone();

        // n=1000 (M2-WYNIKI recommendation 12): at the measured mean (~3.9 ms)
        // this costs ~4 s of wall time, and a p99 gate is only certifiable
        // once n is large enough to describe the tail — at n=60 the sorted
        // "p99" was in practice the worst sample, not a percentile estimate.
        const N: usize = 1000;
        let mut warmup_left = 5;
        let mut latencies = Vec::with_capacity(N);
        let mut seed = 0u64;
        while latencies.len() < N {
            seed += 1;
            let batch = publish_batch(seed);
            let started = Instant::now();
            let result = svc.publish_async(&ctx, TOPIC, batch).await;
            let elapsed = started.elapsed();
            match result {
                Ok(_) => {
                    if warmup_left > 0 {
                        warmup_left -= 1;
                    } else {
                        latencies.push(elapsed);
                    }
                }
                Err(e) => {
                    gate_blocked(
                        "P6",
                        &format!(
                            "publish refused at sample {} — the live ISR held all three \
                             replicas before the loop started, so this is the replication \
                             path itself, not bench setup",
                            latencies.len()
                        ),
                        &e.to_string(),
                    );
                    return;
                }
            }
        }
        latencies.sort();
        let report = LatencyReport::from_sorted(&latencies);
        eprintln!(
            "P6 (publish->ACK, acks=quorum, RF=3, class standard, n={}): p50={:?} p95={:?} p99={:?} p999={:?} mean={:?}",
            report.n, report.p50, report.p95, report.p99, report.p999, report.mean
        );
        eprintln!(
            "P6 verdict (PLAN.md §5.2: Ref A <=15ms, Ref B <=10ms): p99={:?} -> {}",
            report.p99,
            if report.p99 <= Duration::from_millis(10) {
                "PASS (Ref A and Ref B)"
            } else if report.p99 <= Duration::from_millis(15) {
                "PASS (Ref A only)"
            } else {
                "FAIL"
            }
        );
    });
}

/// PLAN.md §5.2 P7: throughput at RF=3/`acks=quorum`, 1 KiB records, vs the
/// M1 P1 baseline (class `standard`, single producer/single partition:
/// 779.4k-1051.2k msg/s, `M1-WYNIKI.md`). Uses PRODUCTION DEFAULT
/// `FollowerConfig` (ack coalescing every 8 batches/500 ms) — unlike P6,
/// this gate measures sustained throughput, where coalesced acks are the
/// realistic production behavior, not an artifact to eliminate.
///
/// TWO windows are measured and reported under distinct labels (final-tree
/// pass, M2-WYNIKI recommendation 13):
/// * "P7 sequential" — one `await` at a time. With `ack_every_n_batches = 8`
///   a strictly sequential producer keeps exactly one batch in flight, so
///   the coalescing threshold is unreachable and every ack fires from the
///   500 ms timer; this window is a HARNESS-SHAPE lower bound (it measures
///   `1 / ack_interval`, not the replication path) and is kept only for
///   continuity with the banked 30.08 numbers.
/// * "P7 pipelined" — `PIPELINE_DEPTH` batches in flight (8 = exactly the
///   coalescing threshold, the smallest depth that lets the production ack
///   path fire on batch count rather than on the timer). This is the window
///   the gate's wording describes.
fn gate_p7(_c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let trio = match build_replicated_trio("p7", FollowerConfig::default()).await {
            Ok(trio) => trio,
            Err(e) => {
                gate_blocked("P7", "trio never reached a quorum-ready ISR", &e);
                return;
            }
        };
        let ctx = trio.leader.ctx.clone();
        let svc = trio.leader.svc.clone();

        const WARMUP_BATCHES: usize = 20;
        const MEASURE_SECS: u64 = 5; // reduced from bus_path.rs's fixed-count loops (module doc)

        let mut seed = 0u64;
        for _ in 0..WARMUP_BATCHES {
            seed += 1;
            if let Err(e) = svc
                .publish_async(&ctx, TOPIC, publish_batch(seed))
                .await
            {
                gate_blocked("P7", &format!("warmup publish {seed}"), &e.to_string());
                return;
            }
        }

        let mut total_records: u64 = 0;
        let mut total_bytes: u64 = 0;
        let deadline = Instant::now() + Duration::from_secs(MEASURE_SECS);
        let started = Instant::now();
        while Instant::now() < deadline {
            seed += 1;
            let batch = publish_batch(seed);
            let result = match svc.publish_async(&ctx, TOPIC, batch).await {
                Ok(result) => result,
                Err(e) => {
                    gate_blocked(
                        "P7",
                        &format!("publish {seed}, after {total_records} records already counted"),
                        &e.to_string(),
                    );
                    return;
                }
            };
            total_records += result.accepted as u64;
            total_bytes += result.accepted as u64 * RECORD_BYTES as u64;
        }
        let elapsed = started.elapsed();
        let msg_per_s = total_records as f64 / elaped_secs(elapsed);
        let mib_per_s = (total_bytes as f64 / (1024.0 * 1024.0)) / elaped_secs(elapsed);

        // M1-WYNIKI.md, P1, class `standard`, single producer (this bench's
        // own shape: one producer, one partition) — see this file's module
        // doc for why the comparison must stay same-class.
        const P1_STANDARD_LOW: f64 = 779_400.0;
        const P1_STANDARD_HIGH: f64 = 1_051_200.0;
        let ratio_low = msg_per_s / P1_STANDARD_HIGH;
        let ratio_high = msg_per_s / P1_STANDARD_LOW;

        eprintln!(
            "P7 sequential (RF=3, acks=quorum, class standard, {:.1}s window): {:.1}k msg/s, {:.1} MiB/s",
            elapsed.as_secs_f64(),
            msg_per_s / 1000.0,
            mib_per_s
        );
        eprintln!(
            "P7 sequential vs M1 P1 standard/single-producer ({:.1}k-{:.1}k msg/s): ratio {:.1}%-{:.1}%",
            P1_STANDARD_LOW / 1000.0,
            P1_STANDARD_HIGH / 1000.0,
            ratio_low * 100.0,
            ratio_high * 100.0
        );
        eprintln!(
            "P7 sequential verdict (PLAN.md §5.2: Ref A >=60% of P1, Ref B >=70%): {}",
            if ratio_low >= 0.70 {
                "PASS (Ref A and Ref B, conservative bound)"
            } else if ratio_high >= 0.60 {
                "PASS (Ref A, using the optimistic bound of P1's range)"
            } else {
                "FAIL"
            }
        );

        // ---- Pipelined window on the same trio (see gate_p7's doc): eight
        // producer tasks, one publish in flight each, disjoint seeds per
        // slot. `publish_async` is `block_in_place`-wrapped, so the
        // multi-thread runtime runs the slots concurrently.
        const PIPELINE_DEPTH: usize = 8;
        const PIPELINE_WARMUP_BATCHES: usize = 8;
        for _ in 0..PIPELINE_WARMUP_BATCHES {
            seed += 1;
            if let Err(e) = svc
                .publish_async(&ctx, TOPIC, publish_batch(seed))
                .await
            {
                gate_blocked("P7 pipelined", "warmup publish", &e.to_string());
                return;
            }
        }
        let pipeline_deadline = Instant::now() + Duration::from_secs(MEASURE_SECS);
        let pipeline_started = Instant::now();
        let mut slots = Vec::with_capacity(PIPELINE_DEPTH);
        for slot in 0..PIPELINE_DEPTH {
            let slot_svc = svc.clone();
            let slot_ctx = ctx.clone();
            slots.push(tokio::spawn(async move {
                // Disjoint seed streams: slot s publishes seeds
                // 1_000_000 + s + k * PIPELINE_DEPTH, never colliding with
                // the sequential window's seeds or with another slot's.
                let mut slot_seed = 1_000_000u64 + slot as u64;
                let mut slot_records = 0u64;
                let mut slot_bytes = 0u64;
                while Instant::now() < pipeline_deadline {
                    slot_seed += PIPELINE_DEPTH as u64;
                    let result = slot_svc
                        .publish_async(&slot_ctx, TOPIC, publish_batch(slot_seed))
                        .await
                        .map_err(|e| e.to_string())?;
                    slot_records += result.accepted as u64;
                    slot_bytes += result.accepted as u64 * RECORD_BYTES as u64;
                }
                Ok::<(u64, u64), String>((slot_records, slot_bytes))
            }));
        }
        let mut pipelined_records: u64 = 0;
        let mut pipelined_bytes: u64 = 0;
        for slot in slots {
            match slot.await {
                Ok(Ok((records, bytes))) => {
                    pipelined_records += records;
                    pipelined_bytes += bytes;
                }
                Ok(Err(e)) => {
                    gate_blocked(
                        "P7 pipelined",
                        &format!(
                            "publish after {} records already counted",
                            pipelined_records
                        ),
                        &e,
                    );
                    return;
                }
                Err(e) => {
                    gate_blocked("P7 pipelined", "producer task panicked", &e.to_string());
                    return;
                }
            }
        }
        let pipeline_elapsed = pipeline_started.elapsed();
        let p_msg_per_s = pipelined_records as f64 / elaped_secs(pipeline_elapsed);
        let p_mib_per_s =
            (pipelined_bytes as f64 / (1024.0 * 1024.0)) / elaped_secs(pipeline_elapsed);
        let p_ratio_low = p_msg_per_s / P1_STANDARD_HIGH;
        let p_ratio_high = p_msg_per_s / P1_STANDARD_LOW;

        eprintln!(
            "P7 pipelined (RF=3, acks=quorum, class standard, {} in flight, {:.1}s window): {:.1}k msg/s, {:.1} MiB/s",
            PIPELINE_DEPTH,
            pipeline_elapsed.as_secs_f64(),
            p_msg_per_s / 1000.0,
            p_mib_per_s
        );
        eprintln!(
            "P7 pipelined vs M1 P1 standard/single-producer ({:.1}k-{:.1}k msg/s): ratio {:.1}%-{:.1}%",
            P1_STANDARD_LOW / 1000.0,
            P1_STANDARD_HIGH / 1000.0,
            p_ratio_low * 100.0,
            p_ratio_high * 100.0
        );
        eprintln!(
            "P7 pipelined verdict (PLAN.md §5.2: Ref A >=60% of P1, Ref B >=70%): {}",
            if p_ratio_low >= 0.70 {
                "PASS (Ref A and Ref B, conservative bound)"
            } else if p_ratio_high >= 0.60 {
                "PASS (Ref A, using the optimistic bound of P1's range)"
            } else {
                "FAIL"
            }
        );
    });
}

fn elaped_secs(d: Duration) -> f64 {
    d.as_secs_f64().max(1e-9)
}

/// PLAN.md §5.2 P9: create-topic wall time, RF=1 vs RF=3, "through the fake
/// ledger with immediate majority" (this file's own phrasing, matching
/// PLAN-M2 §1g's task description) — `FakeLedgerWithImmediateMajority`
/// reports every replica as already-acknowledged the instant `propose` is
/// called, isolating "how fast is the LOCAL create_topic + assignment-apply
/// path" from any real ledger round-trip cost (measured separately, across
/// real processes, by the chaos test's own wall-clock).
fn gate_p9(_c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        const N: usize = 20;

        // ---- RF=1: plain BusService::create_topic, no coordinator at all
        // (M1's own path — PLAN-M2 §4.1 A1: RF=1 must be byte-for-byte M1).
        let mut rf1_latencies = Vec::with_capacity(N);
        for i in 0..N {
            let world = bench_world(&format!("p9-rf1-{i}"));
            let opts = TopicOptions {
                partitions: Some(1),
                replication_factor: Some(1),
                ..Default::default()
            };
            let started = Instant::now();
            world
                .svc
                .create_topic(&world.ctx, TOPIC, opts)
                .expect("create_topic RF=1");
            rf1_latencies.push(started.elapsed());
        }
        rf1_latencies.sort();
        let rf1_report = LatencyReport::from_sorted(&rf1_latencies);

        // ---- RF=3: the timed window is `propose` + `apply_assignment`
        // ONLY (see `started` below). The three local `create_topic` calls
        // and the whole factory/manager wiring are deliberately outside it:
        // `create_topic` at RF=3 costs what it costs at RF=1 on each node
        // (same local path), and the gate being probed here is the
        // assignment round-trip, not the per-node row write. Together these
        // are the two steps `bus/mod.rs::create_topic` WOULD do in one call
        // if its own replica-placement bootstrap could fire (see `tests/
        // process_three_node_bus_failover.rs`'s module doc for why it
        // currently cannot on a real `ReplicationManager`).
        let mut rf3_latencies = Vec::with_capacity(N);
        for i in 0..N {
            let leader = bench_world(&format!("p9-rf3-{i}-a"));
            let follower_b = bench_world(&format!("p9-rf3-{i}-b"));
            let follower_c = bench_world(&format!("p9-rf3-{i}-c"));
            let opts = topic_options();
            for world in [&leader, &follower_b, &follower_c] {
                world
                    .svc
                    .create_topic(&world.ctx, TOPIC, opts.clone())
                    .expect("create_topic RF=3");
            }

            let assignment = PartitionAssignment {
                instance_id: leader.svc.instance_id().to_string(),
                org_id: leader.ctx.org_id.clone(),
                topic: TOPIC.to_string(),
                partition: 0,
                leader_node_id: "node-a".to_string(),
                replicas: vec!["node-a".to_string(), "node-b".to_string(), "node-c".to_string()],
                isr: vec!["node-a".to_string(), "node-b".to_string(), "node-c".to_string()],
                leader_epoch: 1,
                updated_at_ms: now_ms(),
            };

            let mut followers: HashMap<String, Arc<GlueFollowerFactory>> = HashMap::new();
            followers.insert(
                "node-b".to_string(),
                Arc::new(GlueFollowerFactory::new(
                    "node-b",
                    NodeEnvironment::Prod,
                    follower_b.svc.clone()
                        as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
                    FollowerConfig::default(),
                )),
            );
            followers.insert(
                "node-c".to_string(),
                Arc::new(GlueFollowerFactory::new(
                    "node-c",
                    NodeEnvironment::Prod,
                    follower_c.svc.clone()
                        as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
                    FollowerConfig::default(),
                )),
            );
            let transport = Arc::new(DuplexTransport {
                assignment: assignment.clone(),
                followers,
                _keepalive: Arc::new(Mutex::new(Vec::new())),
            });
            let metrics = Arc::new(LeaderMetrics::new());
            let leader_factory = Arc::new(GlueLeaderFactory::new(
                "node-a",
                NodeEnvironment::Prod,
                leader.svc.clone() as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
                transport.clone() as Arc<dyn Transport>,
                LeaderConfig::default(),
                metrics,
            ));
            let unused_follower_factory: Arc<
                dyn tentaflow_core::bus::replication::manager::FollowerRunnerFactory,
            > = Arc::new(GlueFollowerFactory::new(
                "node-a",
                NodeEnvironment::Prod,
                leader.svc.clone() as Arc<dyn tentaflow_core::bus::replication::glue::PartitionProvider>,
                FollowerConfig::default(),
            ));
            let audit = Arc::new(AuditLogReplAudit::new(leader.db.clone(), "node-a"));
            let assignments = Arc::new(FakeAssignmentProposer);
            let manager = ReplicationManager::new(ReplicationManagerConfig {
                instance_id: leader.svc.instance_id().to_string(),
                local_node_id: "node-a".to_string(),
                local_env: NodeEnvironment::Prod,
                transport: transport.clone() as Arc<dyn Transport>,
                ledger: Arc::new(FakeLedgerWithImmediateMajority {
                    replicas: vec!["node-b".to_string(), "node-c".to_string()],
                }),
                assignments: assignments.clone(),
                leader_factory,
                follower_factory: unused_follower_factory,
                audit,
                leo_query_timeout: tentaflow_core::bus::replication::election::LEO_QUERY_TIMEOUT,
                majority_await_timeout: tentaflow_core::bus::replication::election::MAJORITY_AWAIT_TIMEOUT,
            });

            let started = Instant::now();
            assignments
                .propose(assignment.clone())
                .expect("propose");
            manager.apply_assignment(assignment).await;
            rf3_latencies.push(started.elapsed());
        }
        rf3_latencies.sort();
        let rf3_report = LatencyReport::from_sorted(&rf3_latencies);

        eprintln!(
            "P9 RF=1 create_topic (n={}): p50={:?} p99={:?} mean={:?}",
            rf1_report.n, rf1_report.p50, rf1_report.p99, rf1_report.mean
        );
        eprintln!(
            "P9 RF=3 propose+apply_assignment only (n={}, fake ledger, immediate majority; \
             create_topic and all wiring are outside the timed window): p50={:?} p99={:?} mean={:?}",
            rf3_report.n, rf3_report.p50, rf3_report.p99, rf3_report.mean
        );
        eprintln!(
            "P9 verdict (PLAN.md §5.2: RF=1 <=500ms, RF=3 <=2s): RF=1 {} / RF=3 {}",
            if rf1_report.p99 <= Duration::from_millis(500) { "PASS" } else { "FAIL" },
            if rf3_report.p99 <= Duration::from_secs(2) { "PASS" } else { "FAIL" },
        );
    });
}

criterion_group!(benches, gate_p6, gate_p7, gate_p9);
criterion_main!(benches);
