// ===== File: benches/bus_dedup_perf.rs — TentaBus layer-2 dedup gate =====
// (SUM/tentabus/OTWARTE-POZYCJE.md `dedup-300k-gate-unbenchmarked`, PLAN.md
// §5.4 item 4: >= 300 000 ops/s for the per-record idempotency-key dedup
// store — PLAN §3.1 layer 2, one lookup+insert per RECORD, as opposed to
// layer 1's one lookup+insert per BATCH via `(producer_id, epoch, seq)`.)
//
// `tentaflow-bus/benches/meta_perf.rs` used to carry this gate against
// `fjall::Database`, the store layer 2 was originally planned on. That path
// was abandoned for per-record dedup (see `bus/dedup.rs`'s module doc: an
// LSM-backed store measured well under the 300k target for this workload)
// in favor of `MmapDedupStore` — a dedicated mmapped, fixed-size, sharded
// open-addressing key store. This file is the gate against THAT store, the
// one `bus::dedup` actually ships. `meta_perf.rs` keeps only
// `bench_layer1_producer_idempotency_commit`, whose target (fjall) is still
// correct for layer 1.
//
// Realism: this bench opens the store with `DedupConfig::default()` — the
// same config `BusInitConfig`'s defaults would hand a real topic (24h TTL,
// 10 000 msg/s expected rate, 1024 shards, probe_limit 8). At those
// defaults `derive_capacity` hits `MAX_DERIVED_CAPACITY` (16 Mi slots), so
// this store is backed by a real ~512 MiB mmapped file — not a toy table
// that would fit entirely in one page and let the measurement dodge real
// mmap page-fault/TLB costs. The warm-up phase below inserts a working set
// spread across the whole table before measurement starts, so steady-state
// lookups/inserts touch pages across the full file, not just the first few
// KiB written by the OS on `open`.
//
// `harness = false` for the same reason as `bus_path.rs`/`bus_replication.rs`
// (see their module docs): an explicit warm-up/measure loop with a printed
// PASS/FAIL line, not Criterion's statistical sampler.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use criterion::{criterion_group, criterion_main, Criterion};

use tentaflow_core::bus::dedup::{DedupConfig, DedupOutcome, MmapDedupStore};

/// PLAN §5.4 item 4 / OTWARTE-POZYCJE.md `dedup-300k-gate-unbenchmarked`.
const TARGET_OPS_S: f64 = 300_000.0;

/// Keys inserted during warm-up before measurement starts, spread across the
/// whole (derived, ~16 Mi slot) table so the measured phase is not just
/// touching a handful of hot pages near the start of the mmap. Combined
/// with the measured phase's own inserts this keeps total load comfortably
/// under ~20% of the table's ~16.7M slots — high enough to force real
/// cross-page mmap access, low enough that a `probe_limit`-deep run finding
/// no empty slot (an eviction) stays rare, so it does not distort the
/// ops/s measurement.
const WARMUP_KEYS: usize = 500_000;

/// Per-thread key count during the measured `check_and_insert` phase.
const KEYS_PER_THREAD: usize = 300_000;

fn open_default_store(dir: &std::path::Path, tag: &str) -> MmapDedupStore {
    let path = dir.join(format!("dedup-{tag}.bin"));
    MmapDedupStore::open(&path, DedupConfig::default())
        .expect("open MmapDedupStore with production-default sizing")
}

/// Distinct, thread/space-partitioned key so warm-up and measurement never
/// collide with each other's keys (each call in this file uses a disjoint
/// `space` tag).
fn make_key(space: u8, thread: u64, i: u64) -> [u8; 17] {
    let mut key = [0u8; 17];
    key[0] = space;
    key[1..9].copy_from_slice(&thread.to_le_bytes());
    key[9..17].copy_from_slice(&i.to_le_bytes());
    key
}

fn run_warmup(store: &MmapDedupStore) {
    let threads = 8u64;
    let per_thread = (WARMUP_KEYS as u64) / threads;
    let start = Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            scope.spawn(move || {
                for i in 0..per_thread {
                    store.insert(&make_key(0, t, i), i as i64);
                }
            });
        }
    });
    eprintln!(
        "[dedup] warm-up: {} keys inserted across {threads} threads in {:.2?}",
        threads * per_thread,
        start.elapsed(),
    );
}

/// Measures sustained `check_and_insert` throughput with `threads` producer
/// threads, each hammering its own disjoint key space (so lookups spread
/// across shards the same way concurrent topics/partitions would in
/// production, rather than all threads contending one shard).
fn measure_check_and_insert(store: &Arc<MmapDedupStore>, threads: u64, space: u8) -> f64 {
    let fresh_total = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let store = Arc::clone(store);
            let fresh_total = Arc::clone(&fresh_total);
            scope.spawn(move || {
                let mut local_fresh = 0u64;
                for i in 0..KEYS_PER_THREAD as u64 {
                    let key = make_key(space, t, i);
                    if store.check_and_insert(&key, i as i64) == DedupOutcome::Fresh {
                        local_fresh += 1;
                    }
                }
                fresh_total.fetch_add(local_fresh, Ordering::Relaxed);
            });
        }
    });
    let elapsed = start.elapsed();
    let total_ops = threads * KEYS_PER_THREAD as u64;
    let fresh = fresh_total.load(Ordering::Relaxed);
    // Every key in this phase is unique, so `fresh` should equal `total_ops`
    // — a lower count means a probe run hit `probe_limit` without finding
    // an empty slot and evicted a live (not-yet-measured) entry instead. At
    // this bench's load factor that is rare, not impossible; it is a
    // regression WORTH FLAGGING (loudly) but not a reason to fail a
    // throughput gate over statistical hash-table noise unrelated to the
    // ops/s number below.
    if fresh != total_ops {
        eprintln!(
            "[dedup] WARNING: {} of {total_ops} inserts in this phase were NOT reported Fresh \
             (evicted a live entry before this phase finished writing it) — table load may be \
             higher than intended; ops/s below is still a valid call-rate measurement",
            total_ops - fresh,
        );
    }
    total_ops as f64 / elapsed.as_secs_f64()
}

fn gate_dedup_300k(_c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("create temp dir for dedup gate");

    // Single-thread baseline: realistic for a lightly-loaded topic/partition,
    // and the floor the multi-thread number must clear by a wide margin for
    // sharding to be doing its job.
    let single_store = Arc::new(open_default_store(dir.path(), "single"));
    run_warmup(&single_store);
    let single_ops_s = measure_check_and_insert(&single_store, 1, 1);
    eprintln!(
        "[dedup] single-thread check_and_insert: {KEYS_PER_THREAD} ops = {single_ops_s:>10.0} ops/s"
    );

    // Multi-thread (8 threads, disjoint key spaces): the number this gate is
    // actually about — sustained per-record dedup throughput under the
    // concurrency a busy node's publish hot path drives it at.
    let multi_store = Arc::new(open_default_store(dir.path(), "multi"));
    run_warmup(&multi_store);
    let threads = 8u64;
    let multi_ops_s = measure_check_and_insert(&multi_store, threads, 2);
    eprintln!(
        "[dedup] {threads}-thread check_and_insert: {} ops = {multi_ops_s:>10.0} ops/s  \
         (target: >= {TARGET_OPS_S:.0} ops/s)",
        threads * KEYS_PER_THREAD as u64,
    );

    let hits = multi_store.hits();
    let evictions = multi_store.evictions();
    eprintln!(
        "[dedup] table stats after measured phase: hits={hits} evictions={evictions} \
         effective_capacity_window_ms={}",
        multi_store.effective_capacity_window_ms(),
    );

    let pass = multi_ops_s >= TARGET_OPS_S;
    eprintln!(
        "[dedup] gate result: {}  ({multi_ops_s:.0} ops/s vs {TARGET_OPS_S:.0} ops/s target)",
        if pass { "PASS" } else { "FAIL" },
    );
    assert!(
        pass,
        "dedup-300k gate: {multi_ops_s:.0} ops/s is below the {TARGET_OPS_S:.0} ops/s target"
    );
}

criterion_group!(benches, gate_dedup_300k);
criterion_main!(benches);
