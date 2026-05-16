// =============================================================================
// File: benches/sql_host_perf.rs — M3.W13 SQL host-function perf benches
// =============================================================================
//
// Criterion micro-benches for the SQL host-function backend used by addons:
// `sql_exec_v1`, `sql_query_v1`, `sql_query_one_v1`. The ABI wrappers live in
// `addon/host_functions/sql.rs`; the cost they add over the raw rusqlite call
// is dominated by `read_guest_string` + JSON param parse + `write_guest_output`
// (all in-process memcpy, sub-microsecond). The real hot path is rusqlite
// against the per-addon `AddonDbPool` — exactly what we exercise here.
//
// Targets from `notes/tentavision-plan.md` §17.8:
//   * sql/insert  < 5 ms p99
//   * sql/query   < 5 ms p99
//
// Run: `cargo bench --bench sql_host_perf -- --quick --noplot`
//
// Bench is HOME-pinned so the per-addon `data.db` lands in a tempdir and does
// not pollute the developer's real `~/.tentaflow/addons/`.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rusqlite::params;

use tentaflow_core::addon::storage_sql::{close_addon_db, open_addon_db, AddonDbPool};

const ADDON_ID: &str = "bench-sql";

fn setup_pool() -> AddonDbPool {
    let pool = open_addon_db(ADDON_ID).expect("open addon db");
    let conn = pool.get().expect("get connection");
    // DDL is forbidden from addon code at runtime — migrations apply schema —
    // but for bench fixtures we use the raw rusqlite path on the same pool to
    // create a deterministic workload table.
    conn.execute_batch(
        "
        DROP TABLE IF EXISTS bench_kv;
        CREATE TABLE bench_kv (
            id INTEGER PRIMARY KEY,
            k  TEXT NOT NULL,
            v  TEXT NOT NULL,
            n  INTEGER NOT NULL
        );
        CREATE INDEX idx_bench_kv_n ON bench_kv(n);
        DROP TABLE IF EXISTS bench_blob;
        CREATE TABLE bench_blob (
            id   INTEGER PRIMARY KEY,
            data BLOB NOT NULL
        );
        ",
    )
    .expect("schema");
    pool
}

fn populate(pool: &AddonDbPool, rows: i64) {
    let conn = pool.get().expect("populate conn");
    conn.execute("DELETE FROM bench_kv", []).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx
            .prepare("INSERT INTO bench_kv (id, k, v, n) VALUES (?1, ?2, ?3, ?4)")
            .unwrap();
        for i in 1..=rows {
            stmt.execute(params![i, format!("k{}", i), format!("v{}", i), i]).unwrap();
        }
    }
    tx.commit().unwrap();
}

fn pin_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", tmp.path());
    tmp
}

// -----------------------------------------------------------------------------
// sql/insert — DML through the same rusqlite execute() path the host fn uses
// -----------------------------------------------------------------------------

fn bench_sql_insert(c: &mut Criterion) {
    let _tmp = pin_home();
    close_addon_db(ADDON_ID);
    let pool = setup_pool();

    let mut group = c.benchmark_group("sql/insert");
    group.bench_function("kv_row", |b| {
        let mut counter: i64 = 1_000_000;
        b.iter(|| {
            counter += 1;
            let conn = pool.get().expect("conn");
            conn.execute(
                "INSERT INTO bench_kv (id, k, v, n) VALUES (?1, ?2, ?3, ?4)",
                params![counter, "kbench", "vbench", counter],
            )
            .expect("insert");
            black_box(counter);
        });
    });

    // 1 KiB BLOB insert — exercises rusqlite ToSql for &[u8].
    group.bench_function("blob_1kb", |b| {
        let data = vec![0xABu8; 1024];
        b.iter(|| {
            let conn = pool.get().expect("conn");
            conn.execute("INSERT INTO bench_blob (data) VALUES (?1)", params![&data])
                .expect("blob insert");
        });
    });
    group.finish();

    close_addon_db(ADDON_ID);
}

// -----------------------------------------------------------------------------
// sql/query — SELECT through prepare + query (matches `execute_select` path)
// -----------------------------------------------------------------------------

fn bench_sql_query(c: &mut Criterion) {
    let _tmp = pin_home();
    close_addon_db(ADDON_ID);
    let pool = setup_pool();
    populate(&pool, 1000);

    let mut group = c.benchmark_group("sql/query");

    group.bench_function("pk_lookup", |b| {
        b.iter(|| {
            let conn = pool.get().expect("conn");
            let mut stmt = conn
                .prepare("SELECT id, k, v, n FROM bench_kv WHERE id = ?1")
                .unwrap();
            let (id, k, v, n): (i64, String, String, i64) = stmt
                .query_row(params![500i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .expect("row");
            black_box((id, k, v, n));
        });
    });

    for range in [10usize, 100usize] {
        group.bench_with_input(
            BenchmarkId::new("indexed_range", range),
            &range,
            |b, &range| {
                b.iter(|| {
                    let conn = pool.get().expect("conn");
                    let mut stmt = conn
                        .prepare("SELECT id, k, v, n FROM bench_kv WHERE n BETWEEN ?1 AND ?2")
                        .unwrap();
                    let rows: Vec<(i64, String, String, i64)> = stmt
                        .query_map(params![1i64, range as i64], |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                        })
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap();
                    black_box(rows);
                });
            },
        );
    }

    group.finish();
    close_addon_db(ADDON_ID);
}

criterion_group!(benches, bench_sql_insert, bench_sql_query);
criterion_main!(benches);
