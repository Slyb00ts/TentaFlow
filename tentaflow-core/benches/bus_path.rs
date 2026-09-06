// ===== File: benches/bus_path.rs — TentaBus M1 full-service-path gates =====
// (PLAN /Users/critix/repos/rust/SUM/tentabus/PLAN.md §5.1-5.4: P1, P4, P5,
// P10, P13)
//
// Unlike `tentaflow-bus/benches/log_perf.rs`/`device_ceiling.rs` (M0's own
// benches, which drive the bare engine — `tentaflow_bus::Partition` directly
// — to characterize the log/fsync path in isolation), everything in this
// file goes through `tentaflow_core::bus::BusService`: `publish`,
// `open_consumer`/`fetch`/`commit`, `note_delivery_failure`,
// `run_retention_sweep`. That is the ONLY thing M1 adds on top of M0's
// engine — authz, quota, per-record header stamping, the topic-config
// cache, DLQ, retention — so every number here should be read as "M0's
// engine number, plus whatever the service layer costs on top", not as an
// independent measurement of the disk.
//
// `harness = false` (same rationale as `log_perf.rs`): every gate drives its
// own explicit warm-up/measure loop and prints a human-readable summary via
// `eprintln!` — one absolute number, its full durability/compression
// config, and (where a device ceiling applies) % of that ceiling — rather
// than relying on Criterion's own statistical sampler, whose warm-up phase
// calls a routine an indeterminate number of times with no signal
// distinguishing warm-up from measurement (see `log_perf.rs`'s module doc).
// `criterion_group!`/`criterion_main!` are kept only so the usual
// `cargo bench --bench bus_path -- --noplot` CLI still works and Criterion's
// own HTML report machinery is available if a caller wants it later; no gate
// in this file registers a `bench_with_input`/`iter_custom` measurement of
// its own (mirroring `log_perf.rs`'s `bench_multi_producer`, which found
// that registering the same multi-thread workload a second time under
// Criterion's sampler added minutes of wall time for a number the report
// never used).
//
// DEVICE CEILING (re-measured today via `tentaflow-bus/benches/
// device_ceiling.rs`, `cargo bench --bench device_ceiling -- --noplot`,
// same machine, same disk, "growing"-layout numbers — the engine's
// `RollPolicy::preallocate` defaults to `false`, M1-R2 decision 5, so a
// growing file is what `Partition`/`BusService` actually write into; NOT
// the "preallocated" layout `device_ceiling.rs` also measures for
// isolating fsync-only cost):
//   1 MiB chunk, sync_data  (= engine `Durability::FsyncBatch`): 140.51 MiB/s, 7.12 ms/op
//   1 MiB chunk, F_FULLFSYNC (= engine `Durability::FsyncBatchFull`): 148.10 MiB/s, 6.75 ms/op
//   1 MiB chunk, os (no fsync): 1.36 GiB/s
//   64 KiB chunk, sync_data: 12.74 MiB/s, 4.91 ms/op
// These are ~13% BELOW M0-WYNIKI.md's original 26.08 numbers (161.9 MiB/s /
// 6.18 ms at 1 MiB) — Criterion's own regression detector flagged this on
// every single durability/chunk combination it re-ran, so this is a real,
// reproducible difference in today's disk/thermal/background-load state,
// not measurement noise on one data point. M1-WYNIKI.md's % figures use
// TODAY's numbers as the ceiling, exactly because a % against a ceiling
// measured on a different day, under different conditions, would not be
// honest.
//
// OWNER DECISION B (28.08, `src/bus/topics.rs`): `DurabilityClass::Standard`
// (the new DEFAULT class for a topic whose `TopicOptions` leaves both
// `durability`/`durability_class` unset) resolves in Prod/Test to
// `DurabilityPolicy::FsyncInterval{ms: 50}` — the writer thread's ACK
// returns as soon as the write itself lands, not after that batch's fsync;
// a background fsync then runs at most once per 50 ms. `DurabilityClass::
// Critical` still resolves to `FsyncBatchFull` (fsync BEFORE ack) in every
// environment — unchanged from before decision B. Every DLQ topic
// (`dlq::dlq_topic_options`) is ALWAYS `FsyncInterval{ms: 50}` regardless of
// its source topic's class. Because `FsyncInterval`'s ack does not wait on
// its own fsync, its P1 throughput ceiling is NOT the `fsync_batch`/
// `fsync_batch_full` per-op number above — it is the raw `os` (no-fsync)
// ceiling (1.36 GiB/s = 1392.64 MiB/s), minus the amortized cost of one
// ~7 ms fsync stall roughly every 50 ms of wall time. Every `[P1]` line
// below that reports a `standard`/`fsync_interval` result states which
// ceiling it is a percentage of, for exactly this reason.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion};

use tentaflow_core::bus::{
    dlq, groups, topics, BusCallContext, BusService, BusServiceError, ConsumerConfig, PublishBatch,
    PublishRecord,
};

mod support;
use support::{
    bench_world, byte_accounting_topic_options, now_ms, pseudo_random_bytes, LatencyReport,
};

const RECORD_SIZE: usize = 1024; // 1 KiB, every gate's record size (PLAN §5.2)
const RECORDS_PER_BATCH: usize = 1024; // ~1 MiB/batch at 1 KiB records (PLAN P1: "batch 1 MiB / ~1000 records")

/// `keyed`: every record in ONE call gets the SAME key (derived from `seed`
/// alone, not `seed` x record index) — not a per-record-unique key. A real
/// high-throughput keyed producer buffers per PARTITION client-side and
/// flushes one batch per partition (Kafka's own producer does exactly
/// this); `BusService::publish` accepts one `PublishBatch` that CAN span
/// multiple partitions, but internally groups-then-appends them ONE AT A
/// TIME within that single call (`BusService::publish`'s `for (partition,
/// records) in groups` loop, `bus/mod.rs`) — so a batch whose records hash
/// to N different partitions pays N sequential engine appends (and, under
/// `fsync_batch`, N sequential fsyncs) inside what looks like one call, with
/// none of group commit's cross-THREAD batching benefit. A per-record
/// unique key (this function's first version) hits that pathology on
/// EVERY call; a same-key-per-call batch keeps each call single-partition,
/// which is what "8 producers, per-record keyed routing, 8 partitions"
/// is meant to measure — parallel, largely-uncontended writers, one per
/// partition — while `seed` still varying call-to-call still spreads
/// different calls across different partitions over the run.
fn build_records(n: usize, seed: u64, keyed: bool) -> Vec<PublishRecord> {
    let key = keyed.then(|| Bytes::from(format!("k-{seed}")));
    (0..n)
        .map(|i| {
            let payload = pseudo_random_bytes(
                RECORD_SIZE,
                seed ^ (i as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93),
            );
            PublishRecord {
                key: key.clone(),
                headers: Vec::new(),
                payload: Bytes::from(payload),
                timestamp_ms: now_ms(),
                schema_id: 0,
            }
        })
        .collect()
}

fn make_batch(n: usize, seed: u64, partition: Option<u32>, keyed: bool) -> PublishBatch {
    PublishBatch {
        partition,
        producer: None,
        records: build_records(n, seed, keyed),
    }
}

/// Publishes `batch`, retrying on `BusServiceError::Throttled` (the
/// `Partition`'s bounded write channel — 256 slots, `BusService::
/// partition_handle`'s hardcoded `channel_capacity` — filling faster than
/// the single writer thread's fsync can drain it, exactly the group-commit
/// backpressure PLAN §5.3.7 calls for). `BusServiceError`'s own doc notes
/// the engine's returned (unconsumed) batch bytes are DROPPED on
/// translation to `Throttled`, so the caller must retry with its OWN copy —
/// `PublishBatch`/`PublishRecord` are `Clone` over `Bytes`, so cloning
/// before each attempt is a cheap refcount bump, not a payload copy.
fn publish_with_retry(
    svc: &BusService,
    ctx: &BusCallContext,
    topic: &str,
    batch: &PublishBatch,
) -> tentaflow_core::bus::PublishResult {
    loop {
        match svc.publish(ctx, topic, batch.clone()) {
            Ok(res) => return res,
            Err(BusServiceError::Throttled { retry_after_ms }) => {
                thread::sleep(Duration::from_millis(retry_after_ms.clamp(1, 5) as u64));
            }
            Err(e) => panic!("publish('{topic}') failed: {e}"),
        }
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

// =============================================================================
// P1 — full path throughput (PLAN §5.2 P1, M1 column: >= 300k msg/s / 300 MB/s)
// =============================================================================

/// P1's device-ceiling reference for a given resolved `DurabilityPolicy`
/// (owner decision B): `FsyncBatch`/`FsyncBatchFull` block the ACK on their
/// own fsync, so their ceiling is `device_ceiling.rs`'s per-op fsync number
/// (module doc above); `FsyncInterval`'s ACK does NOT wait on its own
/// fsync, so its ceiling is the raw `os` (no-fsync) write bandwidth instead
/// — comparing it against the fsync number would understate how close to
/// "no fsync at all" it actually runs. `Os` reuses the same raw-bandwidth
/// ceiling for the same reason (no fsync at all, ever).
fn p1_ceiling_for(durability: topics::DurabilityPolicy) -> (f64, &'static str) {
    match durability {
        topics::DurabilityPolicy::FsyncBatch => (140.51, "fsync_batch"),
        topics::DurabilityPolicy::FsyncBatchFull => (148.10, "fsync_batch_full"),
        topics::DurabilityPolicy::FsyncInterval { .. } | topics::DurabilityPolicy::Os => {
            (1392.64, "os")
        }
    }
}

/// Single producer, 1 partition — the latency/throughput FLOOR this disk's
/// fsync physics impose on any one writer, matching M0-WYNIKI.md's own
/// "single-producer reported separately as the latency ceiling" framing
/// (M0's coordinator redefinition of P1, see that file's final verdict
/// section). Measured under BOTH of owner decision B's durability classes
/// (topic name suffixed with the class so each gets its own topic/
/// partition): `Standard` (`FsyncInterval{ms:50}` in this bench's default-
/// Prod environment — the new headline number for a default topic) and
/// `Critical` (`FsyncBatchFull` — unchanged durability-first reference).
/// `measure` lets a caller reduce the sample count for a variant that is
/// only a cross-check, not the headline (see `gate_p1`).
fn p1_single_producer(
    world: &support::BenchWorld,
    class: topics::DurabilityClass,
    measure: usize,
) -> (f64, f64, LatencyReport, topics::DurabilityPolicy) {
    let topic = format!("p1-single-{}", class.as_str());
    let cfg = world
        .svc
        .create_topic(&world.ctx, &topic, byte_accounting_topic_options(1, class))
        .expect("create_topic p1-single");
    let durability = cfg.durability;

    const WARMUP: usize = 10;
    let mut seed = 0u64;
    for _ in 0..WARMUP {
        seed += 1;
        let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
        publish_with_retry(&world.svc, &world.ctx, &topic, &batch);
    }
    let before = world
        .svc
        .partition_stats(&world.ctx, &topic, 0)
        .unwrap()
        .size_bytes;
    let mut latencies = Vec::with_capacity(measure);
    let start = Instant::now();
    for _ in 0..measure {
        seed += 1;
        let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
        let t0 = Instant::now();
        publish_with_retry(&world.svc, &world.ctx, &topic, &batch);
        latencies.push(t0.elapsed());
    }
    let elapsed = start.elapsed();
    let after = world
        .svc
        .partition_stats(&world.ctx, &topic, 0)
        .unwrap()
        .size_bytes;
    latencies.sort_unstable();
    let report = LatencyReport::from_sorted(&latencies);
    let msg_s = (measure * RECORDS_PER_BATCH) as f64 / elapsed.as_secs_f64();
    let mib_s = mib(after - before) / elapsed.as_secs_f64();
    let (ceiling_mib_s, ceiling_label) = p1_ceiling_for(durability);
    eprintln!(
        "[P1] service single-producer  class={} durability={} compression=none  n={measure}  msg/s={msg_s:>10.0}  MiB/s={mib_s:>8.2} ({:>5.1}% of {:.2} MiB/s {} device ceiling)  p50={:>7.2?} p99={:>7.2?}",
        class.as_str(),
        durability.to_wire_string(),
        mib_s / ceiling_mib_s * 100.0,
        ceiling_mib_s,
        ceiling_label,
        report.p50,
        report.p99,
    );
    (msg_s, mib_s, report, durability)
}

/// Bare-engine equivalent of `p1_single_producer` — same record/batch shape,
/// same durability, `tentaflow_bus::Partition::append_batch` directly, NO
/// `BusService` in the loop at all — quantifies exactly what the service
/// layer (authz, quota, per-record header stamping, topic-config cache
/// lookup) costs on top of M0's own engine number, in this SAME process
/// (same machine state, same moment) rather than trusting a stale M0
/// number for the comparison. `label` distinguishes the class-specific
/// engine directory/log line; `durability` is the raw engine policy
/// matching the service-side class under test (`FsyncInterval(50ms)` for
/// `Standard`, `FsyncBatchFull` for `Critical`) so the overhead comparison
/// stays apples-to-apples per class.
fn p1_single_producer_engine_only(
    root: &std::path::Path,
    label: &str,
    durability: tentaflow_bus::Durability,
    measure: usize,
) -> f64 {
    let dir = root.join(format!("engine-only-single-{label}"));
    let partition =
        tentaflow_bus::Partition::open(&dir, tentaflow_bus::RollPolicy::default(), durability, 64)
            .expect("open raw partition");

    const WARMUP: usize = 10;
    let build = |seed: u64| -> Bytes {
        let mut b = tentaflow_bus::BatchBuilder::with_capacity(
            0,
            1,
            RECORDS_PER_BATCH * (RECORD_SIZE + 32),
        );
        for i in 0..RECORDS_PER_BATCH {
            let payload = pseudo_random_bytes(
                RECORD_SIZE,
                seed ^ (i as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93),
            );
            b.push(tentaflow_bus::RecordInput::new(
                Bytes::from(payload),
                i as i64,
            ))
            .expect("record within wire limits");
        }
        b.build().expect("non-empty batch")
    };
    let mut seed = 0u64;
    for _ in 0..WARMUP {
        seed += 1;
        partition.append_batch(build(seed)).expect("append_batch");
    }
    let start = Instant::now();
    for _ in 0..measure {
        seed += 1;
        partition.append_batch(build(seed)).expect("append_batch");
    }
    let elapsed = start.elapsed();
    let msg_s = (measure * RECORDS_PER_BATCH) as f64 / elapsed.as_secs_f64();
    eprintln!("[P1] engine-only single-producer (same shape, same process)  label={label}  n={measure}  msg/s={msg_s:>10.0}");
    let _ = std::fs::remove_dir_all(&dir);
    msg_s
}

struct MultiProducerShape {
    producers: usize,
    per_thread_appends: usize,
}

/// Runs `shape.producers` threads concurrently publishing to `topic`
/// (`partition`: `Some(p)` pins the WHOLE run to one partition — group
/// commit; `None` lets each record's independent key hash across every
/// partition the topic has — parallel, uncontended writers). Returns
/// aggregate msg/s, MiB/s (from `partition_stats`, summed per touched
/// partition), and merged per-append p50/p99.
fn run_multi_producer(
    world: &support::BenchWorld,
    topic: &str,
    partitions: u32,
    pin_partition: Option<u32>,
    shape: &MultiProducerShape,
) -> (f64, f64, LatencyReport) {
    let before: u64 = (0..partitions)
        .map(|p| {
            world
                .svc
                .partition_stats(&world.ctx, topic, p)
                .unwrap()
                .size_bytes
        })
        .sum();

    let start = Instant::now();
    let handles: Vec<_> = (0..shape.producers)
        .map(|p| {
            let svc = Arc::clone(&world.svc);
            let ctx = world.ctx.clone();
            let topic = topic.to_string();
            let per_thread_appends = shape.per_thread_appends;
            thread::spawn(move || {
                let mut latencies = Vec::with_capacity(per_thread_appends);
                for i in 0..per_thread_appends {
                    let seed = (p * 1_000_000 + i) as u64;
                    let batch = make_batch(
                        RECORDS_PER_BATCH,
                        seed,
                        pin_partition,
                        pin_partition.is_none(),
                    );
                    let t0 = Instant::now();
                    publish_with_retry(&svc, &ctx, &topic, &batch);
                    latencies.push(t0.elapsed());
                }
                latencies
            })
        })
        .collect();
    let mut all_latencies: Vec<Duration> = Vec::new();
    for h in handles {
        all_latencies.extend(h.join().unwrap());
    }
    let elapsed = start.elapsed();
    let after: u64 = (0..partitions)
        .map(|p| {
            world
                .svc
                .partition_stats(&world.ctx, topic, p)
                .unwrap()
                .size_bytes
        })
        .sum();

    all_latencies.sort_unstable();
    let report = LatencyReport::from_sorted(&all_latencies);
    let total_records = shape.producers * shape.per_thread_appends * RECORDS_PER_BATCH;
    let msg_s = total_records as f64 / elapsed.as_secs_f64();
    let mib_s = mib(after - before) / elapsed.as_secs_f64();
    (msg_s, mib_s, report)
}

/// `DurabilityPolicy` -> matching raw `tentaflow_bus::Durability` for the
/// engine-only comparisons below (`FsyncBatch` never appears here post-
/// decision-B — only `Critical`'s `FsyncBatchFull` and `Standard`'s
/// `FsyncInterval` do — but it is handled for completeness/robustness
/// against a future class resolving to it).
fn engine_durability_for(durability: topics::DurabilityPolicy) -> tentaflow_bus::Durability {
    match durability {
        topics::DurabilityPolicy::Os => tentaflow_bus::Durability::Os,
        topics::DurabilityPolicy::FsyncBatch => tentaflow_bus::Durability::FsyncBatch,
        topics::DurabilityPolicy::FsyncBatchFull => tentaflow_bus::Durability::FsyncBatchFull,
        topics::DurabilityPolicy::FsyncInterval { ms } => {
            tentaflow_bus::Durability::FsyncInterval(Duration::from_millis(ms as u64))
        }
    }
}

/// Both of owner decision B's durability classes, each paired with the
/// `MultiProducerShape` to run it with. `Critical` runs at HALF the sample
/// count of `Standard`: `Standard`/`FsyncInterval{50}` is the new headline
/// this session exists to establish; `Critical`/`FsyncBatchFull` is an
/// unchanged reference already characterized at full sample count in the
/// "Po decyzji B" section's predecessor (the original M1 run, `fsync_batch`
/// which measures ~equal to `fsync_batch_full` on this disk) — a reduced
/// sample here is a cross-check that decision B did not move it, not a
/// fresh characterization from scratch.
fn p1_classes() -> [(topics::DurabilityClass, usize); 2] {
    [
        (topics::DurabilityClass::Standard, 100),
        (topics::DurabilityClass::Critical, 50),
    ]
}

fn gate_p1(_c: &mut Criterion) {
    let world = bench_world("p1");
    let engine_root = tempfile::Builder::new()
        .prefix("tentaflow-core-bus-path-p1-engine-")
        .tempdir()
        .expect("engine-only tempdir");

    // -- Single producer: both classes. --------------------------------
    for (class, measure) in p1_classes() {
        let (_msg_s, _mib_s, _r, durability) = p1_single_producer(&world, class, measure);
        let label = format!("single-{}", class.as_str());
        p1_single_producer_engine_only(
            engine_root.path(),
            &label,
            engine_durability_for(durability),
            measure,
        );
    }

    // -- 8 producers / 8-partition topic, per-record keyed routing ------
    // (parallel, largely-uncontended writers, one per partition), both
    // classes.
    for (class, per_thread) in p1_classes() {
        let shape = MultiProducerShape {
            producers: 8,
            per_thread_appends: per_thread,
        };
        let topic_keyed = format!("p1-keyed-8p-{}", class.as_str());
        let cfg = world
            .svc
            .create_topic(
                &world.ctx,
                &topic_keyed,
                byte_accounting_topic_options(8, class),
            )
            .expect("create_topic p1-keyed-8p");
        let (msg_s, mib_s, r) = run_multi_producer(&world, &topic_keyed, 8, None, &shape);
        let (ceiling_mib_s, ceiling_label) = p1_ceiling_for(cfg.durability);
        eprintln!(
            "[P1] service 8 producers / 8 partitions (keyed)  class={} durability={}  n={per_thread}/thread  msg/s={msg_s:>10.0}  MiB/s={mib_s:>8.2} ({:>5.1}% of {:.2} MiB/s {} ceiling x8 partitions)  p50={:>7.2?} p99={:>7.2?}",
            class.as_str(),
            cfg.durability.to_wire_string(),
            mib_s / (ceiling_mib_s * 8.0) * 100.0,
            ceiling_mib_s * 8.0,
            ceiling_label,
            r.p50,
            r.p99,
        );
    }

    // -- 8 producers / 1 partition, explicit target — group commit ------
    // (both classes), plus its engine-only equivalent + service-layer
    // overhead, same process, per class.
    for (class, per_thread) in p1_classes() {
        let shape = MultiProducerShape {
            producers: 8,
            per_thread_appends: per_thread,
        };
        let topic_group = format!("p1-group-commit-1p-{}", class.as_str());
        let cfg = world
            .svc
            .create_topic(
                &world.ctx,
                &topic_group,
                byte_accounting_topic_options(1, class),
            )
            .expect("create_topic p1-group-commit-1p");
        let (msg_s, mib_s, r) = run_multi_producer(&world, &topic_group, 1, Some(0), &shape);
        let (ceiling_mib_s, ceiling_label) = p1_ceiling_for(cfg.durability);
        eprintln!(
            "[P1] service 8 producers / 1 partition (group commit)  class={} durability={}  n={per_thread}/thread  msg/s={msg_s:>10.0}  MiB/s={mib_s:>8.2} ({:>5.1}% of {:.2} MiB/s {} single-partition ceiling)  p50={:>7.2?} p99={:>7.2?}",
            class.as_str(),
            cfg.durability.to_wire_string(),
            mib_s / ceiling_mib_s * 100.0,
            ceiling_mib_s,
            ceiling_label,
            r.p50,
            r.p99,
        );

        // Engine-only equivalent of the group-commit case, same process,
        // same class's raw durability.
        let engine_durability = engine_durability_for(cfg.durability);
        let engine_group_dir = engine_root
            .path()
            .join(format!("engine-only-group-commit-{}", class.as_str()));
        let engine_partition = Arc::new(
            tentaflow_bus::Partition::open(
                &engine_group_dir,
                tentaflow_bus::RollPolicy::default(),
                engine_durability,
                256,
            )
            .expect("open raw group-commit partition"),
        );
        let start = Instant::now();
        let handles: Vec<_> = (0..shape.producers)
            .map(|p| {
                let partition = Arc::clone(&engine_partition);
                let per_thread_appends = shape.per_thread_appends;
                thread::spawn(move || {
                    for i in 0..per_thread_appends {
                        let seed = (p * 1_000_000 + i) as u64;
                        let mut b = tentaflow_bus::BatchBuilder::with_capacity(
                            0,
                            1,
                            RECORDS_PER_BATCH * (RECORD_SIZE + 32),
                        );
                        for r in 0..RECORDS_PER_BATCH {
                            let payload = pseudo_random_bytes(
                                RECORD_SIZE,
                                (seed as u64) ^ (r as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93),
                            );
                            b.push(tentaflow_bus::RecordInput::new(
                                Bytes::from(payload),
                                r as i64,
                            ))
                            .expect("record within wire limits");
                        }
                        let mut batch = b.build().expect("non-empty batch");
                        loop {
                            match partition.append_batch(batch) {
                                Ok(_) => break,
                                Err(tentaflow_bus::BusError::Throttled {
                                    batch: returned, ..
                                }) => {
                                    batch = returned;
                                    continue;
                                }
                                Err(e) => panic!("unexpected engine error: {e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let elapsed = start.elapsed();
        let engine_msg_s = (shape.producers * shape.per_thread_appends * RECORDS_PER_BATCH) as f64
            / elapsed.as_secs_f64();
        eprintln!(
            "[P1] engine-only 8 producers / 1 partition (group commit, same process)  class={}  n={per_thread}/thread  msg/s={engine_msg_s:>10.0}",
            class.as_str(),
        );
        eprintln!(
            "[P1] service-layer overhead, group commit, class={}: {:.1}% ({:.0} -> {:.0} msg/s)",
            class.as_str(),
            (1.0 - msg_s / engine_msg_s) * 100.0,
            engine_msg_s,
            msg_s,
        );
    }

    let _ = std::fs::remove_dir_all(engine_root.path());
}

// =============================================================================
// P5 — p99 publish -> ACK, single 1 KiB x ~1000-record batch
// (PLAN §5.2 P5: <= 5 ms; M1 stretch column <= 3 ms)
//
// Owner decision B REDEFINES P5's scope: a `Standard`-class topic's ACK
// returns as soon as the write lands, not after any fsync (`FsyncInterval`,
// see the module doc's DECISION B block), so "publish->ACK, dominated by
// one batch's fsync" no longer describes what a default topic pays — only
// a `Critical`-class topic still blocks its ACK on `FsyncBatchFull`. P5
// below therefore measures `Critical` ONLY; `Standard`'s end-to-end
// producer-to-consumer number is P4's `standard` variant instead.
// =============================================================================

fn gate_p5(_c: &mut Criterion) {
    let world = bench_world("p5");
    let topic = "p5-single-batch-ack-critical";
    let cfg = world
        .svc
        .create_topic(
            &world.ctx,
            topic,
            byte_accounting_topic_options(1, topics::DurabilityClass::Critical),
        )
        .expect("create_topic p5");

    const WARMUP: usize = 10;
    const MEASURE: usize = 300;
    let mut seed = 0u64;
    for _ in 0..WARMUP {
        seed += 1;
        let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
        publish_with_retry(&world.svc, &world.ctx, topic, &batch);
    }
    let mut latencies = Vec::with_capacity(MEASURE);
    for _ in 0..MEASURE {
        seed += 1;
        let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
        let t0 = Instant::now();
        publish_with_retry(&world.svc, &world.ctx, topic, &batch);
        latencies.push(t0.elapsed());
    }
    latencies.sort_unstable();
    let r = LatencyReport::from_sorted(&latencies);
    eprintln!(
        "[P5] publish->ACK, 1 batch of {RECORDS_PER_BATCH} x {RECORD_SIZE}B, class=critical durability={}  n={}  p50={:>7.2?} p95={:>7.2?} p99={:>7.2?} p999={:>7.2?}  mean={:>7.2?}  (owner decision B: P5 now applies to Critical-class topics ONLY -- a Standard topic's ACK does not wait on fsync, see P4's standard variant for its end-to-end number)",
        cfg.durability.to_wire_string(),
        r.n, r.p50, r.p95, r.p99, r.p999, r.mean,
    );
}

// =============================================================================
// P4 — p99 publish -> consume, 1 node, 1 KiB, ~1 ms linger
// (PLAN §5.2 P4: <= 3 ms; M1 stretch column <= 2 ms)
// =============================================================================

/// Producer publishes a small batch every ~1 ms (the "linger" window);
/// consumer polls in a tight `fetch` loop with a small `max_wait_ms`.
/// Per-record end-to-end latency is `fetch_wall_clock_ms - record.
/// timestamp_ms` — MILLISECOND resolution (the only clock `PublishRecord::
/// timestamp_ms`/`FetchedRecordMeta::timestamp_ms` carry), so individual
/// samples are quantized to whole milliseconds even though the true latency
/// is finer-grained; with a target this close to the quantization step
/// (<=2-3 ms), p50/p99 read off this data are directionally correct but
/// carry roughly +-1 ms of measurement noise — stated explicitly here and
/// in the report rather than presented as more precise than the instrument
/// actually is.
///
/// Exactly one of `durability`/`durability_class` should be `Some` — mirrors
/// `TopicOptions`' own "explicit policy wins over class" rule (owner
/// decision B) so a caller can exercise either the advanced override (`os`/
/// `fsync_batch`, this gate's pre-decision-B variants, kept for comparison)
/// or the friendly class (`standard`, decision B's new default path).
/// Returns the latency report plus the ACTUAL resolved `DurabilityPolicy`
/// (read back off the created `TopicConfig`) for the caller to log.
fn run_p4(
    world: &support::BenchWorld,
    topic: &str,
    durability: Option<topics::DurabilityPolicy>,
    durability_class: Option<topics::DurabilityClass>,
) -> (LatencyReport, topics::DurabilityPolicy) {
    let cfg = world
        .svc
        .create_topic(
            &world.ctx,
            topic,
            topics::TopicOptions {
                partitions: Some(1),
                durability,
                durability_class,
                ..Default::default()
            },
        )
        .expect("create_topic p4");

    const BATCH_RECORDS: usize = 20;
    const NUM_BATCHES: usize = 300;

    let svc = Arc::clone(&world.svc);
    let ctx = world.ctx.clone();
    let topic_owned = topic.to_string();
    let producer = thread::spawn(move || {
        let mut seed = 0u64;
        for _ in 0..NUM_BATCHES {
            seed += 1;
            let batch = make_batch(BATCH_RECORDS, seed, Some(0), false);
            publish_with_retry(&svc, &ctx, &topic_owned, &batch);
            thread::sleep(Duration::from_millis(1));
        }
    });

    let handle = world
        .svc
        .open_consumer(
            &world.ctx,
            "p4-consumer",
            &[topic.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer p4");

    let expected = NUM_BATCHES * BATCH_RECORDS;
    let mut latencies: Vec<Duration> = Vec::with_capacity(expected);
    let deadline = Instant::now() + Duration::from_secs(30);
    while latencies.len() < expected && Instant::now() < deadline {
        let fetched = handle.fetch(4 * 1024 * 1024, 2).expect("fetch p4");
        let now = now_ms();
        for rec in &fetched.records {
            let latency_ms = (now - rec.timestamp_ms).max(0);
            latencies.push(Duration::from_millis(latency_ms as u64));
        }
    }
    producer.join().unwrap();

    latencies.sort_unstable();
    (LatencyReport::from_sorted(&latencies), cfg.durability)
}

fn gate_p4(_c: &mut Criterion) {
    let world = bench_world("p4");
    // `os`/`fsync_batch`: the two pre-decision-B variants (PLAN never
    // specified a durability for P4), kept verbatim for comparison. `os`
    // is also the closest single-fsync-per-batch-less proxy for how
    // `standard`/`fsync_interval` should behave BETWEEN its periodic
    // background fsyncs. `standard`: owner decision B's new default path
    // — every topic that leaves `durability`/`durability_class` unset now
    // resolves to this.
    for (label, topic_suffix, durability, durability_class) in [
        ("os", "os", Some(topics::DurabilityPolicy::Os), None),
        (
            "fsync_batch",
            "fsync-batch",
            Some(topics::DurabilityPolicy::FsyncBatch),
            None,
        ),
        (
            "standard",
            "standard",
            None,
            Some(topics::DurabilityClass::Standard),
        ),
    ] {
        let (r, resolved) = run_p4(
            &world,
            &format!("p4-{topic_suffix}"),
            durability,
            durability_class,
        );
        eprintln!(
            "[P4] publish->consume, batch~{} records/1ms linger, label={label} durability={}  n={}  p50={:>7.2?} p95={:>7.2?} p99={:>7.2?} mean={:>7.2?}  (ms-resolution timestamps: +-1ms measurement noise)",
            20, resolved.to_wire_string(), r.n, r.p50, r.p95, r.p99, r.mean,
        );
    }
}

// =============================================================================
// P10 — retry/DLQ overhead at 0.1% failure rate vs P1 group commit
// (PLAN §5.2 P10: <= 5% throughput drop vs P1)
//
// Owner decision B changes what this gate's "baseline"/"degraded" durability
// actually is: a topic with no explicit `durability`/`durability_class` now
// defaults to `Standard` (`FsyncInterval{ms:50}` in this bench's default-Prod
// environment) instead of the pre-decision-B `FsyncBatch`, and every DLQ
// topic (`dlq::dlq_topic_options`) is ALWAYS `FsyncInterval{ms:50}`
// regardless of its source's class. `run_p10_pair` runs the full baseline+
// degraded comparison once per MAIN-topic class so the report can show both
// "the now-forced-standard default path" (main=standard, DLQ=standard, the
// service's actual current behavior) and "how much decision B's ALWAYS-
// interval DLQ recovers even when the main topic keeps paying the strongest
// barrier" (main=critical, DLQ=standard regardless).
// =============================================================================

fn run_p10_pair(
    world: &support::BenchWorld,
    label: &str,
    main_class: topics::DurabilityClass,
    shape: &MultiProducerShape,
) {
    // Baseline: identical shape to P1's group-commit case, no consumer/DLQ
    // at all, main topic at `main_class`.
    let topic_baseline = format!("p10-baseline-{label}");
    let cfg_baseline = world
        .svc
        .create_topic(
            &world.ctx,
            &topic_baseline,
            byte_accounting_topic_options(1, main_class),
        )
        .expect("create_topic p10-baseline");
    let (baseline_msg_s, baseline_mib_s, _r) =
        run_multi_producer(&world, &topic_baseline, 1, Some(0), shape);
    eprintln!(
        "[P10] baseline (no consumer, no DLQ), 8 producers / 1 partition, class={label} durability={}  n={}/thread  msg/s={baseline_msg_s:>10.0}  MiB/s={baseline_mib_s:>8.2}",
        cfg_baseline.durability.to_wire_string(),
        shape.per_thread_appends,
    );

    // Degraded: same producer shape, PLUS a consumer that fails ~0.1% of
    // records via `note_delivery_failure` with `max_delivery_attempts = 1`
    // (so a single failure sends straight to `__dlq.<topic>` — no retry
    // delay diluting the signal). The DLQ topic is auto-created on first
    // failure via `dlq::dlq_topic_options`, which ALWAYS assigns
    // `FsyncInterval{ms: STANDARD_FSYNC_INTERVAL_MS}` regardless of
    // `main_class` (owner decision B) — any throughput drop this shows is
    // real physical fsync/write contention on this disk between the main
    // and DLQ partitions, not a shared lock between the two topics.
    let topic_degraded = format!("p10-degraded-{label}");
    let cfg_degraded = world
        .svc
        .create_topic(
            &world.ctx,
            &topic_degraded,
            topics::TopicOptions {
                partitions: Some(1),
                durability_class: Some(main_class),
                compression: Some(topics::CompressionPolicy::None),
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .expect("create_topic p10-degraded");

    let handle = world
        .svc
        .open_consumer(
            &world.ctx,
            "p10-consumer",
            &[topic_degraded.clone()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open_consumer p10");

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dlq_count = Arc::new(AtomicU64::new(0));
    let consumer = {
        let svc = Arc::clone(&world.svc);
        let ctx = world.ctx.clone();
        let stop = Arc::clone(&stop);
        let dlq_count = Arc::clone(&dlq_count);
        let topic_degraded = topic_degraded.clone();
        thread::spawn(move || {
            let mut seq: u64 = 0;
            while !stop.load(Ordering::Acquire) {
                let fetched = handle.fetch(4 * 1024 * 1024, 5).expect("fetch p10");
                if fetched.records.is_empty() {
                    continue;
                }
                for rec in &fetched.records {
                    seq += 1;
                    // ~0.1% failure rate.
                    if seq % 1000 == 0 {
                        let _ = svc
                            .note_delivery_failure(
                                &ctx,
                                "p10-consumer",
                                &topic_degraded,
                                rec.partition,
                                rec.offset,
                                rec,
                                dlq::DlqReason::ConsumerError,
                                "bus_path bench: synthetic 0.1% failure injection",
                            )
                            .expect("note_delivery_failure");
                        dlq_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    let (degraded_msg_s, degraded_mib_s, _r) =
        run_multi_producer(&world, &topic_degraded, 1, Some(0), shape);
    // Give the consumer a little more time to drain what is still queued so
    // the reported DLQ count reflects (most of) the run, then stop it.
    thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::Release);
    consumer.join().unwrap();

    let drop_msg_pct = (1.0 - degraded_msg_s / baseline_msg_s) * 100.0;
    let drop_mib_pct = (1.0 - degraded_mib_s / baseline_mib_s) * 100.0;
    eprintln!(
        "[P10] degraded (0.1% -> DLQ, max_delivery_attempts=1), 8 producers / 1 partition, class={label} main_durability={} dlq_durability=fsync_interval:{}  n={}/thread  msg/s={degraded_msg_s:>10.0}  MiB/s={degraded_mib_s:>8.2}  dlq_sent={}",
        cfg_degraded.durability.to_wire_string(),
        topics::STANDARD_FSYNC_INTERVAL_MS,
        shape.per_thread_appends,
        dlq_count.load(Ordering::Relaxed),
    );
    eprintln!(
        "[P10] throughput drop vs class={label} baseline: msg/s {drop_msg_pct:+.1}%  MiB/s {drop_mib_pct:+.1}%  (target: <= 5% drop)"
    );
}

fn gate_p10(_c: &mut Criterion) {
    let world = bench_world("p10");

    // Standard: main topic AND its DLQ both resolve to FsyncInterval{ms:50}
    // — the DLQ always does; the main topic does here because Standard is
    // now the platform DEFAULT (owner decision B). This is the headline
    // number: "the service now forces" this shape by default.
    run_p10_pair(
        &world,
        "standard",
        topics::DurabilityClass::Standard,
        &MultiProducerShape {
            producers: 8,
            per_thread_appends: 100,
        },
    );

    // Critical: main topic keeps paying FsyncBatchFull; its DLQ is STILL
    // forced to FsyncInterval{ms:50} regardless (decision B never lets a
    // DLQ inherit its source's class). Reduced sample count: this variant
    // exists to show the DLQ-always-interval recovery delta, not to
    // re-establish the critical-class headline from scratch.
    run_p10_pair(
        &world,
        "critical",
        topics::DurabilityClass::Critical,
        &MultiProducerShape {
            producers: 8,
            per_thread_appends: 50,
        },
    );
}

// =============================================================================
// P13 — retention sweep wall time vs segment count (PLAN §5.2 P13: <= 2 s,
// O(segments) not O(bytes); M0-WYNIKI notes 256 MiB is the smallest segment
// size `BusService::partition_handle` uses — `RollPolicy` is not exposed
// through `TopicOptions`, so 256 MiB is also the ONLY size reachable through
// the public service API, matching this gate's own brief.)
// =============================================================================

/// Publishes 1 MiB-ish batches (measuring the FIRST one's actual on-disk
/// delta via `partition_stats` to size the rest of the loop precisely, since
/// `BusService`'s broker-stamped headers make the true on-wire size larger
/// than the raw `RECORDS_PER_BATCH * RECORD_SIZE` payload figure) until
/// `topic`'s partition 0 holds at least `target_bytes`.
fn fill_partition_to(world: &support::BenchWorld, topic: &str, target_bytes: u64) -> u64 {
    let mut seed = 0u64;
    let before = world
        .svc
        .partition_stats(&world.ctx, topic, 0)
        .unwrap()
        .size_bytes;
    seed += 1;
    let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
    publish_with_retry(&world.svc, &world.ctx, topic, &batch);
    let after_one = world
        .svc
        .partition_stats(&world.ctx, topic, 0)
        .unwrap()
        .size_bytes;
    let bytes_per_batch = (after_one - before).max(1);
    let remaining = target_bytes.saturating_sub(after_one - before);
    let more_batches = remaining.div_ceil(bytes_per_batch);
    for _ in 0..more_batches {
        seed += 1;
        let batch = make_batch(RECORDS_PER_BATCH, seed, Some(0), false);
        publish_with_retry(&world.svc, &world.ctx, topic, &batch);
    }
    world
        .svc
        .partition_stats(&world.ctx, topic, 0)
        .unwrap()
        .size_bytes
}

fn run_p13(world: &support::BenchWorld, topic: &str, target_bytes: u64) {
    // `Os` durability for the WRITE phase only: P13 is about retention
    // sweep cost, not append durability, and writing multiple GiB under
    // `fsync_batch` would spend most of this gate's wall time re-measuring
    // P1's own fsync ceiling instead of retention. `run_retention_sweep`'s
    // own cost (what this gate actually measures) does not depend on how
    // the data was written.
    world
        .svc
        .create_topic(
            &world.ctx,
            topic,
            topics::TopicOptions {
                partitions: Some(1),
                durability: Some(topics::DurabilityPolicy::Os),
                compression: Some(topics::CompressionPolicy::None),
                // `retention_ms` cannot go below `MIN_RETENTION_MS` (1 hour,
                // PLAN §7.1 range table) — age alone can never expire
                // anything written moments ago in the same process, so this
                // gate expires purely on the BYTES side: setting
                // `retention_bytes_per_partition` to its allowed MINIMUM (64
                // MiB) makes every one of our 256-MiB sealed segments
                // "over budget" the moment it is written, which
                // `sweep_partition` (`bus/retention.rs`) will delete
                // regardless of age.
                retention_ms: Some(topics::MIN_RETENTION_MS),
                retention_bytes_per_partition: Some(topics::MIN_RETENTION_BYTES_PER_PARTITION),
                ..Default::default()
            },
        )
        .expect("create_topic p13");

    let write_start = Instant::now();
    let final_size = fill_partition_to(world, topic, target_bytes);
    let write_elapsed = write_start.elapsed();
    let stats = world.svc.partition_stats(&world.ctx, topic, 0).unwrap();
    eprintln!(
        "[P13] wrote {:.1} MiB ({} segments incl. active) in {:.2?}, durability=os (write phase only)",
        mib(final_size),
        stats.segments,
        write_elapsed,
    );

    let sweep_start = Instant::now();
    let report = world.svc.run_retention_sweep();
    let sweep_elapsed = sweep_start.elapsed();
    let per_segment = if report.deleted_segments > 0 {
        sweep_elapsed / report.deleted_segments
    } else {
        Duration::ZERO
    };
    eprintln!(
        "[P13] run_retention_sweep(): deleted_segments={} deleted_bytes={:.1} MiB  wall_time={:.2?}  per_segment={:.2?}  (target: <= 2 s total; per-segment cost, not per-byte, is the O(segments) claim)",
        report.deleted_segments,
        mib(report.deleted_bytes),
        sweep_elapsed,
        per_segment,
    );
}

fn gate_p13(_c: &mut Criterion) {
    let world = bench_world("p13");
    // Two data points at the SAME fixed 256 MiB segment size (not
    // independently variable through the public API — see this gate's
    // module doc) so the comparison isolates segment COUNT, not segment
    // size: ~2 sealed segments, then ~4x that (~8 sealed segments). Neither
    // reaches PLAN's literal 10 GiB (40 segments at 256 MiB) — see
    // M1-WYNIKI.md for the extrapolation and the architectural argument
    // (`bus/retention.rs`'s own module doc: "delete WHOLE closed segments,
    // unlink = O(1) per segment, independent of record count") for why this
    // is sound in place of an actual 10 GiB run.
    run_p13(&world, "p13-512mib", 512 * 1024 * 1024);
    run_p13(&world, "p13-2gib", 2 * 1024 * 1024 * 1024);
}

criterion_group!(benches, gate_p1, gate_p5, gate_p4, gate_p10, gate_p13);
criterion_main!(benches);
