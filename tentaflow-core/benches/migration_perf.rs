// =============================================================================
// File: benches/migration_perf.rs — M3.W13 migration apply perf bench
// =============================================================================
//
// Criterion bench for `db::migrations::run()` — the canonical schema bootstrap
// path used by every Core / Router / Desktop start. Two scenarios:
//
//   * `fresh_db` — open an `:memory:` SQLite, apply the full migration list
//                  end-to-end. Measures the cold-boot schema cost.
//   * `idempotent` — apply once, then re-run on the same connection. Every
//                    migration's `version > current_version` check must
//                    short-circuit; this is what every subsequent process
//                    start pays.
//
// No hard p99 target — informational. Plan §17.8 expects the fresh path to
// complete in well under a second; idempotent runs must be effectively free
// (only the SELECT MAX(version) probe).
//
// Run: `cargo bench --bench migration_perf -- --quick --noplot`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::Connection;

use tentaflow_core::db::migrations;

fn bench_fresh_db_full_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("migration/apply");
    group.sample_size(20); // each iter does the full migration list — slow.

    group.bench_function("fresh_db", |b| {
        b.iter_with_large_drop(|| {
            let conn = Connection::open_in_memory().expect("memory db");
            migrations::run(&conn).expect("migrations");
            black_box(conn)
        });
    });

    group.bench_function("idempotent", |b| {
        // Apply once outside the timed loop — the bench then measures the
        // no-op re-run cost (SELECT MAX(version) + per-migration skip).
        let conn = Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("initial migrations");
        b.iter(|| {
            migrations::run(&conn).expect("rerun");
        });
    });

    group.finish();
}

criterion_group!(benches, bench_fresh_db_full_apply);
criterion_main!(benches);
