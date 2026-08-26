// ===== File: benches/log_perf.rs — append throughput matrix (PLAN §5.4 item 1) =====
//
// Matrix: {record 256B/1KiB/64KiB/1MiB} x {batch 64KiB/1MiB} x
// {durability os/fsync_batch/fsync_batch_full/fsync_interval(100ms)} ->
// msg/s, MiB/s, p50/p95/p99 append latency. Only `Partition::append_batch`
// — channel send, group-commit draining, offset patch, pwrite, index
// append, fsync-per-policy — is timed; `build_batch`'s own cost (including
// lz4 when it fires) is measured completely separately, see
// `bench_build_batch_cost` (review P2-12/bullet f: it used to be built
// before the clock started and never reported at all).
//
// Review P3-10: percentile/msg-per-second numbers come from
// `support::measure_latencies`, which runs an explicit warm-up loop
// (discarded) before the timed loop, instead of accumulating every call
// Criterion's own `iter_custom` warm-up phase makes into the same `Vec` the
// final report reads from. Criterion's own `bench_with_input` call still
// runs alongside it (for the HTML report), driven by a throwaway swallow
// closure that never touches that Vec.
//
// Run only once the machine is otherwise idle (criterion numbers under
// contention are meaningless per repo policy) — except `bench_multi_producer`,
// which is contention *by design* (review decision #5 / bullet d): it is
// the one number in this file group commit is supposed to move.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use tentaflow_bus::{BatchBuilder, Codec, Durability, Partition, RecordInput, RollPolicy};

mod support;
use support::{bench_dir, pseudo_random_bytes, LatencyReport};

const WARMUP_ITERS: usize = 20;
const MEASURE_ITERS: usize = 200;

/// Builds a batch of `n_records`, each with its own freshly-generated
/// `record_size`-byte payload (seeded off both `seed` and the record
/// index). A single fixed payload reused across every record in the batch
/// — what an earlier version of this function did — makes each batch
/// accidentally highly compressible (the same bytes repeated `n_records`
/// times), which silently shrinks the on-wire size reported for MiB/s and
/// makes the P1/P2 "% of device ceiling" comparison unfair: the ceiling
/// probe in `device_ceiling.rs` always writes literal incompressible
/// bytes, so an artificially-compressible engine payload understates real
/// bytes-on-disk for the same `batch_target`.
fn build_batch(n_records: usize, record_size: usize, seed: u64, codec: Option<Codec>) -> Bytes {
    let mut b = BatchBuilder::with_capacity(0, 1, n_records * (record_size + 32));
    if let Some(codec) = codec {
        b = b.with_codec(codec);
    }
    for i in 0..n_records {
        let payload = pseudo_random_bytes(
            record_size,
            seed ^ (i as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93),
        );
        b.push(RecordInput::new(Bytes::from(payload), i as i64))
            .expect("record fields within wire limits");
    }
    b.build().expect("non-empty batch")
}

fn report(label: &str, r: &LatencyReport, on_wire_bytes_per_batch: u64, records_per_batch: u64) {
    let batches_s = r.ops_per_sec();
    let msg_s = batches_s * records_per_batch as f64;
    // Binary MiB/s (bytes / 2^20) of the actual on-wire bytes written per
    // batch (header + stored, possibly-compressed body) — matching
    // `device_ceiling.rs`'s definition of "bytes written" exactly, so the
    // "% of device ceiling" ratio compares like with like (review P3-11:
    // the previous version used *payload* bytes here against *written*
    // bytes in device_ceiling, silently including header/compression
    // overhead in the engine number only).
    let mib_s = batches_s * on_wire_bytes_per_batch as f64 / (1024.0 * 1024.0);
    eprintln!(
        "[log_perf] {label:<80} n={:<6} msg/s={msg_s:>10.0} MiB/s={mib_s:>9.2} p50={:>8.2?} p95={:>8.2?} p99={:>8.2?}",
        r.n, r.p50, r.p95, r.p99,
    );
}

fn bench_append_matrix(c: &mut Criterion) {
    let record_sizes: [usize; 4] = [256, 1024, 64 * 1024, 1024 * 1024];
    let batch_targets: [usize; 2] = [64 * 1024, 1024 * 1024];
    let mut durabilities: Vec<(&str, Durability)> = vec![
        ("os", Durability::Os),
        ("fsync_batch", Durability::FsyncBatch),
        (
            "fsync_interval_100ms",
            Durability::FsyncInterval(Duration::from_millis(100)),
        ),
    ];
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        durabilities.push(("fsync_batch_full", Durability::FsyncBatchFull));
    }

    let mut group = c.benchmark_group("append");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.warm_up_time(Duration::from_millis(500));

    for record_size in record_sizes {
        for batch_target in batch_targets {
            let records_per_batch = (batch_target / record_size).max(1);

            for &(durability_name, durability) in &durabilities {
                let label = format!(
                    "record={record_size}B/batch_target={batch_target}B/records_per_batch={records_per_batch}/durability={durability_name}"
                );

                let dir = bench_dir("log-perf", &label.replace(['=', '/'], "_"));
                let partition = Partition::open(&dir, RollPolicy::default(), durability, 64)
                    .expect("open partition");

                // The exact on-wire size of one built batch — used both to
                // drive our custom measurement loop and to report bytes
                // consistently with device_ceiling.rs (review P3-11).
                let probe = build_batch(records_per_batch, record_size, 1, None);
                let on_wire_bytes_per_batch = probe.len() as u64;

                let mut seed = 100u64;
                let latencies = support::measure_latencies(WARMUP_ITERS, MEASURE_ITERS, || {
                    seed = seed.wrapping_add(1);
                    let batch = build_batch(records_per_batch, record_size, seed, None);
                    let start = Instant::now();
                    partition
                        .append_batch(std::hint::black_box(batch))
                        .expect("append_batch");
                    start.elapsed()
                });
                let r = LatencyReport::from_sorted(&latencies);
                report(
                    &label,
                    &r,
                    on_wire_bytes_per_batch,
                    records_per_batch as u64,
                );

                // Criterion's own HTML-report timing, independent of the
                // custom percentiles above — its internal warm-up/sample
                // mixing is fine here, this is not what P3-10 objects to.
                group.throughput(Throughput::Bytes(on_wire_bytes_per_batch));
                let mut criterion_seed = 900_000u64;
                group.bench_with_input(BenchmarkId::from_parameter(&label), &label, |b, _| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::ZERO;
                        for _ in 0..iters {
                            criterion_seed = criterion_seed.wrapping_add(1);
                            let batch =
                                build_batch(records_per_batch, record_size, criterion_seed, None);
                            let start = Instant::now();
                            partition
                                .append_batch(std::hint::black_box(batch))
                                .expect("append_batch");
                            total += start.elapsed();
                        }
                        total
                    });
                });

                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    group.finish();
}

/// Review decision #5 / bullet (d), extended per coordinator follow-up
/// (26.08.2026): a single-threaded harness cannot show group commit's
/// effect at all — one producer never has anything to share a group/fsync
/// with. This spins up `N` producer threads hammering the *same* partition
/// concurrently and reports aggregate msg/s/MiB/s *and* per-append
/// p50/p99 (measured per producer thread, merged), for both the original
/// ~64 KiB batch size and — the point of the follow-up — PLAN §5.2 P1's
/// actual 1 MiB batch size, at 1/2/4/8 concurrent producers. This is what
/// resolves whether P1's absolute 300k msg/s target is reachable under
/// realistic concurrency even though the single-producer number
/// (`bench_append_matrix`) falls short of it.
///
/// No Criterion `bench_with_input`/`iter_custom` registration here (unlike
/// `bench_append_matrix`) — this function's own explicit measurement *is*
/// the reported number, and registering the same multi-thread workload a
/// second time with Criterion added several minutes of wall time for a
/// number this report never used.
fn bench_multi_producer(_c: &mut Criterion) {
    const PRODUCERS: [usize; 4] = [1, 2, 4, 8];
    const RECORD_SIZE: usize = 1024;

    struct BatchShape {
        label: &'static str,
        records_per_batch: usize,
        per_thread_appends: usize,
    }
    // 1 MiB is the actual PLAN §5.2 P1 batch size; 64 KiB is kept from the
    // original bench for continuity/comparison. Fewer per-thread appends
    // at 1 MiB keeps 8-producer runs from taking unreasonably long while
    // still giving several hundred fsync-covering groups to percentile
    // over.
    let shapes = [
        BatchShape {
            label: "64KiB",
            records_per_batch: 64,
            per_thread_appends: 300,
        },
        BatchShape {
            label: "1MiB",
            records_per_batch: 1024,
            per_thread_appends: 120,
        },
    ];

    for shape in &shapes {
        for &producers in &PRODUCERS {
            let label = format!(
                "batch={}/producers={producers}/durability=fsync_batch",
                shape.label
            );
            let dir = bench_dir("log-perf-multi", &label.replace(['=', '/'], "_"));
            let partition =
                Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 256)
                    .expect("open partition");
            let partition = Arc::new(partition);

            let start = Instant::now();
            let handles: Vec<_> = (0..producers)
                .map(|p| {
                    let partition = Arc::clone(&partition);
                    let records_per_batch = shape.records_per_batch;
                    let per_thread_appends = shape.per_thread_appends;
                    thread::spawn(move || {
                        let mut bytes = 0u64;
                        let mut latencies = Vec::with_capacity(per_thread_appends);
                        for i in 0..per_thread_appends {
                            let seed = (p * 1_000_000 + i) as u64;
                            let mut batch = build_batch(records_per_batch, RECORD_SIZE, seed, None);
                            loop {
                                bytes += batch.len() as u64;
                                let op_start = Instant::now();
                                match partition.append_batch(std::hint::black_box(batch)) {
                                    Ok(_) => {
                                        latencies.push(op_start.elapsed());
                                        break;
                                    }
                                    Err(tentaflow_bus::BusError::Throttled {
                                        batch: returned,
                                        ..
                                    }) => {
                                        bytes -= returned.len() as u64;
                                        batch = returned;
                                        continue;
                                    }
                                    Err(e) => panic!("unexpected error: {e}"),
                                }
                            }
                        }
                        (bytes, latencies)
                    })
                })
                .collect();

            let mut total_bytes = 0u64;
            let mut all_latencies: Vec<Duration> = Vec::new();
            for h in handles {
                let (bytes, latencies) = h.join().unwrap();
                total_bytes += bytes;
                all_latencies.extend(latencies);
            }
            let elapsed = start.elapsed();
            let total_appends = producers * shape.per_thread_appends;
            let msg_s =
                total_appends as f64 * shape.records_per_batch as f64 / elapsed.as_secs_f64();
            let mib_s = total_bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
            all_latencies.sort_unstable();
            let p50 = support::percentile(&all_latencies, 0.50);
            let p99 = support::percentile(&all_latencies, 0.99);
            eprintln!(
                "[log_perf] multi_producer {label:<45} total_appends={total_appends:<6} elapsed={elapsed:>8.2?} msg/s={msg_s:>10.0} MiB/s={mib_s:>9.2} p50={p50:>8.2?} p99={p99:>8.2?}"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Review P2-12/bullet (f): `BatchBuilder::build()`'s own cost — including
/// lz4 `safe-encode` when it fires — reported on its own, separate from
/// append latency. `log_perf.rs` previously built the batch *before*
/// starting the clock on `append_batch`, which hid this cost from every
/// number in the M0 report even though, at ~1 MiB, it is comparable to a
/// single fsync (review "Ocena bramki" item 6).
fn bench_build_batch_cost(c: &mut Criterion) {
    const RECORD_SIZE: usize = 1024;
    const RECORDS_PER_BATCH: usize = 1024; // ~1 MiB batch, compressible payload below

    let mut group = c.benchmark_group("build_batch_cost");
    group.sample_size(30);

    for (label, codec, seed_kind) in [
        ("codec=none/payload=random", Some(Codec::None), 0u64),
        ("codec=lz4/payload=random", Some(Codec::Lz4), 0u64),
        // A constant-byte payload is maximally compressible — the opposite
        // extreme from the random payload above, showing the cost range
        // lz4 can land in depending on data shape.
        ("codec=lz4/payload=repetitive", Some(Codec::Lz4), u64::MAX),
    ] {
        let mut seed = seed_kind;
        // Each record gets its *own* payload (fresh random bytes, or the
        // same repeated byte for the intentionally-repetitive variant) —
        // reusing one buffer across every record in the batch would make
        // even the "random" case accidentally compressible (the same
        // review P3-11/P2-12 byte-accounting-fidelity issue found in
        // `build_batch` above).
        let make_payload = |seed: &mut u64| -> Vec<u8> {
            if seed_kind == u64::MAX {
                vec![0x42u8; RECORD_SIZE]
            } else {
                *seed = seed.wrapping_add(1);
                pseudo_random_bytes(RECORD_SIZE, *seed)
            }
        };

        let mut latencies = Vec::with_capacity(MEASURE_ITERS);
        for i in 0..(WARMUP_ITERS + MEASURE_ITERS) {
            let mut b = BatchBuilder::with_capacity(0, 1, RECORDS_PER_BATCH * (RECORD_SIZE + 32));
            if let Some(codec) = codec {
                b = b.with_codec(codec);
            }
            for r in 0..RECORDS_PER_BATCH {
                let payload = make_payload(&mut seed);
                b.push(RecordInput::new(Bytes::from(payload), r as i64))
                    .unwrap();
            }
            let start = Instant::now();
            let built = std::hint::black_box(b.build().unwrap());
            let elapsed = start.elapsed();
            if i >= WARMUP_ITERS {
                latencies.push(elapsed);
            }
            std::hint::black_box(built);
        }
        latencies.sort_unstable();
        let r = LatencyReport::from_sorted(&latencies);
        eprintln!(
            "[log_perf] build_batch_cost {label:<35} n={:<6} mean={:>8.2?} p50={:>8.2?} p95={:>8.2?} p99={:>8.2?}",
            r.n, r.mean, r.p50, r.p95, r.p99,
        );

        let mut criterion_seed = seed_kind;
        group.bench_function(label, |b| {
            b.iter(|| {
                let mut bb =
                    BatchBuilder::with_capacity(0, 1, RECORDS_PER_BATCH * (RECORD_SIZE + 32));
                if let Some(codec) = codec {
                    bb = bb.with_codec(codec);
                }
                for r in 0..RECORDS_PER_BATCH {
                    let payload = make_payload(&mut criterion_seed);
                    bb.push(RecordInput::new(Bytes::from(payload), r as i64))
                        .unwrap();
                }
                std::hint::black_box(bb.build().unwrap())
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_append_matrix,
    bench_multi_producer,
    bench_build_batch_cost
);
criterion_main!(benches);
