// ===== File: benchmark/db.rs — Benchmark Studio content database: schema of the instance db (plan-01 §6) and its repository =====
//
// Benchmark definitions, targets, runs and results live in the app INSTANCE
// database (`<instance data dir>/benchmark.db`, opened through
// `addon::app_db`), not in the main `tentaflow.db`: the content is local to
// the node that ran it, is never synced, and disappears with the instance on
// uninstall. The file has no foreign keys outside itself — `org_id` is a plain
// column because the instance directory is already org-scoped.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::crypto::SettingsCipher;
use crate::db::DbPool;

use super::types::{
    BenchmarkListItem, BenchmarkRecord, BenchmarkResultRecord, BenchmarkRunRecord,
    BenchmarkTargetRecord, BenchmarkTargetUpsert,
};

/// Package id, also the label of the content db migrations in the log.
const APP: &str = "benchmark-studio";

/// v1 — `benchmark_results.target_id` has NO FK on purpose: targets may be
/// edited/replaced between runs while historical results must stay intact,
/// hence the `target_label` snapshot on each row. External API keys land in
/// `api_key_enc` encrypted with the settings cipher. `api_type='local'` is a
/// target measured in-process (embedded engine, QUIC sidecar, coding-agent
/// bridge) — the only way to reach a backend without a dialable endpoint.
/// `kind` records WHERE the target came from (picked from the service list vs
/// typed by hand), `api_type` says how we talk to it.
const SCHEMA_V1: &str = r#"
CREATE TABLE benchmarks (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL,
    name         TEXT NOT NULL,
    config_json  TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_benchmarks_org ON benchmarks(org_id);

CREATE TABLE benchmark_targets (
    id            TEXT PRIMARY KEY,
    benchmark_id  TEXT NOT NULL REFERENCES benchmarks(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('service','external')),
    service_ref   TEXT,
    api_type      TEXT NOT NULL DEFAULT 'openai'
                  CHECK (api_type IN ('openai','anthropic','local')),
    host          TEXT NOT NULL,
    port          INTEGER NOT NULL DEFAULT 0,
    api_key_enc   TEXT,
    model         TEXT NOT NULL,
    label         TEXT NOT NULL
);
CREATE INDEX idx_benchmark_targets_benchmark ON benchmark_targets(benchmark_id);

CREATE TABLE benchmark_runs (
    id                TEXT PRIMARY KEY,
    benchmark_id      TEXT NOT NULL REFERENCES benchmarks(id) ON DELETE CASCADE,
    started_at        TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at       TEXT,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK (status IN ('running','success','failed','cancelled')),
    error             TEXT,
    engine_meta_json  TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX idx_benchmark_runs_benchmark ON benchmark_runs(benchmark_id, started_at DESC);

CREATE TABLE benchmark_results (
    id                TEXT PRIMARY KEY,
    run_id            TEXT NOT NULL REFERENCES benchmark_runs(id) ON DELETE CASCADE,
    target_id         TEXT NOT NULL,
    target_label      TEXT NOT NULL,
    scenario          TEXT NOT NULL CHECK (scenario IN ('latency','throughput','context','sustained')),
    variant_json      TEXT NOT NULL DEFAULT '{}',
    ttft_ms_mean      REAL,
    ttft_ms_sigma     REAL,
    prefill_tps_mean  REAL,
    prefill_tps_sigma REAL,
    decode_tps_mean   REAL,
    decode_tps_sigma  REAL,
    total_ms_mean     REAL,
    total_ms_sigma    REAL,
    p50_ms            REAL,
    p90_ms            REAL,
    p99_ms            REAL,
    requests          INTEGER NOT NULL DEFAULT 0,
    errors            INTEGER NOT NULL DEFAULT 0,
    samples_json      TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX idx_benchmark_results_run ON benchmark_results(run_id);
"#;

/// Brings the content database up to date. Idempotent: runs on every first
/// open of the instance in a process and on every install/reconcile.
pub fn migrate(conn: &Connection) -> Result<()> {
    crate::addon::app_db::run_versioned_migrations(conn, APP, &[(1, SCHEMA_V1)])
}

/// Pool of the content database of instance `addon_id`. Native apps are
/// installed under the default org (`lifecycle::install_instance`), so that is
/// the only org whose data dir can hold the file.
pub fn pool(main_db: &DbPool, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(
        main_db,
        crate::services::org::DEFAULT_ORG_ID,
        addon_id,
        migrate,
    )
}

fn acquire(pool: &DbPool) -> Result<parking_lot::MutexGuard<'_, Connection>> {
    pool.write()
        .map_err(|e| anyhow::anyhow!("benchmark db lock: {e}"))
}

/// Upserts a benchmark definition together with its full target set. Targets
/// absent from `targets` are deleted; `api_key: None` keeps a stored key,
/// `Some("")` clears it, `Some(key)` stores it encrypted with the settings
/// cipher (secrets never hit the DB in plaintext).
pub fn upsert_benchmark(
    pool: &DbPool,
    org_id: &str,
    id: &str,
    name: &str,
    config_json: &str,
    targets: &[BenchmarkTargetUpsert],
    cipher: &SettingsCipher,
) -> Result<()> {
    let conn = acquire(pool)?;
    let tx = conn.unchecked_transaction()?;
    // Org-scope guard: ON CONFLICT(id) would otherwise silently reassign an
    // existing benchmark owned by another org. Reject the cross-org upsert.
    let existing_org: Option<String> = tx
        .query_row(
            "SELECT org_id FROM benchmarks WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(owner) = existing_org {
        if owner != org_id {
            anyhow::bail!("benchmark {id} belongs to another org");
        }
    }
    tx.execute(
        "INSERT INTO benchmarks (id, org_id, name, config_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             config_json = excluded.config_json,
             updated_at = datetime('now')",
        rusqlite::params![id, org_id, name, config_json],
    )?;

    // Remove targets dropped by the caller; results keep their own label
    // snapshot, so pruning here never breaks historical runs.
    let kept_ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
    if kept_ids.is_empty() {
        tx.execute(
            "DELETE FROM benchmark_targets WHERE benchmark_id = ?1",
            rusqlite::params![id],
        )?;
    } else {
        let placeholders = kept_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "DELETE FROM benchmark_targets WHERE benchmark_id = ?1 AND id NOT IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&id];
        for kept in &kept_ids {
            params.push(kept);
        }
        tx.execute(&sql, params.as_slice())?;
    }

    for target in targets {
        // A target id already bound to a different benchmark must not be
        // hijacked into this one via ON CONFLICT(id) reassigning benchmark_id.
        let existing_bench: Option<String> = tx
            .query_row(
                "SELECT benchmark_id FROM benchmark_targets WHERE id = ?1",
                rusqlite::params![target.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner_bench) = existing_bench {
            if owner_bench != id {
                anyhow::bail!("target {} belongs to another benchmark", target.id);
            }
        }
        let api_key_enc: Option<String> = match target.api_key.as_deref() {
            Some(key) if !key.is_empty() => Some(
                cipher
                    .encrypt(key)
                    .map_err(|e| anyhow::anyhow!("encrypt benchmark api key: {e}"))?,
            ),
            _ => None,
        };
        // COALESCE keeps the stored key when the caller resends the target
        // without re-entering the secret (listing returns it redacted).
        tx.execute(
            "INSERT INTO benchmark_targets
                 (id, benchmark_id, kind, service_ref, api_type, host, port,
                  api_key_enc, model, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 benchmark_id = excluded.benchmark_id,
                 kind = excluded.kind,
                 service_ref = excluded.service_ref,
                 api_type = excluded.api_type,
                 host = excluded.host,
                 port = excluded.port,
                 api_key_enc = COALESCE(excluded.api_key_enc, benchmark_targets.api_key_enc),
                 model = excluded.model,
                 label = excluded.label",
            rusqlite::params![
                target.id,
                id,
                target.kind,
                target.service_ref,
                target.api_type,
                target.host,
                target.port,
                api_key_enc,
                target.model,
                target.label,
            ],
        )?;
        if target.api_key.as_deref() == Some("") {
            tx.execute(
                "UPDATE benchmark_targets SET api_key_enc = NULL WHERE id = ?1",
                rusqlite::params![target.id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn read_benchmark_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BenchmarkRecord> {
    Ok(BenchmarkRecord {
        id: row.get(0)?,
        org_id: row.get(1)?,
        name: row.get(2)?,
        config_json: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn read_benchmark_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BenchmarkRunRecord> {
    Ok(BenchmarkRunRecord {
        id: row.get(0)?,
        benchmark_id: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        status: row.get(4)?,
        error: row.get(5)?,
        engine_meta_json: row.get(6)?,
    })
}

/// Lists an org's benchmarks with target count and latest-run summary.
pub fn list_benchmarks(pool: &DbPool, org_id: &str) -> Result<Vec<BenchmarkListItem>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT b.id, b.org_id, b.name, b.config_json, b.created_at, b.updated_at,
                (SELECT COUNT(*) FROM benchmark_targets t WHERE t.benchmark_id = b.id)
         FROM benchmarks b WHERE b.org_id = ?1 ORDER BY b.updated_at DESC",
    )?;
    let rows: Vec<(BenchmarkRecord, u32)> = stmt
        .query_map(rusqlite::params![org_id], |row| {
            Ok((read_benchmark_row(row)?, row.get::<_, i64>(6)? as u32))
        })?
        .collect::<std::result::Result<_, _>>()?;

    let mut run_stmt = conn.prepare(
        "SELECT id, benchmark_id, started_at, finished_at, status, error, engine_meta_json
         FROM benchmark_runs WHERE benchmark_id = ?1
         ORDER BY started_at DESC LIMIT 1",
    )?;
    // Model labels of the targets in insertion order — the chip on the list card.
    let mut model_stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(model, ''), label) FROM benchmark_targets
         WHERE benchmark_id = ?1 ORDER BY rowid",
    )?;
    let mut items = Vec::with_capacity(rows.len());
    for (record, target_count) in rows {
        let last_run = run_stmt
            .query_row(rusqlite::params![record.id], read_benchmark_run_row)
            .optional()?;
        let models: Vec<String> = model_stmt
            .query_map(rusqlite::params![record.id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        items.push(BenchmarkListItem {
            record,
            target_count,
            models,
            last_run,
        });
    }
    Ok(items)
}

/// Returns a benchmark definition together with its targets, or `None`.
/// Target `api_key_enc` stays encrypted — the runner decrypts at execution.
pub fn get_benchmark(
    pool: &DbPool,
    org_id: &str,
    id: &str,
) -> Result<Option<(BenchmarkRecord, Vec<BenchmarkTargetRecord>)>> {
    let conn = acquire(pool)?;
    let record = conn
        .query_row(
            "SELECT id, org_id, name, config_json, created_at, updated_at
             FROM benchmarks WHERE id = ?1 AND org_id = ?2",
            rusqlite::params![id, org_id],
            read_benchmark_row,
        )
        .optional()?;
    let Some(record) = record else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT id, benchmark_id, kind, service_ref, api_type, host, port,
                api_key_enc, model, label
         FROM benchmark_targets WHERE benchmark_id = ?1 ORDER BY label",
    )?;
    let targets = stmt
        .query_map(rusqlite::params![id], |row| {
            Ok(BenchmarkTargetRecord {
                id: row.get(0)?,
                benchmark_id: row.get(1)?,
                kind: row.get(2)?,
                service_ref: row.get(3)?,
                api_type: row.get(4)?,
                host: row.get(5)?,
                port: row.get::<_, i64>(6)? as u16,
                api_key_enc: row.get(7)?,
                model: row.get(8)?,
                label: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some((record, targets)))
}

/// Deletes a benchmark (targets, runs and results cascade). Returns whether
/// a row was actually removed.
pub fn delete_benchmark(pool: &DbPool, org_id: &str, id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let deleted = conn.execute(
        "DELETE FROM benchmarks WHERE id = ?1 AND org_id = ?2",
        rusqlite::params![id, org_id],
    )?;
    Ok(deleted > 0)
}

/// Opens a new run in 'running' state and returns its id. `engine_meta_json`
/// snapshots version/env context for later run-to-run comparisons.
pub fn create_benchmark_run(
    pool: &DbPool,
    benchmark_id: &str,
    engine_meta_json: &str,
) -> Result<String> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO benchmark_runs (id, benchmark_id, engine_meta_json)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![run_id, benchmark_id, engine_meta_json],
    )?;
    Ok(run_id)
}

/// Closes a run with a terminal status ('success' | 'failed' | 'cancelled').
pub fn finish_benchmark_run(
    pool: &DbPool,
    run_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE benchmark_runs
         SET status = ?2, error = ?3, finished_at = datetime('now')
         WHERE id = ?1",
        rusqlite::params![run_id, status, error],
    )?;
    Ok(())
}

/// Single run lookup scoped to an org (via its parent benchmark). `None` when
/// the run does not exist or belongs to another org.
pub fn get_benchmark_run(
    pool: &DbPool,
    org_id: &str,
    run_id: &str,
) -> Result<Option<BenchmarkRunRecord>> {
    let conn = acquire(pool)?;
    conn.query_row(
        "SELECT r.id, r.benchmark_id, r.started_at, r.finished_at, r.status, r.error,
                r.engine_meta_json
         FROM benchmark_runs r
         JOIN benchmarks b ON b.id = r.benchmark_id
         WHERE r.id = ?1 AND b.org_id = ?2",
        rusqlite::params![run_id, org_id],
        read_benchmark_run_row,
    )
    .optional()
    .map_err(Into::into)
}

pub fn insert_benchmark_result(pool: &DbPool, result: &BenchmarkResultRecord) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO benchmark_results
             (id, run_id, target_id, target_label, scenario, variant_json,
              ttft_ms_mean, ttft_ms_sigma, prefill_tps_mean, prefill_tps_sigma,
              decode_tps_mean, decode_tps_sigma, total_ms_mean, total_ms_sigma,
              p50_ms, p90_ms, p99_ms, requests, errors, samples_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        rusqlite::params![
            result.id,
            result.run_id,
            result.target_id,
            result.target_label,
            result.scenario,
            result.variant_json,
            result.ttft_ms_mean,
            result.ttft_ms_sigma,
            result.prefill_tps_mean,
            result.prefill_tps_sigma,
            result.decode_tps_mean,
            result.decode_tps_sigma,
            result.total_ms_mean,
            result.total_ms_sigma,
            result.p50_ms,
            result.p90_ms,
            result.p99_ms,
            result.requests,
            result.errors,
            result.samples_json,
        ],
    )?;
    Ok(())
}

pub fn list_benchmark_runs(
    pool: &DbPool,
    org_id: &str,
    benchmark_id: &str,
) -> Result<Vec<BenchmarkRunRecord>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT r.id, r.benchmark_id, r.started_at, r.finished_at, r.status, r.error,
                r.engine_meta_json
         FROM benchmark_runs r
         JOIN benchmarks b ON b.id = r.benchmark_id
         WHERE r.benchmark_id = ?1 AND b.org_id = ?2 ORDER BY r.started_at DESC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![benchmark_id, org_id],
        read_benchmark_run_row,
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_benchmark_run_results(
    pool: &DbPool,
    org_id: &str,
    run_id: &str,
) -> Result<Vec<BenchmarkResultRecord>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT res.id, res.run_id, res.target_id, res.target_label, res.scenario,
                res.variant_json, res.ttft_ms_mean, res.ttft_ms_sigma,
                res.prefill_tps_mean, res.prefill_tps_sigma, res.decode_tps_mean,
                res.decode_tps_sigma, res.total_ms_mean, res.total_ms_sigma,
                res.p50_ms, res.p90_ms, res.p99_ms, res.requests, res.errors,
                res.samples_json
         FROM benchmark_results res
         JOIN benchmark_runs run ON run.id = res.run_id
         JOIN benchmarks b ON b.id = run.benchmark_id
         WHERE res.run_id = ?1 AND b.org_id = ?2
         ORDER BY res.target_label, res.scenario, res.variant_json",
    )?;
    let rows = stmt.query_map(rusqlite::params![run_id, org_id], |row| {
        Ok(BenchmarkResultRecord {
            id: row.get(0)?,
            run_id: row.get(1)?,
            target_id: row.get(2)?,
            target_label: row.get(3)?,
            scenario: row.get(4)?,
            variant_json: row.get(5)?,
            ttft_ms_mean: row.get(6)?,
            ttft_ms_sigma: row.get(7)?,
            prefill_tps_mean: row.get(8)?,
            prefill_tps_sigma: row.get(9)?,
            decode_tps_mean: row.get(10)?,
            decode_tps_sigma: row.get(11)?,
            total_ms_mean: row.get(12)?,
            total_ms_sigma: row.get(13)?,
            p50_ms: row.get(14)?,
            p90_ms: row.get(15)?,
            p99_ms: row.get(16)?,
            requests: row.get::<_, i64>(17)? as u32,
            errors: row.get::<_, i64>(18)? as u32,
            samples_json: row.get(19)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Latest runs across all of an org's benchmarks, newest first, paired with
/// the benchmark name for the dashboard's "recent runs" list.
pub fn list_recent_benchmark_runs(
    pool: &DbPool,
    org_id: &str,
    limit: u32,
) -> Result<Vec<(BenchmarkRunRecord, String)>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT r.id, r.benchmark_id, r.started_at, r.finished_at, r.status, r.error,
                r.engine_meta_json, b.name
         FROM benchmark_runs r
         JOIN benchmarks b ON b.id = r.benchmark_id
         WHERE b.org_id = ?1
         ORDER BY r.started_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![org_id, limit], |row| {
        Ok((read_benchmark_run_row(row)?, row.get::<_, String>(7)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const ORG: &str = "org-default";

    fn fresh_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrate(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn cipher() -> SettingsCipher {
        SettingsCipher::new(&[0u8; 32])
    }

    fn target(id: &str, api_type: &str, api_key: Option<&str>) -> BenchmarkTargetUpsert {
        BenchmarkTargetUpsert {
            id: id.to_string(),
            kind: "external".to_string(),
            service_ref: None,
            api_type: api_type.to_string(),
            host: "api.example.test".to_string(),
            port: 443,
            api_key: api_key.map(str::to_string),
            model: format!("model-{id}"),
            label: format!("Label {id}"),
        }
    }

    fn result(run_id: &str, target_id: &str, scenario: &str) -> BenchmarkResultRecord {
        BenchmarkResultRecord {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            target_id: target_id.to_string(),
            target_label: format!("Label {target_id}"),
            scenario: scenario.to_string(),
            variant_json: "{}".to_string(),
            ttft_ms_mean: Some(12.5),
            ttft_ms_sigma: Some(1.0),
            prefill_tps_mean: None,
            prefill_tps_sigma: None,
            decode_tps_mean: Some(40.0),
            decode_tps_sigma: Some(2.0),
            total_ms_mean: Some(300.0),
            total_ms_sigma: Some(10.0),
            p50_ms: Some(290.0),
            p90_ms: Some(310.0),
            p99_ms: Some(330.0),
            requests: 8,
            errors: 1,
            samples_json: "[]".to_string(),
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 1);
    }

    /// An in-process benchmark target (`api_type='local'`) — embedded engine,
    /// coding-agent bridge, sidecar — must pass the CHECK constraint; the two
    /// endpoint protocols keep working, and an unknown one is still rejected.
    #[test]
    fn schema_accepts_in_process_benchmark_target() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO benchmarks (id, org_id, name, config_json) VALUES ('b1', ?1, 'b', '{}')",
            rusqlite::params![ORG],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO benchmark_targets
                 (id, benchmark_id, kind, api_type, host, port, model, label)
             VALUES ('t1', 'b1', 'service', 'local', '', 0, 'bielik-4.5b', 'Bielik')",
            [],
        )
        .expect("in-process target must pass the api_type CHECK");
        conn.execute(
            "INSERT INTO benchmark_targets
                 (id, benchmark_id, kind, api_type, host, port, model, label)
             VALUES ('t2', 'b1', 'external', 'anthropic', 'api.anthropic.com', 443, 'm', 'A')",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO benchmark_targets
                     (id, benchmark_id, kind, api_type, host, port, model, label)
                 VALUES ('t3', 'b1', 'external', 'grpc', 'h', 1, 'm', 'G')",
                [],
            )
            .is_err());
    }

    #[test]
    fn upsert_get_list_delete_round_trip() {
        let pool = fresh_pool();
        let cipher = cipher();
        upsert_benchmark(
            &pool,
            ORG,
            "b1",
            "first",
            "{}",
            &[target("t1", "openai", Some("sk-secret")), target("t2", "local", None)],
            &cipher,
        )
        .unwrap();

        let (record, targets) = get_benchmark(&pool, ORG, "b1").unwrap().expect("exists");
        assert_eq!(record.name, "first");
        assert_eq!(record.org_id, ORG);
        assert_eq!(targets.len(), 2);
        // Keys are encrypted at rest and never come back in plaintext.
        let t1 = targets.iter().find(|t| t.id == "t1").unwrap();
        let enc = t1.api_key_enc.as_deref().expect("stored key");
        assert_ne!(enc, "sk-secret");
        assert_eq!(cipher.decrypt(enc).unwrap(), "sk-secret");
        assert!(targets.iter().find(|t| t.id == "t2").unwrap().api_key_enc.is_none());

        // Another org sees nothing and cannot hijack the id.
        assert!(get_benchmark(&pool, "org-other", "b1").unwrap().is_none());
        assert!(upsert_benchmark(&pool, "org-other", "b1", "x", "{}", &[], &cipher).is_err());

        // Resending without the secret keeps it; an empty string clears it;
        // a target left out of the set is pruned.
        upsert_benchmark(
            &pool,
            ORG,
            "b1",
            "renamed",
            r#"{"request_timeout_secs":5}"#,
            &[target("t1", "openai", None)],
            &cipher,
        )
        .unwrap();
        let (record, targets) = get_benchmark(&pool, ORG, "b1").unwrap().unwrap();
        assert_eq!(record.name, "renamed");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].api_key_enc.is_some());
        upsert_benchmark(&pool, ORG, "b1", "renamed", "{}", &[target("t1", "openai", Some(""))], &cipher)
            .unwrap();
        let (_, targets) = get_benchmark(&pool, ORG, "b1").unwrap().unwrap();
        assert!(targets[0].api_key_enc.is_none());

        // A target id bound to another benchmark cannot be pulled over.
        upsert_benchmark(&pool, ORG, "b2", "second", "{}", &[target("t9", "openai", None)], &cipher)
            .unwrap();
        assert!(
            upsert_benchmark(&pool, ORG, "b1", "renamed", "{}", &[target("t9", "openai", None)], &cipher)
                .is_err()
        );

        let items = list_benchmarks(&pool, ORG).unwrap();
        assert_eq!(items.len(), 2);
        let b1 = items.iter().find(|i| i.record.id == "b1").unwrap();
        assert_eq!(b1.target_count, 1);
        assert_eq!(b1.models, vec!["model-t1".to_string()]);
        assert!(b1.last_run.is_none());
        assert!(list_benchmarks(&pool, "org-other").unwrap().is_empty());

        assert!(!delete_benchmark(&pool, "org-other", "b1").unwrap());
        assert!(delete_benchmark(&pool, ORG, "b1").unwrap());
        assert!(get_benchmark(&pool, ORG, "b1").unwrap().is_none());
        let orphans: i64 = pool
            .write()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM benchmark_targets WHERE benchmark_id = 'b1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "targets cascade with their benchmark");
    }

    #[test]
    fn runs_and_results_round_trip() {
        let pool = fresh_pool();
        let cipher = cipher();
        upsert_benchmark(&pool, ORG, "b1", "bench", "{}", &[target("t1", "openai", None)], &cipher)
            .unwrap();

        let run_id = create_benchmark_run(&pool, "b1", r#"{"node_id":"n1"}"#).unwrap();
        let run = get_benchmark_run(&pool, ORG, &run_id).unwrap().expect("run");
        assert_eq!(run.status, "running");
        assert!(run.finished_at.is_none());
        assert_eq!(run.engine_meta_json, r#"{"node_id":"n1"}"#);
        // Org scoping goes through the parent benchmark.
        assert!(get_benchmark_run(&pool, "org-other", &run_id).unwrap().is_none());

        insert_benchmark_result(&pool, &result(&run_id, "t1", "latency")).unwrap();
        insert_benchmark_result(&pool, &result(&run_id, "t1", "throughput")).unwrap();
        // The result keeps its label snapshot even without a matching target.
        insert_benchmark_result(&pool, &result(&run_id, "t-gone", "context")).unwrap();

        finish_benchmark_run(&pool, &run_id, "failed", Some("boom")).unwrap();
        let run = get_benchmark_run(&pool, ORG, &run_id).unwrap().unwrap();
        assert_eq!(run.status, "failed");
        assert_eq!(run.error.as_deref(), Some("boom"));
        assert!(run.finished_at.is_some());

        let rows = get_benchmark_run_results(&pool, ORG, &run_id).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].requests, 8);
        assert_eq!(rows[0].errors, 1);
        assert_eq!(rows[0].ttft_ms_mean, Some(12.5));
        assert!(get_benchmark_run_results(&pool, "org-other", &run_id)
            .unwrap()
            .is_empty());

        let runs = list_benchmark_runs(&pool, ORG, "b1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        let recent = list_recent_benchmark_runs(&pool, ORG, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].1, "bench");
        assert!(list_recent_benchmark_runs(&pool, "org-other", 10).unwrap().is_empty());

        let items = list_benchmarks(&pool, ORG).unwrap();
        assert_eq!(items[0].last_run.as_ref().unwrap().id, run_id);

        // Deleting the benchmark cascades through runs into results.
        assert!(delete_benchmark(&pool, ORG, "b1").unwrap());
        let leftovers: i64 = pool
            .write()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM benchmark_results", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leftovers, 0);
    }
}
