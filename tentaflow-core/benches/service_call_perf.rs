// =============================================================================
// File: benches/service_call_perf.rs — M3.W13 service_call overhead bench
// =============================================================================
//
// Target from `notes/tentavision-plan.md` §17.8:
//   service_call overhead (no inference, no QUIC dispatch)  < 5 ms p99
//
// `service_call_v1` adds the following work on top of frame pickup (which is
// already covered by `streaming_pickup_perf::pickup_handler_direct` /
// `pickup_http_roundtrip`):
//
//   1. `resolve_model_alias_for_addon` — read transaction across
//      `model_aliases`, `model_alias_owners`, `model_alias_visibility`,
//      `addon_uses_alias`, `model_alias_consumers` (F1a §6.6 alias gate).
//   2. `alias_calls` insert — durable per-call audit row (FK to
//      `model_aliases`, indexed on `alias_id` + `ts`).
//   3. `audit_log` insert with risk class — the same A-class compliance row
//      every host fn writes.
//
// We bench (1)+(2)+(3) on an in-memory SQLite that has the full production
// schema applied via `db::migrations::run`. The remaining cost of the host
// fn — wasmtime memory read/write, hyper/QUIC client dispatch, rate limiter
// hit — is either negligible (<5 us memcpy) or out of scope for a unit
// micro-bench (QUIC dispatch lives in tentaflow-transport benches).
//
// Run: `cargo bench --bench service_call_perf -- --quick --noplot`

use std::hint::black_box;
use std::sync::{Arc, Mutex};

use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::{params, Connection};

use tentaflow_core::db::repository::resolve_model_alias_for_addon;
use tentaflow_core::db::{migrations, DbPool};

const CALLER_ADDON: &str = "bench-caller";
const OWNER_ADDON: &str = "bench-owner";
const ALIAS_NAME: &str = "bench-alias";

fn make_pool() -> DbPool {
    let conn = Connection::open_in_memory().expect("memory db");
    migrations::run(&conn).expect("migrations");

    // Seed model_aliases + owner + public visibility + caller uses_alias grant.
    // This mirrors the "happy path" service_call sees once the addon has been
    // installed with `[[uses_alias]]` and the alias owner has set
    // `visibility = 'public'`.
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model, is_active) VALUES (?1, 'gpt-4', 1)",
        params![ALIAS_NAME],
    )
    .expect("insert alias");
    let alias_id: i64 = conn
        .query_row(
            "SELECT id FROM model_aliases WHERE alias = ?1",
            params![ALIAS_NAME],
            |row| row.get(0),
        )
        .expect("alias id");
    conn.execute(
        "INSERT INTO model_alias_owners (alias_id, owner_type, owner_id) VALUES (?1, 'addon', ?2)",
        params![alias_id, OWNER_ADDON],
    )
    .expect("insert owner");
    conn.execute(
        "INSERT INTO model_alias_visibility (alias_id, visibility, updated_at) \
         VALUES (?1, 'public', 0)",
        params![alias_id],
    )
    .expect("insert visibility");
    conn.execute(
        "INSERT INTO addon_uses_alias \
             (addon_id, alias_target_name, required, reason, grant_status, created_at) \
         VALUES (?1, ?2, 0, 'bench', 'granted', 0)",
        params![CALLER_ADDON, ALIAS_NAME],
    )
    .expect("insert uses_alias");

    Arc::new(Mutex::new(conn))
}

// Inserts an `alias_calls` row with the same column set `log_alias_call` uses.
// Kept inline so the bench reflects the cost the host fn actually pays.
fn insert_alias_call(conn: &Connection, alias_id: i64, request_id: &str) {
    conn.execute(
        "INSERT INTO alias_calls \
             (alias_id, alias_name, method, target_used, target_node_id, service_id, \
              caller_addon_id, caller_user_id, request_id, duration_ms, payload_bytes, \
              response_bytes, fallback_used, fallback_chain_position, result, error_code, ts) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL, ?7, ?8, ?9, ?10, 0, NULL, ?11, NULL, ?12)",
        params![
            alias_id,
            ALIAS_NAME,
            "service.request",
            ALIAS_NAME,
            ALIAS_NAME,
            CALLER_ADDON,
            request_id,
            1i64,
            64i64,
            128i64,
            "ok",
            1_715_500_000i64,
        ],
    )
    .expect("alias_calls insert");
}

// Mirrors `audit_log_with_risk` but uses a held connection so the bench
// stays in-process and does not contend on the Mutex on every iter.
fn insert_audit_log(conn: &Connection, request_id: &str) {
    conn.execute(
        "INSERT INTO audit_log \
             (user_id, addon_id, instance_id, action, resource_type, resource_id, \
              result, error_message, action_hash, risk_class, related_claim_id, request_id) \
         VALUES (NULL, ?1, NULL, 'service.request', 'service', ?2, 'ok', NULL, 0, 'A', NULL, ?3)",
        params![CALLER_ADDON, ALIAS_NAME, request_id],
    )
    .expect("audit_log insert");
}

fn bench_alias_resolve(c: &mut Criterion) {
    let pool = make_pool();
    let mut group = c.benchmark_group("service_call");

    // Just the gate evaluation — read-only transaction with 4 row probes.
    group.bench_function("alias_resolve_public_path", |b| {
        b.iter(|| {
            let row = resolve_model_alias_for_addon(
                &pool,
                ALIAS_NAME,
                Some(CALLER_ADDON),
                Some("service.request"),
                Some("req-bench"),
            )
            .expect("resolve");
            black_box(row);
        });
    });

    // Full overhead path: gate + alias_calls + audit_log. Roughly what the
    // production `service_call_v1` charges per call before dispatching to the
    // backend service.
    group.bench_function("full_overhead", |b| {
        let alias_id: i64 = {
            let conn = pool.lock().unwrap();
            conn.query_row(
                "SELECT id FROM model_aliases WHERE alias = ?1",
                params![ALIAS_NAME],
                |row| row.get(0),
            )
            .unwrap()
        };
        let mut req_seq: u64 = 0;
        b.iter(|| {
            req_seq = req_seq.wrapping_add(1);
            let request_id = format!("req-{req_seq}");
            let row = resolve_model_alias_for_addon(
                &pool,
                ALIAS_NAME,
                Some(CALLER_ADDON),
                Some("service.request"),
                Some(&request_id),
            )
            .expect("resolve");
            let conn = pool.lock().unwrap();
            insert_alias_call(&conn, alias_id, &request_id);
            insert_audit_log(&conn, &request_id);
            drop(conn);
            black_box(row);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_alias_resolve);
criterion_main!(benches);
