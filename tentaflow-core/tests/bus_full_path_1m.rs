//! M1 gate: 1,000,000 messages through the FULL TentaBus service path
//! (`bus::BusService::publish` -> on-disk log -> `ConsumerHandle::fetch`/
//! `commit`), driven exclusively through the crate's public API from
//! outside `tentaflow_core` — modeled on `tests/bus_demo_seed.rs`'s own
//! "open a real `BusService` with an allow-all authorizer" shape, but with
//! a private `tempfile` DB/bus dir per test rather than an operator-pointed
//! `.runtime/` directory.
//!
//! SINGLETON NOTE: unlike `bus_demo_seed.rs`, every test in this file calls
//! `BusService::new` DIRECTLY (not the process-global `bus::init`/
//! `bus::global` pair). `bus::init` caches its `Arc<BusService>` in a
//! `OnceLock` for the whole test **process** — `bus_demo_seed.rs`'s own
//! header warns its two `#[ignore]`d tests must be run "one at a time" for
//! exactly this reason. This file's three tests (a non-ignored 10k smoke
//! test plus two `#[ignore]`d heavy gates) are designed to be safe to run
//! together in one process precisely BECAUSE `BusService::new` is public
//! and side-effect-free with respect to any global state: each test gets
//! its own fully independent service, database, and `bus_dir`. The two
//! `#[ignore]`d tests should still be invoked ONE AT A TIME in practice
//! (`cargo test --release --test bus_full_path_1m -- --ignored --nocapture
//! <exact test name>`) purely to keep this gate's timing/throughput
//! numbers uncontended by the other heavy test's threads on a shared
//! machine, not because of any shared-state hazard.
//!
//! RECOVERY LIMITATION (see `recovery_after_abrupt_close_keeps_every_acked_record`'s
//! doc): a genuine OS-level `kill -9` (or the M0 `tentaflow-bus` unit
//! tests' own crash-recovery pattern, which truncates segment file BYTES
//! directly using private, non-`pub` `Segment`/`dir` helpers internal to
//! that crate's `#[cfg(test)]` module) is not reachable from this
//! integration test file. What this file exercises instead is a real
//! `Drop` of the whole `BusService` (releasing every partition directory's
//! `flock` and closing every file descriptor) followed by a fresh
//! `BusService::new` against the same `bus_dir`/database — the strongest
//! "did the durable on-disk state survive a full close+reopen" check
//! reachable through the public surface. This is not a lesser check for
//! data that was already ACKed: `DurabilityPolicy::FsyncBatch` means
//! `publish()` only returns `Ok` once its batch's `fsync` has already
//! completed, so every accepted record here was durable on disk BEFORE
//! this test ever drops the service — there is nothing left for a
//! "cleaner" or "dirtier" shutdown to lose or preserve differently.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use tentaflow_core::bus::{
    self, groups, producer, topics, BusAction, BusCallContext, BusInitConfig, BusService,
    BusServiceError, ConsumerConfig, FetchedRecordMeta, PublishBatch, PublishRecord, PublishResult,
    TopicPartition,
};

const ORIGIN: &str = "bus-full-path-1m-test";

/// Allow-all authorizer — this file tests the storage/delivery path, not
/// RBAC (already covered by `src/bus/mod.rs`'s own `#[cfg(test)]` suite).
struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

fn call_ctx(org_id: &str) -> BusCallContext {
    BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse("tentabus-00000001")
            .expect("valid instance id"),
        org_id: org_id.to_string(),
        actor: Some("m1-gate".to_string()),
        correlation_id: Some("bus-full-path-1m".to_string()),
        origin: ORIGIN.to_string(),
    }
}

/// Opens an independent `BusService` — see this file's module doc for why
/// this calls `BusService::new` directly instead of the process-global
/// `bus::init`. `db_path` is `Path::new(":memory:")` for a one-shot test
/// (fresh state every time, no disk I/O for the admin-plane SQLite side)
/// or a real file path for a test that needs to reopen the SAME database
/// later (the crash-recovery variant).
fn open_service(bus_dir: PathBuf, db_path: &Path) -> Arc<BusService> {
    let db = tentaflow_core::db::init(db_path).expect("init db");
    let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
    bus::db::migrate(&local_conn).expect("migrate local db");
    let local_db: tentaflow_core::db::DbPool =
        Arc::new(tentaflow_core::db::Db::from_connection(local_conn));
    Arc::new(
        BusService::new(BusInitConfig {
            instance_id: bus::instance::BusInstanceId::parse("tentabus-00000001")
                .expect("valid instance id"),
            local_db,
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 200_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service"),
    )
}

/// Publishes `batch`, transparently backing off and retrying on the two
/// error variants that mean "not now, try again shortly" rather than a
/// real failure (`QuotaExceeded`/`Throttled`) — a sustained multi-threaded
/// publish load against a single org's token bucket is expected to hit
/// these occasionally under this test's default org quota (200k msg/s),
/// which is a normal, retryable backpressure signal, not a test failure.
fn publish_with_backoff(
    svc: &BusService,
    ctx: &BusCallContext,
    topic: &str,
    batch: PublishBatch,
) -> PublishResult {
    let mut attempts = 0u32;
    loop {
        match svc.publish(ctx, topic, batch.clone()) {
            Ok(result) => return result,
            Err(BusServiceError::QuotaExceeded { retry_after_ms })
            | Err(BusServiceError::Throttled { retry_after_ms }) => {
                attempts += 1;
                assert!(
                    attempts < 20_000,
                    "publish('{topic}') backed off {attempts} times without ever succeeding \
                     — the service appears stuck, not merely throttled"
                );
                std::thread::sleep(Duration::from_millis(retry_after_ms.max(1) as u64));
            }
            Err(e) => panic!("publish('{topic}') failed: {e}"),
        }
    }
}

// ---- Record encoding: `K-<producer>-<seq>` key, seq embedded in payload too ----

/// Every record's payload is exactly this many bytes (1 KiB) regardless of
/// how small its embedded `producer`/`seq` header text is — the remainder
/// is filler, matching a realistic fixed-size-ish workload rather than a
/// handful of bytes per message.
const PAYLOAD_LEN: usize = 1024;

fn make_key(producer: usize, seq: usize) -> Bytes {
    Bytes::from(format!("K-{producer}-{seq}"))
}

fn parse_key(key: &[u8]) -> (usize, usize) {
    let text = std::str::from_utf8(key).expect("key must be valid utf-8");
    let rest = text
        .strip_prefix("K-")
        .unwrap_or_else(|| panic!("key '{text}' missing 'K-' prefix"));
    let (p, s) = rest
        .split_once('-')
        .unwrap_or_else(|| panic!("key '{text}' missing producer/seq separator"));
    (
        p.parse()
            .unwrap_or_else(|_| panic!("bad producer in key '{text}'")),
        s.parse()
            .unwrap_or_else(|_| panic!("bad seq in key '{text}'")),
    )
}

fn make_payload(producer: usize, seq: usize) -> Bytes {
    let header = format!("P{producer}S{seq}|");
    assert!(
        header.len() < PAYLOAD_LEN,
        "producer/seq header '{header}' does not fit in a {PAYLOAD_LEN}-byte payload"
    );
    let mut buf = vec![b'.'; PAYLOAD_LEN];
    buf[..header.len()].copy_from_slice(header.as_bytes());
    Bytes::from(buf)
}

fn parse_payload(payload: &[u8]) -> (usize, usize) {
    let text = std::str::from_utf8(payload).expect("payload must be valid utf-8");
    let bar = text
        .find('|')
        .expect("payload missing header delimiter '|'");
    let header = &text[..bar];
    let header = header
        .strip_prefix('P')
        .unwrap_or_else(|| panic!("payload header '{header}' missing 'P' prefix"));
    let (p, s) = header
        .split_once('S')
        .unwrap_or_else(|| panic!("payload header '{header}' missing 'S' separator"));
    (
        p.parse()
            .unwrap_or_else(|_| panic!("bad producer in payload header '{header}'")),
        s.parse()
            .unwrap_or_else(|_| panic!("bad seq in payload header '{header}'")),
    )
}

fn make_record(producer: usize, seq: usize) -> PublishRecord {
    PublishRecord {
        key: Some(make_key(producer, seq)),
        headers: vec![],
        payload: make_payload(producer, seq),
        timestamp_ms: now_ms(),
        schema_id: 0,
    }
}

/// Verifies one fetched record against the exact position (`partition`,
/// `expected_seq`) the caller expects to see next, cross-checks the key
/// against the payload's own embedded copy, and records one delivery for
/// `(producer, seq)` in the shared flat counter array — `delivery_counts`
/// is sized `producers * records_per_producer`, one `AtomicU8` per distinct
/// record, so a delivery count other than exactly 1 at the very end is a
/// direct, per-record loss/duplication signal.
///
/// Asserting `seq == expected_seq` (not just "seq > last seq seen") proves
/// gap-free, strictly ordered, exactly-once-per-call delivery within this
/// partition in one step — every owning producer maps 1:1 onto its
/// partition (see the gate test's own doc), so offset order and `seq`
/// order must coincide exactly.
fn verify_and_record(
    rec: &FetchedRecordMeta,
    expected_partition: u32,
    expected_seq: u64,
    delivery_counts: &[AtomicU8],
    records_per_producer: usize,
) {
    assert_eq!(
        rec.partition, expected_partition,
        "record delivered on unexpected partition"
    );
    let key = rec
        .key
        .as_ref()
        .expect("every record published by this test carries a key");
    let (producer, seq) = parse_key(key);
    assert_eq!(
        producer as u32, expected_partition,
        "producer/partition mapping violated (producer {producer} on partition {})",
        rec.partition
    );
    assert_eq!(
        seq as u64, expected_seq,
        "partition {} delivered seq {seq} out of order (expected {expected_seq})",
        rec.partition
    );
    let (payload_producer, payload_seq) = parse_payload(&rec.payload);
    assert_eq!(payload_producer, producer, "key/payload producer mismatch");
    assert_eq!(payload_seq, seq, "key/payload seq mismatch");
    let idx = producer * records_per_producer + seq;
    delivery_counts[idx].fetch_add(1, Ordering::Relaxed);
}

// ---- Producer side ------------------------------------------------------

fn run_producer(
    svc: Arc<BusService>,
    ctx: BusCallContext,
    topic: String,
    producer_idx: usize,
    records_per_producer: usize,
    batch_size: usize,
    retry_batches: Arc<HashSet<(usize, usize)>>,
    total_accepted: Arc<AtomicU64>,
    total_duplicate_calls: Arc<AtomicU64>,
) {
    let producer_id = format!("m1gate-producer-{producer_idx}");
    let num_batches = records_per_producer / batch_size;
    for batch_idx in 0..num_batches {
        let records: Vec<PublishRecord> = (0..batch_size)
            .map(|i| make_record(producer_idx, batch_idx * batch_size + i))
            .collect();
        // Every record from this producer routes to ONE dedicated
        // partition (`partition: Some(producer_idx)`) instead of relying
        // on per-record key hashing — with exactly as many producers as
        // partitions, this gives a simple, verifiable invariant: within
        // partition P, every record was written by producer P, in strict
        // send order, so consuming it in offset order IS consuming it in
        // `seq` order.
        let batch = PublishBatch {
            partition: Some(producer_idx as u32),
            producer: Some(producer::ProducerIdentity {
                producer_id: producer_id.clone(),
                epoch: 1,
                base_seq: batch_idx as u64,
            }),
            records,
        };
        let result = publish_with_backoff(&svc, &ctx, &topic, batch.clone());
        assert!(
            !result.duplicate,
            "fresh batch producer={producer_idx} batch={batch_idx} reported as duplicate"
        );
        assert_eq!(result.accepted, batch_size as u32);
        assert_eq!(result.deduplicated, 0);
        total_accepted.fetch_add(result.accepted as u64, Ordering::Relaxed);

        if retry_batches.contains(&(producer_idx, batch_idx)) {
            // Re-publish the EXACT same batch (same producer identity,
            // same base_seq, same records) to exercise the producer-seq
            // idempotency layer (PLAN §3.1 layer 1) — must be recognized
            // as a full-batch replay and contribute nothing new.
            let retry_result = publish_with_backoff(&svc, &ctx, &topic, batch);
            assert!(
                retry_result.duplicate,
                "retried batch producer={producer_idx} batch={batch_idx} was NOT deduplicated"
            );
            assert_eq!(retry_result.accepted, 0);
            total_accepted.fetch_add(retry_result.accepted as u64, Ordering::Relaxed);
            total_duplicate_calls.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ---- Consumer side --------------------------------------------------------

const FETCH_MAX_BYTES: usize = 8 * 1024 * 1024;
const FETCH_MAX_WAIT_MS: u32 = 200;
/// Generous per-thread wall-clock ceiling so a real regression (a stuck
/// fetch loop, a broken offset check) fails fast with a clear message
/// instead of hanging the whole gate indefinitely.
const CONSUMER_STUCK_AFTER: Duration = Duration::from_secs(900);

/// Drains two partitions this thread owns exclusively (fetch immediately
/// followed by commit of exactly what was just processed, every call) —
/// under normal operation (no crash injected) this can never redeliver
/// anything: `AtLeastOnce`-style processing here means "commit only after
/// the record is accounted for", not "commit before/without processing".
///
/// NOTE ON REDUNDANT READS: `ConsumerHandle::fetch` always scans EVERY
/// subscribed partition in the same fixed order starting from partition 0
/// (see its own doc — this is not true round-robin across calls); since
/// this test opens one `open_consumer` per thread for the SAME topic
/// (which always subscribes to every partition of that topic), a thread
/// that owns higher-numbered partitions locally re-reads (but never
/// commits or double-counts) every lower-numbered partition's data first.
/// This is wasted I/O, not a correctness issue — `partition_handle`'s
/// `DashMap::entry(..).or_try_insert_with` ensures every thread shares the
/// SAME already-open `Partition` per (org, topic, partition), so this
/// never double-opens a directory flock.
fn run_normal_consumer(
    svc: Arc<BusService>,
    ctx: BusCallContext,
    topic: String,
    group: String,
    owned: [u32; 2],
    target: u64,
    delivery_counts: Arc<Vec<AtomicU8>>,
    records_per_producer: usize,
) -> [u64; 2] {
    let handle = svc
        .open_consumer(
            &ctx,
            &group,
            std::slice::from_ref(&topic),
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer (normal)");
    let mut local = [0u64; 2];
    let deadline = Instant::now() + CONSUMER_STUCK_AFTER;
    while local[0] < target || local[1] < target {
        assert!(
            Instant::now() < deadline,
            "normal consumer owning partitions {owned:?} made no progress within \
             {CONSUMER_STUCK_AFTER:?} (local={local:?}, target={target})"
        );
        let batch = handle
            .fetch(FETCH_MAX_BYTES, FETCH_MAX_WAIT_MS)
            .expect("fetch (normal)");
        for rec in &batch.records {
            for (i, &p) in owned.iter().enumerate() {
                if rec.partition == p && local[i] < target {
                    verify_and_record(rec, p, local[i], &delivery_counts, records_per_producer);
                    local[i] += 1;
                }
            }
        }
        handle
            .commit(&[
                (
                    TopicPartition {
                        topic: topic.clone(),
                        partition: owned[0],
                    },
                    local[0],
                ),
                (
                    TopicPartition {
                        topic: topic.clone(),
                        partition: owned[1],
                    },
                    local[1],
                ),
            ])
            .expect("commit (normal)");
    }
    local
}

struct RecoveryOutcome {
    /// Number of `p_crash` records fetched-but-never-committed by the
    /// FIRST (dropped) handle — exactly the set that gets redelivered once
    /// the second handle reopens against the same, still-`0`, durable
    /// commit. This is the test's ground truth for "how many deliberately
    /// provoked duplicates should exist".
    pre_drop_p_crash: u64,
    final_normal: u64,
    final_crash: u64,
}

/// Owns two partitions like `run_normal_consumer`, but deliberately
/// mishandles ONE of them (`p_crash`) to provoke a real, bounded
/// at-least-once redelivery: fetches (and verifies/records) some of
/// `p_crash` WITHOUT ever committing it, then drops that `ConsumerHandle`
/// — releasing its directory locks/reader state exactly like an abrupt
/// disconnect would — and opens a brand new handle under the SAME group.
/// The new handle resumes `p_crash` from its last DURABLE commit (still
/// `0`), so it redelivers every record the first handle already processed
/// but never acknowledged. `p_normal` is drained normally throughout
/// (commit immediately after every processed record) so it never
/// redelivers anything, proving the duplicate is specific to the
/// provoked boundary, not a general property of this consumer group.
fn run_recovery_consumer(
    svc: Arc<BusService>,
    ctx: BusCallContext,
    topic: String,
    group: String,
    p_normal: u32,
    p_crash: u32,
    target: u64,
    crash_threshold: u64,
    delivery_counts: Arc<Vec<AtomicU8>>,
    records_per_producer: usize,
) -> RecoveryOutcome {
    let deadline = Instant::now() + CONSUMER_STUCK_AFTER;
    let mut local_normal = 0u64;
    let mut local_crash = 0u64;

    {
        let handle_a = svc
            .open_consumer(
                &ctx,
                &group,
                std::slice::from_ref(&topic),
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .expect("open_consumer (phase 1)");
        while local_crash < crash_threshold {
            assert!(
                Instant::now() < deadline,
                "recovery consumer phase 1 made no progress within {CONSUMER_STUCK_AFTER:?} \
                 (local_normal={local_normal}, local_crash={local_crash})"
            );
            let batch = handle_a
                .fetch(FETCH_MAX_BYTES, FETCH_MAX_WAIT_MS)
                .expect("fetch (phase 1)");
            let mut normal_advanced = false;
            for rec in &batch.records {
                if rec.partition == p_normal && local_normal < target {
                    verify_and_record(
                        rec,
                        p_normal,
                        local_normal,
                        &delivery_counts,
                        records_per_producer,
                    );
                    local_normal += 1;
                    normal_advanced = true;
                } else if rec.partition == p_crash && local_crash < crash_threshold {
                    verify_and_record(
                        rec,
                        p_crash,
                        local_crash,
                        &delivery_counts,
                        records_per_producer,
                    );
                    local_crash += 1;
                }
            }
            if normal_advanced {
                handle_a
                    .commit(&[(
                        TopicPartition {
                            topic: topic.clone(),
                            partition: p_normal,
                        },
                        local_normal,
                    )])
                    .expect("commit p_normal (phase 1)");
            }
            // `p_crash`'s offset is DELIBERATELY never committed here —
            // this is the provoked at-least-once boundary the test exists
            // to demonstrate.
        }
        // `handle_a` drops at the end of this block: directory
        // locks/reader state for every partition it opened are released
        // without `p_crash`'s in-flight progress ever having been
        // durably committed.
    }
    let pre_drop_p_crash = local_crash;

    let handle_b = svc
        .open_consumer(
            &ctx,
            &group,
            std::slice::from_ref(&topic),
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer (phase 2)");
    let mut local_crash_b = 0u64;
    while local_normal < target || local_crash_b < target {
        assert!(
            Instant::now() < deadline,
            "recovery consumer phase 2 made no progress within {CONSUMER_STUCK_AFTER:?} \
             (local_normal={local_normal}, local_crash_b={local_crash_b})"
        );
        let batch = handle_b
            .fetch(FETCH_MAX_BYTES, FETCH_MAX_WAIT_MS)
            .expect("fetch (phase 2)");
        for rec in &batch.records {
            if rec.partition == p_normal && local_normal < target {
                verify_and_record(
                    rec,
                    p_normal,
                    local_normal,
                    &delivery_counts,
                    records_per_producer,
                );
                local_normal += 1;
            } else if rec.partition == p_crash {
                // Re-verifies records [0, pre_drop_p_crash) against the
                // SAME expected seq sequence as phase 1 (this handle's own
                // `local_crash_b` starts at 0 again) — proving the
                // redelivery is a clean, ordered REPLAY of exactly what was
                // already seen, not corruption or reordering.
                verify_and_record(
                    rec,
                    p_crash,
                    local_crash_b,
                    &delivery_counts,
                    records_per_producer,
                );
                local_crash_b += 1;
            }
        }
        handle_b
            .commit(&[
                (
                    TopicPartition {
                        topic: topic.clone(),
                        partition: p_normal,
                    },
                    local_normal,
                ),
                (
                    TopicPartition {
                        topic: topic.clone(),
                        partition: p_crash,
                    },
                    local_crash_b,
                ),
            ])
            .expect("commit (phase 2)");
    }

    RecoveryOutcome {
        pre_drop_p_crash,
        final_normal: local_normal,
        final_crash: local_crash_b,
    }
}

/// Runs one topic's full 8-producer/4-consumer scenario end to end,
/// returning `(publish_elapsed, consume_elapsed, delivery_counts,
/// recovery, total_accepted, total_duplicate_calls, retry_count)` so both
/// the 1M gate and the 10k smoke test can share every invariant check
/// while only differing in size.
struct ScenarioResult {
    publish_elapsed: Duration,
    consume_elapsed: Duration,
    delivery_counts: Vec<u8>,
    recovery: RecoveryOutcome,
    total_accepted: u64,
    total_duplicate_calls: u64,
    retry_count: usize,
}

#[allow(clippy::too_many_arguments)]
fn run_scenario(
    org_id: &str,
    topic: &str,
    group: &str,
    producers: usize,
    records_per_producer: usize,
    batch_size: usize,
    crash_threshold: u64,
    retry_batches: HashSet<(usize, usize)>,
) -> ScenarioResult {
    assert_eq!(
        producers % 2,
        0,
        "this harness pairs producers/partitions two at a time across consumer threads"
    );
    let total_records = producers * records_per_producer;

    let tmp = tempfile::tempdir().expect("create temp dir");
    let bus_dir = tmp.path().join("bus");
    let svc = open_service(bus_dir, Path::new(":memory:"));
    let ctx = call_ctx(org_id);

    svc.create_topic(
        &ctx,
        topic,
        topics::TopicOptions {
            partitions: Some(producers as u32),
            // Explicit Prod-shape durability (PLAN §7.1's "fsync_batch is
            // the Prod default") rather than relying on this process's
            // resolved `NodeEnvironment`, whatever it happens to default
            // to for a freshly migrated test database.
            durability: Some(topics::DurabilityPolicy::FsyncBatch),
            ..Default::default()
        },
    )
    .expect("create_topic");

    let delivery_counts: Arc<Vec<AtomicU8>> =
        Arc::new((0..total_records).map(|_| AtomicU8::new(0)).collect());
    let total_accepted = Arc::new(AtomicU64::new(0));
    let total_duplicate_calls = Arc::new(AtomicU64::new(0));
    let retry_batches = Arc::new(retry_batches);
    let retry_count = retry_batches.len();

    let publish_start = Instant::now();
    std::thread::scope(|s| {
        for producer_idx in 0..producers {
            let svc = Arc::clone(&svc);
            let ctx = ctx.clone();
            let topic = topic.to_string();
            let retry_batches = Arc::clone(&retry_batches);
            let total_accepted = Arc::clone(&total_accepted);
            let total_duplicate_calls = Arc::clone(&total_duplicate_calls);
            s.spawn(move || {
                run_producer(
                    svc,
                    ctx,
                    topic,
                    producer_idx,
                    records_per_producer,
                    batch_size,
                    retry_batches,
                    total_accepted,
                    total_duplicate_calls,
                );
            });
        }
    });
    let publish_elapsed = publish_start.elapsed();

    assert_eq!(
        total_accepted.load(Ordering::Relaxed),
        total_records as u64,
        "PublishResult.accepted must sum to exactly the number of unique records published"
    );
    assert_eq!(
        total_duplicate_calls.load(Ordering::Relaxed),
        retry_count as u64,
        "every injected retry must be reported back as PublishResult.duplicate"
    );

    // 4 consumer threads: 3 plain (2 owned partitions each, commit
    // immediately after every processed record) plus 1 recovery thread
    // that deliberately drops-and-reopens mid-way on its second partition.
    let pairs: Vec<[u32; 2]> = (0..producers)
        .step_by(2)
        .map(|p| [p as u32, p as u32 + 1])
        .collect();
    let (recovery_pair, normal_pairs) = pairs.split_last().expect("at least one partition pair");

    let mut recovery_outcome: Option<RecoveryOutcome> = None;
    let consume_start = Instant::now();
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for &owned in normal_pairs {
            let svc = Arc::clone(&svc);
            let ctx = ctx.clone();
            let topic = topic.to_string();
            let group = group.to_string();
            let delivery_counts = Arc::clone(&delivery_counts);
            handles.push(s.spawn(move || {
                run_normal_consumer(
                    svc,
                    ctx,
                    topic,
                    group,
                    owned,
                    records_per_producer as u64,
                    delivery_counts,
                    records_per_producer,
                )
            }));
        }
        let recovery_handle = {
            let svc = Arc::clone(&svc);
            let ctx = ctx.clone();
            let topic = topic.to_string();
            let group = group.to_string();
            let delivery_counts = Arc::clone(&delivery_counts);
            let [p_normal, p_crash] = *recovery_pair;
            s.spawn(move || {
                run_recovery_consumer(
                    svc,
                    ctx,
                    topic,
                    group,
                    p_normal,
                    p_crash,
                    records_per_producer as u64,
                    crash_threshold,
                    delivery_counts,
                    records_per_producer,
                )
            })
        };
        for h in handles {
            h.join().expect("normal consumer thread panicked");
        }
        recovery_outcome = Some(
            recovery_handle
                .join()
                .expect("recovery consumer thread panicked"),
        );
    });
    let consume_elapsed = consume_start.elapsed();
    let recovery = recovery_outcome.expect("recovery consumer must have run");

    // `lag()` from a fresh handle opened AFTER every producing/consuming
    // handle above has already been joined/dropped — reads the durable
    // committed offsets, not any in-process cursor, so this is a true
    // end-to-end confirmation that every partition's commit reached its
    // log end.
    let verify_handle = svc
        .open_consumer(
            &ctx,
            group,
            std::slice::from_ref(&topic.to_string()),
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer (final lag check)");
    for (tp, lag) in verify_handle.lag().expect("lag") {
        assert_eq!(
            lag, 0,
            "partition {} of '{}' has non-zero lag ({lag}) after every consumer finished",
            tp.partition, tp.topic
        );
    }

    let delivery_counts: Vec<u8> = delivery_counts
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();

    ScenarioResult {
        publish_elapsed,
        consume_elapsed,
        delivery_counts,
        recovery,
        total_accepted: total_accepted.load(Ordering::Relaxed),
        total_duplicate_calls: total_duplicate_calls.load(Ordering::Relaxed),
        retry_count,
    }
}

/// Shared final-verification pass over the flat `delivery_counts` array:
/// no zero (loss), unique count matches `total_records` exactly, and the
/// only entries ever seen more than once are the ones the recovery
/// consumer's provoked redelivery predicts — everywhere else, at-least-once
/// degrades to exactly-once because nothing else in this test ever fails
/// to commit what it fetched.
fn verify_delivery_counts(
    result: &ScenarioResult,
    producers: usize,
    records_per_producer: usize,
    recovery_producer: usize,
) {
    let total_records = producers * records_per_producer;
    assert_eq!(result.delivery_counts.len(), total_records);

    let mut zero = 0u64;
    let mut unique = 0u64;
    let mut duplicated = 0u64;
    let mut total_deliveries = 0u64;
    for &c in &result.delivery_counts {
        total_deliveries += c as u64;
        match c {
            0 => zero += 1,
            1 => unique += 1,
            _ => {
                unique += 1;
                duplicated += 1;
            }
        }
    }

    assert_eq!(
        zero, 0,
        "{zero} record(s) were never delivered at all (loss)"
    );
    assert_eq!(
        unique, total_records as u64,
        "unique delivered record count mismatch"
    );
    assert_eq!(
        duplicated, result.recovery.pre_drop_p_crash,
        "duplicate count must equal exactly the deliberately provoked \
         fetched-but-uncommitted redelivery window"
    );
    assert_eq!(
        total_deliveries,
        total_records as u64 + result.recovery.pre_drop_p_crash,
        "total deliveries must equal unique records plus exactly the provoked duplicates"
    );

    // Pinpoint exactly WHICH records were duplicated: only the recovery
    // producer's [0, pre_drop_p_crash) prefix, delivered exactly twice;
    // everything else (including the REST of that same producer's own
    // records) exactly once.
    let base = recovery_producer * records_per_producer;
    for seq in 0..result.recovery.pre_drop_p_crash as usize {
        assert_eq!(
            result.delivery_counts[base + seq],
            2,
            "expected the provoked redelivery window to show count=2 at seq={seq}"
        );
    }
    for seq in result.recovery.pre_drop_p_crash as usize..records_per_producer {
        assert_eq!(
            result.delivery_counts[base + seq],
            1,
            "expected exactly-once delivery outside the provoked redelivery window at seq={seq}"
        );
    }
}

fn print_summary(
    label: &str,
    result: &ScenarioResult,
    producers: usize,
    records_per_producer: usize,
) {
    let total_records = producers * records_per_producer;
    let total_bytes = total_records as u64 * PAYLOAD_LEN as u64;
    let publish_secs = result.publish_elapsed.as_secs_f64().max(1e-9);
    let consume_secs = result.consume_elapsed.as_secs_f64().max(1e-9);
    println!("\n=== {label} summary ===");
    println!(
        "records:            {total_records} ({producers} producers x {records_per_producer})"
    );
    println!(
        "payload:            {PAYLOAD_LEN} bytes/record, durability=fsync_batch (Prod default)"
    );
    println!(
        "publish:            {:.3}s, {:.0} msg/s, {:.2} MB/s (accepted={}, retries_injected={}, duplicate_calls={})",
        result.publish_elapsed.as_secs_f64(),
        total_records as f64 / publish_secs,
        (total_bytes as f64 / (1024.0 * 1024.0)) / publish_secs,
        result.total_accepted,
        result.retry_count,
        result.total_duplicate_calls,
    );
    println!(
        "consume:            {:.3}s, {:.0} msg/s, {:.2} MB/s",
        result.consume_elapsed.as_secs_f64(),
        total_records as f64 / consume_secs,
        (total_bytes as f64 / (1024.0 * 1024.0)) / consume_secs,
    );
    let unique = result.delivery_counts.iter().filter(|&&c| c > 0).count();
    println!(
        "unique delivered:   {unique} / {total_records}; provoked duplicates: {}",
        result.recovery.pre_drop_p_crash
    );
    println!(
        "total wall time:    {:.3}s\n",
        result.publish_elapsed.as_secs_f64() + result.consume_elapsed.as_secs_f64()
    );
}

/// M1 gate: 1,000,000 records (8 producers x 125,000, 1000-record batches,
/// 1 KiB payloads) through 8 partitions with `fsync_batch` durability, a 4
/// consumer thread group deliberately provoking one bounded at-least-once
/// redelivery, and full loss/duplication accounting. Numbers this prints
/// go into the M1 report — see `print_summary`.
#[test]
#[ignore]
fn one_million_messages_through_the_full_path_without_loss() {
    const PRODUCERS: usize = 8;
    const RECORDS_PER_PRODUCER: usize = 125_000;
    const BATCH_SIZE: usize = 1_000;
    const RECOVERY_PRODUCER: usize = 7; // owns partition 7, the crash/reopen boundary
    const CRASH_THRESHOLD: u64 = 12_000; // records fetched-but-uncommitted before the drop

    // ~1% of the 8 * 125 = 1000 total batches, spread across every
    // producer, deterministically (not random — a flaky gate test that
    // occasionally injects 0 retries would silently stop testing the
    // idempotency layer on the runs that matter most).
    let retry_batches: HashSet<(usize, usize)> = [
        (0, 10),
        (1, 45),
        (2, 80),
        (3, 15),
        (4, 60),
        (5, 100),
        (6, 30),
        (7, 90),
        (0, 120),
        (3, 3),
    ]
    .into_iter()
    .collect();

    let result = run_scenario(
        "m1-gate",
        "m1.gate.bus",
        "m1-gate-consumers",
        PRODUCERS,
        RECORDS_PER_PRODUCER,
        BATCH_SIZE,
        CRASH_THRESHOLD,
        retry_batches,
    );

    verify_delivery_counts(&result, PRODUCERS, RECORDS_PER_PRODUCER, RECOVERY_PRODUCER);
    assert!(
        result.recovery.pre_drop_p_crash > 0,
        "the provoked redelivery scenario must actually have redelivered something"
    );
    assert_eq!(result.recovery.final_normal, RECORDS_PER_PRODUCER as u64);
    assert_eq!(result.recovery.final_crash, RECORDS_PER_PRODUCER as u64);

    print_summary(
        "one_million_messages_through_the_full_path_without_loss",
        &result,
        PRODUCERS,
        RECORDS_PER_PRODUCER,
    );
}

/// Fast, non-ignored smoke variant (10,000 records) that keeps this gate's
/// path alive in every normal `cargo test` run — same producer-partition
/// mapping, idempotency retries, and duplicate/loss accounting as the 1M
/// gate, just two orders of magnitude smaller so it finishes in well under
/// 30s even in an unoptimized debug build.
#[test]
fn ten_thousand_messages_smoke_path() {
    const PRODUCERS: usize = 4;
    const RECORDS_PER_PRODUCER: usize = 2_500;
    const BATCH_SIZE: usize = 250;
    const RECOVERY_PRODUCER: usize = 3;
    const CRASH_THRESHOLD: u64 = 400;

    let retry_batches: HashSet<(usize, usize)> =
        [(0, 2), (1, 5), (2, 8), (3, 1)].into_iter().collect();

    let result = run_scenario(
        "m1-gate-smoke",
        "m1.gate.bus.smoke",
        "m1-gate-smoke-consumers",
        PRODUCERS,
        RECORDS_PER_PRODUCER,
        BATCH_SIZE,
        CRASH_THRESHOLD,
        retry_batches,
    );

    verify_delivery_counts(&result, PRODUCERS, RECORDS_PER_PRODUCER, RECOVERY_PRODUCER);
    assert!(result.recovery.pre_drop_p_crash > 0);
    assert_eq!(result.recovery.final_normal, RECORDS_PER_PRODUCER as u64);
    assert_eq!(result.recovery.final_crash, RECORDS_PER_PRODUCER as u64);

    print_summary(
        "ten_thousand_messages_smoke_path",
        &result,
        PRODUCERS,
        RECORDS_PER_PRODUCER,
    );
}

/// Crash-recovery variant: publishes 100,000 records, then drops the
/// WHOLE `BusService` (every partition writer thread/flock, not just one
/// `ConsumerHandle`) and reopens a fresh one against the same `bus_dir`
/// and database file, then consumes everything from scratch and asserts
/// every previously-ACKed record is present exactly once. See this file's
/// module doc for why this is a real `Drop`+reopen rather than a true
/// `kill -9`/mid-write segment truncation (that pattern lives in
/// `tentaflow-bus`'s own private, `#[cfg(test)]`-only `Segment` unit
/// tests and is not reachable from here).
#[test]
#[ignore]
fn recovery_after_abrupt_close_keeps_every_acked_record() {
    const TOTAL_RECORDS: usize = 100_000;
    const BATCH_SIZE: usize = 1_000;
    const PARTITIONS: u32 = 4;
    const ORG_ID: &str = "m1-gate-recovery";
    const TOPIC: &str = "m1.gate.recovery";
    const GROUP: &str = "m1-gate-recovery-consumers";

    let tmp = tempfile::tempdir().expect("create temp dir");
    let bus_dir = tmp.path().join("bus");
    let db_path = tmp.path().join("recovery.db");
    let ctx = call_ctx(ORG_ID);

    let publish_elapsed = {
        let svc = open_service(bus_dir.clone(), &db_path);
        svc.create_topic(
            &ctx,
            TOPIC,
            topics::TopicOptions {
                partitions: Some(PARTITIONS),
                durability: Some(topics::DurabilityPolicy::FsyncBatch),
                ..Default::default()
            },
        )
        .expect("create_topic");

        let start = Instant::now();
        let mut total_accepted = 0u32;
        let num_batches = TOTAL_RECORDS / BATCH_SIZE;
        for batch_idx in 0..num_batches {
            let records: Vec<PublishRecord> = (0..BATCH_SIZE)
                .map(|i| make_record(0, batch_idx * BATCH_SIZE + i))
                .collect();
            let batch = PublishBatch {
                partition: None, // let key hashing spread across all 4 partitions
                producer: Some(producer::ProducerIdentity {
                    producer_id: "m1gate-recovery-producer".to_string(),
                    epoch: 1,
                    base_seq: batch_idx as u64,
                }),
                records,
            };
            let result = publish_with_backoff(&svc, &ctx, TOPIC, batch);
            assert!(!result.duplicate);
            total_accepted += result.accepted;
        }
        assert_eq!(total_accepted, TOTAL_RECORDS as u32);
        let elapsed = start.elapsed();

        // `svc` (and everything it opened: every partition's writer
        // thread/directory flock, the `_meta` fjall database) is dropped
        // here, at the end of this block — see the module doc for why a
        // real `Drop` is the strongest "did the durable state survive a
        // close+reopen" signal reachable from outside the crate, and why
        // it is not a weaker check than a true process kill for data that
        // was already ACKed under `fsync_batch` durability.
        elapsed
    };

    // Reopen fresh against the SAME bus_dir + database file.
    let svc2 = open_service(bus_dir, &db_path);
    let handle = svc2
        .open_consumer(
            &ctx,
            GROUP,
            std::slice::from_ref(&TOPIC.to_string()),
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer after reopen");

    let mut seen: HashSet<(usize, usize)> = HashSet::with_capacity(TOTAL_RECORDS);
    let mut committed: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let deadline = Instant::now() + CONSUMER_STUCK_AFTER;
    let consume_start = Instant::now();
    loop {
        let lag = handle.lag().expect("lag");
        if lag.iter().all(|(_, l)| *l == 0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recovery consumption made no progress within {CONSUMER_STUCK_AFTER:?}"
        );
        let batch = handle
            .fetch(FETCH_MAX_BYTES, FETCH_MAX_WAIT_MS)
            .expect("fetch after reopen");
        let mut advanced: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        for rec in &batch.records {
            let key = rec.key.as_ref().expect("record must carry a key");
            let (producer, seq) = parse_key(key);
            let (payload_producer, payload_seq) = parse_payload(&rec.payload);
            assert_eq!(payload_producer, producer);
            assert_eq!(payload_seq, seq);
            assert!(
                seen.insert((producer, seq)),
                "record (producer={producer}, seq={seq}) was delivered more than once after reopen"
            );
            let next = advanced.entry(rec.partition).or_insert(rec.offset);
            *next = (*next).max(rec.offset + 1);
        }
        let commits: Vec<(TopicPartition, u64)> = advanced
            .into_iter()
            .map(|(partition, next_offset)| {
                let entry = committed.entry(partition).or_insert(0);
                *entry = (*entry).max(next_offset);
                (
                    TopicPartition {
                        topic: TOPIC.to_string(),
                        partition,
                    },
                    *entry,
                )
            })
            .collect();
        if !commits.is_empty() {
            handle.commit(&commits).expect("commit after reopen");
        }
    }
    let consume_elapsed = consume_start.elapsed();

    assert_eq!(
        seen.len(),
        TOTAL_RECORDS,
        "every ACKed record must be present exactly once after reopening against the same dir/db"
    );
    for (_, lag) in handle.lag().expect("final lag") {
        assert_eq!(lag, 0);
    }

    println!("\n=== recovery_after_abrupt_close_keeps_every_acked_record summary ===");
    println!("records published before drop: {TOTAL_RECORDS}");
    println!(
        "publish elapsed (before drop): {:.3}s",
        publish_elapsed.as_secs_f64()
    );
    println!(
        "consume elapsed (after reopen): {:.3}s",
        consume_elapsed.as_secs_f64()
    );
    println!("unique records recovered:      {}", seen.len());
    println!("=== end summary ===\n");
}
