// =============================================================================
// File: tentanas/db.rs — schema and row access of the per-node `tentanas.db`
//       (plan-02 §5.2). The file lives in the instance data dir and is opened
//       through `addon::app_db`; nothing in it is synced — every node keeps
//       its own disks, samples, alerts and jobs, the dashboard reaches them
//       through node forwarding. Settings that must reach other nodes go to
//       `addon_config` of the instance instead.
// =============================================================================

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use tentaflow_protocol::tentanas::{NasAlert, NasDiskSample, NasJob};

use crate::db::DbPool;

const APP: &str = "tentanas";

/// Append-only. A released step is never edited: the runner records applied
/// versions per file and only executes the ones above the recorded maximum.
const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    "CREATE TABLE nas_settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_environment (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL,
        probed_at TEXT NOT NULL
    );
    CREATE TABLE nas_disks (
        disk_id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        model TEXT NOT NULL,
        serial TEXT NOT NULL,
        wwn TEXT,
        size_bytes INTEGER NOT NULL,
        kind TEXT NOT NULL,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        smart_json TEXT,
        smart_read_at TEXT,
        health TEXT NOT NULL DEFAULT 'unknown',
        health_reason TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE nas_disk_samples (
        disk_id TEXT NOT NULL,
        at TEXT NOT NULL,
        temperature_c INTEGER,
        reallocated INTEGER,
        pending INTEGER,
        crc_errors INTEGER,
        media_errors INTEGER,
        read_bps INTEGER NOT NULL DEFAULT 0,
        write_bps INTEGER NOT NULL DEFAULT 0,
        await_ms REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (disk_id, at)
    ) WITHOUT ROWID;
    CREATE TABLE nas_alerts (
        alert_id TEXT PRIMARY KEY,
        severity TEXT NOT NULL,
        subject_kind TEXT NOT NULL,
        subject_id TEXT NOT NULL,
        title TEXT NOT NULL,
        detail TEXT NOT NULL,
        raised_at TEXT NOT NULL,
        acked_at TEXT,
        resolved_at TEXT,
        dedupe_key TEXT NOT NULL
    );
    CREATE UNIQUE INDEX nas_alerts_open_dedupe
        ON nas_alerts(dedupe_key) WHERE resolved_at IS NULL;
    CREATE INDEX nas_alerts_subject ON nas_alerts(subject_kind, subject_id);
    CREATE TABLE nas_jobs (
        job_id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        subject TEXT NOT NULL,
        status TEXT NOT NULL,
        progress_pct INTEGER,
        started_by TEXT NOT NULL,
        started_at TEXT NOT NULL,
        finished_at TEXT,
        error TEXT,
        log TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX nas_jobs_started ON nas_jobs(started_at DESC);",
)];

/// `app_db::Migrate` for the TentaNas instance database.
pub fn migrate(conn: &Connection) -> Result<()> {
    crate::addon::app_db::run_versioned_migrations(conn, APP, MIGRATIONS)
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn write(pool: &DbPool) -> Result<parking_lot::MutexGuard<'_, Connection>> {
    pool.write().map_err(|e| anyhow!("tentanas db lock: {e}"))
}

// ----- settings ---------------------------------------------------------------

pub fn setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT value FROM nas_settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now()],
    )?;
    Ok(())
}

// ----- environment cache -------------------------------------------------------

pub fn cached_environment(pool: &DbPool) -> Result<Option<(String, String)>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT json, probed_at FROM nas_environment WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn store_environment(pool: &DbPool, json: &str, probed_at: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_environment (id, json, probed_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json, probed_at = excluded.probed_at",
        params![json, probed_at],
    )?;
    Ok(())
}

// ----- disks -------------------------------------------------------------------

/// Identity of a disk as the inventory last saw it. `smart_json` is the raw
/// `smartctl --json=c -x` document, the source of attributes and self-tests
/// on the detail view — it is parsed on demand rather than normalized into
/// columns because the attribute table differs per vendor.
pub struct DiskRow {
    pub disk_id: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub smart_json: Option<String>,
    pub smart_read_at: Option<String>,
    pub health: String,
    pub health_reason: String,
}

pub struct DiskIdentity<'a> {
    pub disk_id: &'a str,
    pub name: &'a str,
    pub model: &'a str,
    pub serial: &'a str,
    pub wwn: Option<&'a str>,
    pub size_bytes: u64,
    pub kind: &'a str,
}

pub fn upsert_disk_seen(pool: &DbPool, disk: &DiskIdentity<'_>) -> Result<()> {
    let conn = write(pool)?;
    let ts = now();
    conn.execute(
        "INSERT INTO nas_disks (disk_id, name, model, serial, wwn, size_bytes, kind,
                                first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(disk_id) DO UPDATE SET
            name = excluded.name, model = excluded.model, serial = excluded.serial,
            wwn = excluded.wwn, size_bytes = excluded.size_bytes, kind = excluded.kind,
            last_seen_at = excluded.last_seen_at",
        params![
            disk.disk_id,
            disk.name,
            disk.model,
            disk.serial,
            disk.wwn,
            disk.size_bytes as i64,
            disk.kind,
            ts
        ],
    )?;
    Ok(())
}

pub fn disk_row(pool: &DbPool, disk_id: &str) -> Result<Option<DiskRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT disk_id, first_seen_at, last_seen_at, smart_json, smart_read_at,
                    health, health_reason
             FROM nas_disks WHERE disk_id = ?1",
            params![disk_id],
            |r| {
                Ok(DiskRow {
                    disk_id: r.get(0)?,
                    first_seen_at: r.get(1)?,
                    last_seen_at: r.get(2)?,
                    smart_json: r.get(3)?,
                    smart_read_at: r.get(4)?,
                    health: r.get(5)?,
                    health_reason: r.get(6)?,
                })
            },
        )
        .optional()?)
}

pub fn store_smart(
    pool: &DbPool,
    disk_id: &str,
    smart_json: &str,
    health: &str,
    health_reason: &str,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_disks SET smart_json = ?2, smart_read_at = ?3, health = ?4, health_reason = ?5
         WHERE disk_id = ?1",
        params![disk_id, smart_json, now(), health, health_reason],
    )?;
    Ok(())
}

pub fn store_health(pool: &DbPool, disk_id: &str, health: &str, reason: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_disks SET health = ?2, health_reason = ?3 WHERE disk_id = ?1",
        params![disk_id, health, reason],
    )?;
    Ok(())
}

// ----- samples -----------------------------------------------------------------

pub struct SampleInsert<'a> {
    pub disk_id: &'a str,
    pub at: &'a str,
    pub temperature_c: Option<i32>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    pub read_bps: u64,
    pub write_bps: u64,
    pub await_ms: f64,
}

pub fn insert_samples(pool: &DbPool, samples: &[SampleInsert<'_>]) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_disk_samples
                (disk_id, at, temperature_c, reallocated, pending, crc_errors, media_errors,
                 read_bps, write_bps, await_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for s in samples {
            stmt.execute(params![
                s.disk_id,
                s.at,
                s.temperature_c,
                s.reallocated.map(|v| v as i64),
                s.pending.map(|v| v as i64),
                s.crc_errors.map(|v| v as i64),
                s.media_errors.map(|v| v as i64),
                s.read_bps as i64,
                s.write_bps as i64,
                s.await_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Samples of one disk since `since` (RFC 3339), oldest first.
pub fn samples_since(pool: &DbPool, disk_id: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_samples WHERE disk_id = ?1 AND at >= ?2 ORDER BY at",
    )?;
    let rows = stmt
        .query_map(params![disk_id, since], |r| {
            Ok(NasDiskSample {
                at: r.get(0)?,
                temperature_c: r.get(1)?,
                reallocated_sectors: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                pending_sectors: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                read_bps: r.get::<_, i64>(4)? as u64,
                write_bps: r.get::<_, i64>(5)? as u64,
                await_ms: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The raw value of one SMART attribute as it was ~7 days ago: the "trend"
/// column of the attribute table (§5.4).
pub fn attribute_week_ago(pool: &DbPool, disk_id: &str, column: &str) -> Result<Option<i64>> {
    // Column name comes from a fixed set in disks.rs, never from the wire.
    debug_assert!(["reallocated", "pending", "crc_errors", "media_errors"].contains(&column));
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let since = (chrono::Utc::now() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(conn
        .query_row(
            &format!(
                "SELECT {column} FROM nas_disk_samples
                 WHERE disk_id = ?1 AND at <= ?2 AND {column} IS NOT NULL
                 ORDER BY at DESC LIMIT 1"
            ),
            params![disk_id, since],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Retention (§5.4): minute samples are kept 30 days.
pub fn prune_samples(pool: &DbPool) -> Result<usize> {
    let conn = write(pool)?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(conn.execute(
        "DELETE FROM nas_disk_samples WHERE at < ?1",
        params![cutoff],
    )?)
}

// ----- alerts ------------------------------------------------------------------

fn alert_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasAlert> {
    Ok(NasAlert {
        alert_id: r.get(0)?,
        severity: r.get(1)?,
        subject_kind: r.get(2)?,
        subject_id: r.get(3)?,
        title: r.get(4)?,
        detail: r.get(5)?,
        raised_at: r.get(6)?,
        acked_at: r.get(7)?,
        resolved_at: r.get(8)?,
    })
}

const ALERT_COLUMNS: &str = "alert_id, severity, subject_kind, subject_id, title, detail, \
                             raised_at, acked_at, resolved_at";

/// Raises an alert unless an open one with the same `dedupe_key` exists.
/// Returns true when a new row was inserted.
pub fn raise_alert(
    pool: &DbPool,
    dedupe_key: &str,
    severity: &str,
    subject_kind: &str,
    subject_id: &str,
    title: &str,
    detail: &str,
) -> Result<bool> {
    let conn = write(pool)?;
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO nas_alerts
            (alert_id, severity, subject_kind, subject_id, title, detail, raised_at, dedupe_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            uuid::Uuid::now_v7().to_string(),
            severity,
            subject_kind,
            subject_id,
            title,
            detail,
            now(),
            dedupe_key
        ],
    )?;
    Ok(inserted == 1)
}

pub fn resolve_alert(pool: &DbPool, dedupe_key: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_alerts SET resolved_at = ?2 WHERE dedupe_key = ?1 AND resolved_at IS NULL",
        params![dedupe_key, now()],
    )?;
    Ok(())
}

pub fn ack_alert(pool: &DbPool, alert_id: &str) -> Result<bool> {
    let conn = write(pool)?;
    let n = conn.execute(
        "UPDATE nas_alerts SET acked_at = ?2 WHERE alert_id = ?1 AND acked_at IS NULL",
        params![alert_id, now()],
    )?;
    Ok(n == 1)
}

pub fn list_alerts(pool: &DbPool, include_acked: bool) -> Result<Vec<NasAlert>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let sql = format!(
        "SELECT {ALERT_COLUMNS} FROM nas_alerts
         WHERE resolved_at IS NULL {}
         ORDER BY raised_at DESC LIMIT 500",
        if include_acked { "" } else { "AND acked_at IS NULL" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], alert_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn alerts_for_subject(pool: &DbPool, kind: &str, subject_id: &str) -> Result<Vec<NasAlert>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ALERT_COLUMNS} FROM nas_alerts
         WHERE subject_kind = ?1 AND subject_id = ?2 ORDER BY raised_at DESC LIMIT 50"
    ))?;
    let rows = stmt
        .query_map(params![kind, subject_id], alert_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count_open_alerts(pool: &DbPool) -> Result<u32> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM nas_alerts WHERE resolved_at IS NULL AND acked_at IS NULL",
        [],
        |r| r.get::<_, i64>(0),
    )? as u32)
}

// ----- jobs --------------------------------------------------------------------

fn job_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasJob> {
    let log: String = r.get(9)?;
    Ok(NasJob {
        job_id: r.get(0)?,
        kind: r.get(1)?,
        subject: r.get(2)?,
        status: r.get(3)?,
        progress_pct: r.get::<_, Option<i64>>(4)?.map(|v| v.clamp(0, 100) as u8),
        started_by: r.get(5)?,
        started_at: r.get(6)?,
        finished_at: r.get(7)?,
        error: r.get(8)?,
        log: if log.is_empty() {
            Vec::new()
        } else {
            log.lines().map(str::to_string).collect()
        },
    })
}

const JOB_COLUMNS: &str = "job_id, kind, subject, status, progress_pct, started_by, started_at, \
                           finished_at, error, log";

pub fn insert_job(pool: &DbPool, job: &NasJob) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_jobs (job_id, kind, subject, status, progress_pct, started_by,
                               started_at, log)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            job.job_id,
            job.kind,
            job.subject,
            job.status,
            job.progress_pct.map(i64::from),
            job.started_by,
            job.started_at,
            job.log.join("\n")
        ],
    )?;
    Ok(())
}

pub fn append_job_log(pool: &DbPool, job_id: &str, line: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_jobs SET log = CASE WHEN log = '' THEN ?2 ELSE log || char(10) || ?2 END
         WHERE job_id = ?1",
        params![job_id, line],
    )?;
    Ok(())
}

pub fn set_job_progress(pool: &DbPool, job_id: &str, status: &str, pct: Option<u8>) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_jobs SET status = ?2, progress_pct = ?3 WHERE job_id = ?1",
        params![job_id, status, pct.map(i64::from)],
    )?;
    Ok(())
}

pub fn finish_job(pool: &DbPool, job_id: &str, status: &str, error: Option<&str>) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_jobs SET status = ?2, error = ?3, finished_at = ?4,
                progress_pct = CASE WHEN ?2 = 'succeeded' THEN 100 ELSE progress_pct END
         WHERE job_id = ?1",
        params![job_id, status, error, now()],
    )?;
    Ok(())
}

pub fn job(pool: &DbPool, job_id: &str) -> Result<Option<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM nas_jobs WHERE job_id = ?1"),
            params![job_id],
            job_from_row,
        )
        .optional()?)
}

pub fn list_jobs(pool: &DbPool, limit: u32) -> Result<Vec<NasJob>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {JOB_COLUMNS} FROM nas_jobs ORDER BY started_at DESC LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map(params![i64::from(limit.clamp(1, 500))], job_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Jobs that were `running` when the process died: marked failed on init so
/// the list never shows a spinner for work nobody is doing.
pub fn fail_orphaned_jobs(pool: &DbPool) -> Result<usize> {
    let conn = write(pool)?;
    Ok(conn.execute(
        "UPDATE nas_jobs SET status = 'failed', error = 'interrupted by core restart',
                finished_at = ?1
         WHERE status IN ('queued', 'running')",
        params![now()],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn migration_is_idempotent_and_tables_exist() {
        let p = pool();
        let conn = p.write().unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'nas_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn alert_dedupe_keeps_one_open_row_per_key() {
        let p = pool();
        assert!(raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        assert!(!raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        resolve_alert(&p, "disk:a:temp").unwrap();
        assert!(raise_alert(&p, "disk:a:temp", "warning", "disk", "a", "hot", "").unwrap());
        assert_eq!(list_alerts(&p, false).unwrap().len(), 1);
        let id = list_alerts(&p, false).unwrap()[0].alert_id.clone();
        assert!(ack_alert(&p, &id).unwrap());
        assert_eq!(list_alerts(&p, false).unwrap().len(), 0);
        assert_eq!(list_alerts(&p, true).unwrap().len(), 1);
    }

    #[test]
    fn job_log_round_trips_as_lines() {
        let p = pool();
        let j = NasJob {
            job_id: "j1".into(),
            kind: "packages.install".into(),
            subject: "zfs".into(),
            status: "running".into(),
            progress_pct: None,
            started_by: "u1".into(),
            started_at: now(),
            finished_at: None,
            error: None,
            log: vec![],
        };
        insert_job(&p, &j).unwrap();
        append_job_log(&p, "j1", "first").unwrap();
        append_job_log(&p, "j1", "second").unwrap();
        finish_job(&p, "j1", "succeeded", None).unwrap();
        let got = job(&p, "j1").unwrap().unwrap();
        assert_eq!(got.log, vec!["first", "second"]);
        assert_eq!(got.progress_pct, Some(100));
        assert_eq!(fail_orphaned_jobs(&p).unwrap(), 0);
    }
}
