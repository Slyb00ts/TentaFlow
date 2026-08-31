// ===== File: benches/meta_perf.rs — fjall offset-commit / dedup (PLAN §5.4 item 4) =====
//
// PLAN §2.1/§10 R2: data never lives in fjall, only metadata (committed
// offsets, producer-idempotency dedup).
//
// The 300k ops/s target applies to **layer 2** (optional per-record dedup
// by `idempotency_key`, PLAN §3.1), not layer 1 (per-batch producer
// idempotency, which is ~hundreds of ops/s at realistic batch sizes) — a
// single combined `target=300000 ops/s` number would silently measure the
// wrong layer.
//
// This file now measures the two layers separately:
//   - `bench_layer1_producer_idempotency_commit`: **informational only, no
//     gate.** One dedup-seq lookup + one offset upsert per *batch*. At
//     PLAN's batch cadence (~hundreds/s at 1 MiB batches) this will always
//     clear any plausible target by orders of magnitude — reported for
//     visibility, not compared against 300k.
//   - `bench_layer2_dedup_lookup_beyond_memtable` / `bench_layer2_combined_ack`:
//     **the actual 300k ops/s gate**, per *record*. The preloaded dataset
//     is sized with a deliberately small `max_memtable_size` so lookups hit
//     multiple flushed SSTs (and their bloom filters), not just an
//     in-memory map — the original 100k-key/2.5MB dataset never left the
//     memtable, so "fjall wytrzymuje tempo" was unproven. Every commit
//     declares an explicit `PersistMode` (`Buffer` and `SyncData`, reported
//     separately) via `WriteBatch::durability` — the original called
//     `db.persist(PersistMode::Buffer)` once, outside and after the timed
//     loop, which persisted nothing the measurement depended on.

use std::time::Instant;

use fjall::{Database, KeyspaceCreateOptions, PersistMode};

mod support;
use support::{bench_dir, LatencyReport};

const WARMUP_ITERS: usize = 50;
const MEASURE_ITERS: usize = 2_000;

fn open_db(label: &str) -> Database {
    let dir = bench_dir("meta-perf", label);
    Database::builder(&dir).open().expect("open fjall database")
}

/// 24-byte dedup key stand-in for `blake3-128(idempotency_key)` (PLAN
/// §3.1) — this bench measures LSM key traffic shape/rate, not the actual
/// hash function, so any fixed-size, well-distributed key works.
fn dedup_key(i: u64) -> [u8; 24] {
    let mut k = [0u8; 24];
    k[0..8].copy_from_slice(&i.to_le_bytes());
    k[8..16].copy_from_slice(&i.wrapping_mul(0x9E3779B97F4A7C15).to_le_bytes());
    k
}

/// `offsets` keyspace key: `(group_id, topic, partition)` collapsed to a
/// fixed shape for the bench (PLAN §3.2).
fn offset_key(group: u32, partition: u32) -> [u8; 8] {
    let mut k = [0u8; 8];
    k[0..4].copy_from_slice(&group.to_le_bytes());
    k[4..8].copy_from_slice(&partition.to_le_bytes());
    k
}

fn offset_value(committed_offset: u64, ts_ms: i64, attempts: u32) -> [u8; 20] {
    let mut v = [0u8; 20];
    v[0..8].copy_from_slice(&committed_offset.to_le_bytes());
    v[8..16].copy_from_slice(&ts_ms.to_le_bytes());
    v[16..20].copy_from_slice(&attempts.to_le_bytes());
    v
}

fn print_report(prefix: &str, r: &LatencyReport, target: Option<f64>) {
    let ops_s = r.ops_per_sec();
    match target {
        Some(t) => eprintln!(
            "[meta_perf] {prefix:<60} n={:<6} ops/s={ops_s:>10.0} mean={:>8.2?} p50={:>8.2?} p95={:>8.2?} p99={:>8.2?} target={t:.0} pass={}",
            r.n, r.mean, r.p50, r.p95, r.p99, ops_s >= t,
        ),
        None => eprintln!(
            "[meta_perf] {prefix:<60} n={:<6} ops/s={ops_s:>10.0} mean={:>8.2?} p50={:>8.2?} p95={:>8.2?} p99={:>8.2?} (informational, no gate)",
            r.n, r.mean, r.p50, r.p95, r.p99,
        ),
    }
}

/// Layer 1 — producer idempotency, per *batch* (PLAN §3.1): one dedup-seq
/// lookup + one offset upsert per batch, at whatever batch cadence the
/// engine actually sustains (hundreds/s at 1 MiB batches, PLAN §5.2 P1).
/// No 300k target applies here — that number belongs to layer 2 (decision
/// #4). Reported purely so the two layers are never conflated again.
fn bench_layer1_producer_idempotency_commit(_c: &mut criterion::Criterion) {
    let db = open_db("layer1-producer-seq");
    let offsets = db
        .keyspace("offsets", KeyspaceCreateOptions::default)
        .expect("open offsets keyspace");
    let producer_seq = db
        .keyspace("producer_seq", KeyspaceCreateOptions::default)
        .expect("open producer_seq keyspace");

    let mut i = 0u64;
    let latencies = support::measure_latencies(WARMUP_ITERS, MEASURE_ITERS, || {
        i += 1;
        let dkey = dedup_key(i); // stands in for (producer_id, epoch)
        let okey = offset_key((i % 64) as u32, (i % 8) as u32);
        let start = Instant::now();
        let mut batch = db.batch().durability(Some(PersistMode::SyncData));
        batch.insert(&producer_seq, dkey.as_slice(), i.to_le_bytes().as_slice());
        batch.insert(
            &offsets,
            okey.as_slice(),
            offset_value(i, i as i64, 0).as_slice(),
        );
        batch.commit().expect("commit ack batch");
        start.elapsed()
    });
    let r = LatencyReport::from_sorted(&latencies);
    print_report("layer1 per-batch producer idempotency (SyncData)", &r, None);
}

/// Layer 2 raw lookup cost — the dataset is preloaded with a small
/// `max_memtable_size` so most of `PRELOAD` keys are flushed to SSTs
/// before the timed loop starts, forcing lookups through bloom
/// filters/block reads rather than an in-memory map.
fn bench_layer2_dedup_lookup_beyond_memtable(_c: &mut criterion::Criterion) {
    const PRELOAD: u64 = 400_000;
    let db = open_db("layer2-dedup-lookup");
    let dedup = db
        .keyspace(
            "dedup",
            // ~24 B key + ~1 B value; a 512 KiB memtable cap forces a flush
            // roughly every ~20k inserts, so PRELOAD keys are spread across
            // dozens of SSTs by the time preload finishes — unlike the
            // original 100k-key/2.5MB dataset, which fit in one default
            // (64 MiB) memtable and never touched disk for reads at all.
            || KeyspaceCreateOptions::default().max_memtable_size(512 * 1024),
        )
        .expect("open dedup keyspace");

    for i in 0..PRELOAD {
        dedup
            .insert(dedup_key(i).as_slice(), b"1".as_slice())
            .expect("preload insert");
    }
    db.persist(PersistMode::SyncData).expect("persist preload");

    let mut i = 0u64;
    let latencies = support::measure_latencies(WARMUP_ITERS, MEASURE_ITERS, || {
        i += 1;
        // 90% hits (recently-seen key range), 10% misses (new keys) — a
        // rough stand-in for a real producer's replay/duplicate rate.
        let key = if i.is_multiple_of(10) {
            dedup_key(PRELOAD + i)
        } else {
            dedup_key(i % PRELOAD)
        };
        let start = Instant::now();
        let found = dedup.contains_key(key.as_slice()).expect("contains_key");
        let elapsed = start.elapsed();
        std::hint::black_box(found);
        elapsed
    });
    let r = LatencyReport::from_sorted(&latencies);
    print_report(
        &format!("layer2 dedup lookup only, beyond memtable ({PRELOAD} keys preloaded)"),
        &r,
        None,
    );
}

/// **The gate**: layer 2's actual unit of work — one dedup lookup + one
/// offset upsert *per record*, committed atomically (PLAN §3.2:
/// "atomowość ACK-a i deduplikacji") — against the same beyond-memtable
/// dataset as above, with an explicit `PersistMode` declared on every
/// commit — measuring buffering into the WAL without ever declaring a
/// `PersistMode` would silently skip the persisted-commit cost. Both
/// `Buffer` (an upper bound, no durability barrier) and `SyncData` (an
/// actual durability barrier per ack) are measured and reported side by
/// side, so a report using this number can say which one it refers to
/// instead of leaving it implicit.
fn bench_layer2_combined_ack(_c: &mut criterion::Criterion) {
    const PRELOAD: u64 = 400_000;
    const TARGET_OPS_S: f64 = 300_000.0;

    for (mode_name, mode) in [
        ("Buffer", PersistMode::Buffer),
        ("SyncData", PersistMode::SyncData),
    ] {
        let db = open_db(&format!("layer2-combined-ack-{mode_name}"));
        let offsets = db
            .keyspace("offsets", KeyspaceCreateOptions::default)
            .expect("open offsets keyspace");
        let dedup = db
            .keyspace("dedup", || {
                KeyspaceCreateOptions::default().max_memtable_size(512 * 1024)
            })
            .expect("open dedup keyspace");

        for i in 0..PRELOAD {
            dedup
                .insert(dedup_key(i).as_slice(), b"1".as_slice())
                .expect("preload insert");
        }
        db.persist(PersistMode::SyncData).expect("persist preload");

        let mut i = PRELOAD; // continue past the preloaded key range
        let latencies = support::measure_latencies(WARMUP_ITERS, MEASURE_ITERS, || {
            i += 1;
            let dkey = dedup_key(i);
            let okey = offset_key((i % 64) as u32, (i % 8) as u32);

            let start = Instant::now();
            let _already_seen = dedup.contains_key(dkey.as_slice()).expect("contains_key");
            let mut batch = db.batch().durability(Some(mode));
            batch.insert(&dedup, dkey.as_slice(), b"1".as_slice());
            batch.insert(
                &offsets,
                okey.as_slice(),
                offset_value(i, i as i64, 0).as_slice(),
            );
            batch.commit().expect("commit ack batch");
            start.elapsed()
        });
        let r = LatencyReport::from_sorted(&latencies);
        print_report(
            &format!("layer2 combined ack (dedup lookup + offset upsert), persist={mode_name}"),
            &r,
            Some(TARGET_OPS_S),
        );
    }
}

fn benches(c: &mut criterion::Criterion) {
    bench_layer1_producer_idempotency_commit(c);
    bench_layer2_dedup_lookup_beyond_memtable(c);
    bench_layer2_combined_ack(c);
}

criterion::criterion_group! {
    name = meta_perf;
    config = criterion::Criterion::default().sample_size(10);
    targets = benches
}
criterion::criterion_main!(meta_perf);
