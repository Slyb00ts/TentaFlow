// ===== File: benches/meta_perf.rs — fjall offset-commit / layer-1 dedup (PLAN §5.4 item 4) =====
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
// This file measures ONLY layer 1 now:
//   - `bench_layer1_producer_idempotency_commit`: **informational only, no
//     gate.** One dedup-seq lookup + one offset upsert per *batch*. At
//     PLAN's batch cadence (~hundreds/s at 1 MiB batches) this will always
//     clear any plausible target by orders of magnitude — reported for
//     visibility, not compared against 300k. Layer 1 genuinely stays on
//     fjall (`bus/producer.rs`) — that workload is one lookup per batch, not
//     per record, so it comfortably fits fjall's throughput.
//
// Layer 2's own two benches (`bench_layer2_dedup_lookup_beyond_memtable`,
// `bench_layer2_combined_ack`) were REMOVED (TentaBus 1C,
// SUM/tentabus/OTWARTE-POZYCJE.md `dedup-300k-gate-unbenchmarked`, decided
// 2026-09-22): fjall was abandoned for per-record dedup — an LSM-backed
// store measured well under the 300k target for this workload, see
// `bus/dedup.rs`'s own module doc — in favor of `bus::dedup::MmapDedupStore`,
// a dedicated mmapped fixed-size sharded key store. That gate now lives in
// `tentaflow-core/benches/bus_dedup_perf.rs`, against the store that
// actually ships. `tentaflow-bus` cannot depend on `tentaflow-core` (wrong
// dependency direction, would pull in GStreamer etc. for a bench), so the
// 300k gate could not simply move a function here — it lives with the store
// it measures instead.

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

fn print_report(prefix: &str, r: &LatencyReport) {
    let ops_s = r.ops_per_sec();
    eprintln!(
        "[meta_perf] {prefix:<60} n={:<6} ops/s={ops_s:>10.0} mean={:>8.2?} p50={:>8.2?} p95={:>8.2?} p99={:>8.2?} (informational, no gate)",
        r.n, r.mean, r.p50, r.p95, r.p99,
    );
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
    print_report("layer1 per-batch producer idempotency (SyncData)", &r);
}

fn benches(c: &mut criterion::Criterion) {
    bench_layer1_producer_idempotency_commit(c);
}

criterion::criterion_group! {
    name = meta_perf;
    config = criterion::Criterion::default().sample_size(10);
    targets = benches
}
criterion::criterion_main!(meta_perf);
