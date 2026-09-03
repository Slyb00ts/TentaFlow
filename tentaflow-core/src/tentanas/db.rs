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
use tentaflow_protocol::tentanas::{
    NasAlert, NasDiskSample, NasJob, NasNfsOptions, NasSchedule, NasShareAccess, NasShareUser,
    NasSmartSchedule, NasSmbOptions, NasSnapshotSchedule,
};

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
), (
    2,
    // Recurring work of the node and the pool throughput history. Nothing here
    // mirrors ZFS state: a schedule is an intention, and `zpool`/`zfs` stay the
    // only source of truth for what actually exists.
    "CREATE TABLE nas_scrub_schedules (
        pool TEXT PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT
    );
    CREATE TABLE nas_snapshot_schedules (
        schedule_id TEXT PRIMARY KEY,
        dataset TEXT NOT NULL UNIQUE,
        enabled INTEGER NOT NULL DEFAULT 0,
        recursive INTEGER NOT NULL DEFAULT 0,
        schedule_json TEXT NOT NULL,
        keep_frequent INTEGER NOT NULL DEFAULT 0,
        keep_hourly INTEGER NOT NULL DEFAULT 0,
        keep_daily INTEGER NOT NULL DEFAULT 0,
        keep_weekly INTEGER NOT NULL DEFAULT 0,
        keep_monthly INTEGER NOT NULL DEFAULT 0,
        last_run_at TEXT,
        last_result TEXT NOT NULL DEFAULT '',
        next_run_at TEXT
    );
    CREATE TABLE nas_pool_samples (
        pool TEXT NOT NULL,
        sampled_at TEXT NOT NULL,
        read_bps INTEGER NOT NULL DEFAULT 0,
        write_bps INTEGER NOT NULL DEFAULT 0,
        read_iops REAL NOT NULL DEFAULT 0,
        write_iops REAL NOT NULL DEFAULT 0,
        read_latency_ms REAL NOT NULL DEFAULT 0,
        write_latency_ms REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (pool, sampled_at)
    ) WITHOUT ROWID;",
), (
    3,
    // File shares and the local Samba accounts they grant to. This is the
    // DESIRED state: `/etc/samba/tentanas.conf` and the exports file are
    // generated from these rows on every change, never read back — one source
    // of truth, no drift (§3.4 "Persystencja po reboocie"). Passwords are not
    // here and never were: the helper hands them to `smbpasswd` and forgets.
    "CREATE TABLE nas_shares (
        share_id TEXT PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        protocol TEXT NOT NULL,
        source_path TEXT NOT NULL,
        dataset TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        fleet_mount INTEGER NOT NULL DEFAULT 1,
        options_json TEXT NOT NULL DEFAULT '{}',
        state TEXT NOT NULL DEFAULT 'disabled',
        state_detail TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE nas_share_users (
        name TEXT PRIMARY KEY,
        description TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL
    );
    CREATE TABLE nas_share_grants (
        share_id TEXT NOT NULL,
        user TEXT NOT NULL,
        mode TEXT NOT NULL,
        PRIMARY KEY (share_id, user)
    ) WITHOUT ROWID;
    CREATE INDEX nas_share_grants_user ON nas_share_grants(user);",
), (
    4,
    // The 30-day half of the disk history (§5.4, the charts labelled "30 dni").
    // Same columns as the minute table so one reader can concatenate both:
    // `nas_disk_samples` holds the last 48 h at minute resolution, this one
    // holds hourly rows for 30 days. Keeping 30 days of minutes instead would
    // be ~43k rows per disk for a chart that cannot draw them.
    "CREATE TABLE nas_disk_hourly (
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
    ) WITHOUT ROWID;",
)];

/// How far back a disk's health history reaches, and how much of it keeps
/// minute resolution. Both are answers the frontend labels its charts with,
/// so they live here rather than in three call sites.
pub const HISTORY_DAYS: u32 = 30;
const MINUTE_RETENTION_HOURS: i64 = 48;

/// The SMART self-test schedule is one JSON document, not a table: it has
/// exactly one row per node and no keys to index by.
pub const SETTING_SMART_SCHEDULE: &str = "smart_schedule";

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

/// Adds one to a numeric setting and returns the new value, in a single
/// statement: the privilege-channel counter is bumped from every request
/// handler and a read-modify-write would lose invocations under load.
pub fn bump_counter(pool: &DbPool, key: &str) -> Result<u64> {
    let conn = write(pool)?;
    let value: i64 = conn.query_row(
        "INSERT INTO nas_settings (key, value, updated_at) VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET
            value = CAST(CAST(value AS INTEGER) + 1 AS TEXT),
            updated_at = excluded.updated_at
         RETURNING CAST(value AS INTEGER)",
        params![key, now()],
        |r| r.get(0),
    )?;
    Ok(value.max(0) as u64)
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

fn sample_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasDiskSample> {
    Ok(NasDiskSample {
        at: r.get(0)?,
        temperature_c: r.get(1)?,
        reallocated_sectors: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        pending_sectors: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        read_bps: r.get::<_, i64>(4)? as u64,
        write_bps: r.get::<_, i64>(5)? as u64,
        await_ms: r.get(6)?,
    })
}

/// Minute samples of one disk since `since` (RFC 3339), oldest first.
pub fn samples_since(pool: &DbPool, disk_id: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_samples WHERE disk_id = ?1 AND at >= ?2 ORDER BY at",
    )?;
    let rows = stmt
        .query_map(params![disk_id, since], sample_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The whole history of one disk since `since`: hourly rows for everything
/// older than the minute window, minute rows after it. The two tables never
/// overlap — downsampling writes an hour's row in the same call that deletes
/// its minutes — so a plain concatenation is already ordered and gap-free.
pub fn history_since(pool: &DbPool, disk_id: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_hourly WHERE disk_id = ?1 AND at >= ?2
         UNION ALL
         SELECT at, temperature_c, reallocated, pending, read_bps, write_bps, await_ms
         FROM nas_disk_samples WHERE disk_id = ?1 AND at >= ?2
         ORDER BY at",
    )?;
    let rows = stmt
        .query_map(params![disk_id, since], sample_from_row)?
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
    // A week ago is outside the minute window, so the hourly table is where
    // the answer normally is; both are searched because the minute table still
    // holds everything on a node younger than the retention cutoff.
    Ok(conn
        .query_row(
            &format!(
                "SELECT {column} FROM (
                     SELECT at, {column} FROM nas_disk_hourly
                      WHERE disk_id = ?1 AND at <= ?2 AND {column} IS NOT NULL
                     UNION ALL
                     SELECT at, {column} FROM nas_disk_samples
                      WHERE disk_id = ?1 AND at <= ?2 AND {column} IS NOT NULL
                 ) ORDER BY at DESC LIMIT 1"
            ),
            params![disk_id, since],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Retention (§5.4): 48 h of minute samples, then hourly rows out to 30 days.
/// Downsampling and pruning are ONE call and one transaction on purpose — an
/// hour whose minutes were deleted before its hourly row was written would be
/// a permanent hole in the chart.
pub fn prune_samples(pool: &DbPool) -> Result<usize> {
    let mut conn = write(pool)?;
    let now = chrono::Utc::now();
    // Aligned to the hour, and the SAME boundary decides both statements: an
    // hour is downsampled only once it is entirely behind the window, so a
    // later run can never replace a full hour's row with the average of the
    // few minutes that had not crossed the boundary yet.
    let minute_cutoff = (now - chrono::Duration::hours(MINUTE_RETENTION_HOURS))
        .format("%Y-%m-%dT%H:00:00Z")
        .to_string();
    let history_cutoff = (now - chrono::Duration::days(i64::from(HISTORY_DAYS)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let tx = conn.transaction()?;
    // The counters are monotonic, so an hour's value is its MAX; throughput,
    // temperature and latency are averages of the minutes in it.
    tx.execute(
        "INSERT OR REPLACE INTO nas_disk_hourly
            (disk_id, at, temperature_c, reallocated, pending, crc_errors, media_errors,
             read_bps, write_bps, await_ms)
         SELECT disk_id,
                substr(at, 1, 13) || ':00:00Z',
                CAST(ROUND(AVG(temperature_c)) AS INTEGER),
                MAX(reallocated), MAX(pending), MAX(crc_errors), MAX(media_errors),
                CAST(ROUND(AVG(read_bps)) AS INTEGER),
                CAST(ROUND(AVG(write_bps)) AS INTEGER),
                AVG(await_ms)
           FROM nas_disk_samples
          WHERE at < ?1 AND at >= ?2
          GROUP BY disk_id, substr(at, 1, 13)",
        params![minute_cutoff, history_cutoff],
    )?;
    let dropped = tx.execute(
        "DELETE FROM nas_disk_samples WHERE at < ?1",
        params![minute_cutoff],
    )?;
    let expired = tx.execute(
        "DELETE FROM nas_disk_hourly WHERE at < ?1",
        params![history_cutoff],
    )?;
    tx.commit()?;
    Ok(dropped + expired)
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

// ----- schedules ----------------------------------------------------------------

/// The recurring scrub of one pool, as the Tasks tab and the pool card show it.
pub struct ScrubScheduleRow {
    pub pool: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
    pub last_run_at: Option<String>,
    pub last_result: String,
    pub next_run_at: Option<String>,
}

fn scrub_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScrubScheduleRow> {
    let json: String = r.get(2)?;
    Ok(ScrubScheduleRow {
        pool: r.get(0)?,
        enabled: r.get::<_, i64>(1)? != 0,
        schedule: serde_json::from_str(&json).unwrap_or_default(),
        last_run_at: r.get(3)?,
        last_result: r.get(4)?,
        next_run_at: r.get(5)?,
    })
}

const SCRUB_COLUMNS: &str = "pool, enabled, schedule_json, last_run_at, last_result, next_run_at";

pub fn scrub_schedule(pool: &DbPool, name: &str) -> Result<Option<ScrubScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {SCRUB_COLUMNS} FROM nas_scrub_schedules WHERE pool = ?1"),
            params![name],
            scrub_from_row,
        )
        .optional()?)
}

pub fn list_scrub_schedules(pool: &DbPool) -> Result<Vec<ScrubScheduleRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {SCRUB_COLUMNS} FROM nas_scrub_schedules ORDER BY pool"))?;
    let rows = stmt
        .query_map([], scrub_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_scrub_schedule(
    pool: &DbPool,
    name: &str,
    enabled: bool,
    schedule: &NasSchedule,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_scrub_schedules (pool, enabled, schedule_json, next_run_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(pool) DO UPDATE SET
            enabled = excluded.enabled,
            schedule_json = excluded.schedule_json,
            next_run_at = excluded.next_run_at",
        params![
            name,
            i64::from(enabled),
            serde_json::to_string(schedule)?,
            next_run_at
        ],
    )?;
    Ok(())
}

pub fn record_scrub_run(
    pool: &DbPool,
    name: &str,
    result: &str,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_scrub_schedules SET last_run_at = ?2, last_result = ?3, next_run_at = ?4
         WHERE pool = ?1",
        params![name, now(), result, next_run_at],
    )?;
    Ok(())
}

/// Drops the schedule of a pool that no longer exists (destroyed or exported).
pub fn delete_scrub_schedule(pool: &DbPool, name: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute("DELETE FROM nas_scrub_schedules WHERE pool = ?1", params![name])?;
    Ok(())
}

fn snapshot_schedule_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NasSnapshotSchedule> {
    let json: String = r.get(4)?;
    Ok(NasSnapshotSchedule {
        schedule_id: r.get(0)?,
        dataset: r.get(1)?,
        enabled: r.get::<_, i64>(2)? != 0,
        recursive: r.get::<_, i64>(3)? != 0,
        schedule: serde_json::from_str(&json).unwrap_or_default(),
        keep_frequent: r.get::<_, i64>(5)? as u32,
        keep_hourly: r.get::<_, i64>(6)? as u32,
        keep_daily: r.get::<_, i64>(7)? as u32,
        keep_weekly: r.get::<_, i64>(8)? as u32,
        keep_monthly: r.get::<_, i64>(9)? as u32,
        last_run_at: r.get(10)?,
        next_run_at: r.get(11)?,
        // Filled from the live snapshot list by the caller that has it.
        snapshot_count: 0,
    })
}

const SNAPSHOT_SCHEDULE_COLUMNS: &str =
    "schedule_id, dataset, enabled, recursive, schedule_json, keep_frequent, keep_hourly, \
     keep_daily, keep_weekly, keep_monthly, last_run_at, next_run_at";

pub fn list_snapshot_schedules(pool: &DbPool) -> Result<Vec<NasSnapshotSchedule>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SNAPSHOT_SCHEDULE_COLUMNS} FROM nas_snapshot_schedules ORDER BY dataset"
    ))?;
    let rows = stmt
        .query_map([], snapshot_schedule_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn snapshot_schedule(pool: &DbPool, schedule_id: &str) -> Result<Option<NasSnapshotSchedule>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!(
                "SELECT {SNAPSHOT_SCHEDULE_COLUMNS} FROM nas_snapshot_schedules \
                 WHERE schedule_id = ?1"
            ),
            params![schedule_id],
            snapshot_schedule_from_row,
        )
        .optional()?)
}

/// One schedule per dataset: the unique index enforces it, and setting a
/// schedule for a dataset that already has one replaces it in place.
pub fn upsert_snapshot_schedule(
    pool: &DbPool,
    schedule: &NasSnapshotSchedule,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_snapshot_schedules
            (schedule_id, dataset, enabled, recursive, schedule_json, keep_frequent, keep_hourly,
             keep_daily, keep_weekly, keep_monthly, next_run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(dataset) DO UPDATE SET
            enabled = excluded.enabled, recursive = excluded.recursive,
            schedule_json = excluded.schedule_json, keep_frequent = excluded.keep_frequent,
            keep_hourly = excluded.keep_hourly, keep_daily = excluded.keep_daily,
            keep_weekly = excluded.keep_weekly, keep_monthly = excluded.keep_monthly,
            next_run_at = excluded.next_run_at",
        params![
            schedule.schedule_id,
            schedule.dataset,
            i64::from(schedule.enabled),
            i64::from(schedule.recursive),
            serde_json::to_string(&schedule.schedule)?,
            i64::from(schedule.keep_frequent),
            i64::from(schedule.keep_hourly),
            i64::from(schedule.keep_daily),
            i64::from(schedule.keep_weekly),
            i64::from(schedule.keep_monthly),
            next_run_at
        ],
    )?;
    Ok(())
}

pub fn delete_snapshot_schedule(pool: &DbPool, schedule_id: &str) -> Result<bool> {
    let conn = write(pool)?;
    Ok(conn.execute(
        "DELETE FROM nas_snapshot_schedules WHERE schedule_id = ?1",
        params![schedule_id],
    )? == 1)
}

pub fn record_snapshot_run(
    pool: &DbPool,
    schedule_id: &str,
    result: &str,
    next_run_at: Option<&str>,
) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_snapshot_schedules SET last_run_at = ?2, last_result = ?3, next_run_at = ?4
         WHERE schedule_id = ?1",
        params![schedule_id, now(), result, next_run_at],
    )?;
    Ok(())
}

pub fn snapshot_schedule_result(pool: &DbPool, schedule_id: &str) -> Result<String> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT last_result FROM nas_snapshot_schedules WHERE schedule_id = ?1",
            params![schedule_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default())
}

pub fn smart_schedule(pool: &DbPool) -> Result<NasSmartSchedule> {
    Ok(setting(pool, SETTING_SMART_SCHEDULE)?
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default())
}

pub fn set_smart_schedule(pool: &DbPool, schedule: &NasSmartSchedule) -> Result<()> {
    set_setting(pool, SETTING_SMART_SCHEDULE, &serde_json::to_string(schedule)?)
}

// ----- pool samples -------------------------------------------------------------

pub struct PoolSampleInsert<'a> {
    pub pool: &'a str,
    pub sampled_at: &'a str,
    pub read_bps: u64,
    pub write_bps: u64,
    pub read_iops: f64,
    pub write_iops: f64,
    pub read_latency_ms: f64,
    pub write_latency_ms: f64,
}

pub fn insert_pool_samples(pool: &DbPool, samples: &[PoolSampleInsert<'_>]) -> Result<()> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_pool_samples
                (pool, sampled_at, read_bps, write_bps, read_iops, write_iops,
                 read_latency_ms, write_latency_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for s in samples {
            stmt.execute(params![
                s.pool,
                s.sampled_at,
                s.read_bps as i64,
                s.write_bps as i64,
                s.read_iops,
                s.write_iops,
                s.read_latency_ms,
                s.write_latency_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Pool throughput history in the shape the disk detail already uses, so the
/// dashboard draws both charts with one component. `temperature_c`,
/// `reallocated_sectors` and `pending_sectors` stay `None`: a pool has no
/// temperature and no sectors of its own — those belong to its member disks,
/// which the Disks tab charts separately. `await_ms` carries the combined
/// read/write service time, weighted by the IOPS of each direction.
pub fn pool_samples_since(pool: &DbPool, name: &str, since: &str) -> Result<Vec<NasDiskSample>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT sampled_at, read_bps, write_bps, read_iops, write_iops,
                read_latency_ms, write_latency_ms
         FROM nas_pool_samples WHERE pool = ?1 AND sampled_at >= ?2 ORDER BY sampled_at",
    )?;
    let rows = stmt
        .query_map(params![name, since], |r| {
            let read_iops: f64 = r.get(3)?;
            let write_iops: f64 = r.get(4)?;
            let read_latency: f64 = r.get(5)?;
            let write_latency: f64 = r.get(6)?;
            let ops = read_iops + write_iops;
            let await_ms = if ops > 0.0 {
                (read_iops * read_latency + write_iops * write_latency) / ops
            } else {
                0.0
            };
            Ok(NasDiskSample {
                at: r.get(0)?,
                temperature_c: None,
                reallocated_sectors: None,
                pending_sectors: None,
                read_bps: r.get::<_, i64>(1)? as u64,
                write_bps: r.get::<_, i64>(2)? as u64,
                await_ms: (await_ms * 100.0).round() / 100.0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pool samples are the live chart of the pool detail, not a health record:
/// 24 h is everything the view can show, so nothing older is worth its rows.
pub fn prune_pool_samples(pool: &DbPool) -> Result<usize> {
    let conn = write(pool)?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(24))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(conn.execute(
        "DELETE FROM nas_pool_samples WHERE sampled_at < ?1",
        params![cutoff],
    )?)
}

// ----- shares --------------------------------------------------------------------

/// One file share as the node wants it to be. `smb`/`nfs` mirror the protocol
/// column: exactly one is `Some`, and the SMB grants come from
/// `nas_share_grants` rather than from the JSON, so deleting a user is one
/// statement instead of a rewrite of every share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShareRow {
    pub share_id: String,
    pub name: String,
    pub protocol: String,
    pub source_path: String,
    pub dataset: Option<String>,
    pub enabled: bool,
    pub fleet_mount: bool,
    pub smb: Option<NasSmbOptions>,
    pub nfs: Option<NasNfsOptions>,
    pub state: String,
    pub state_detail: String,
    pub created_at: String,
    pub updated_at: String,
}

const SHARE_COLUMNS: &str = "share_id, name, protocol, source_path, dataset, enabled, \
                             fleet_mount, options_json, state, state_detail, created_at, updated_at";

fn share_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShareRow> {
    let protocol: String = r.get(2)?;
    let options: String = r.get(7)?;
    let (smb, nfs) = if protocol == "smb" {
        (Some(serde_json::from_str(&options).unwrap_or_default()), None)
    } else {
        (None, Some(serde_json::from_str(&options).unwrap_or_default()))
    };
    Ok(ShareRow {
        share_id: r.get(0)?,
        name: r.get(1)?,
        source_path: r.get(3)?,
        dataset: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        fleet_mount: r.get::<_, i64>(6)? != 0,
        smb,
        nfs,
        state: r.get(8)?,
        state_detail: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        protocol,
    })
}

/// The grants of one share, ordered so the generated `valid users` line is
/// stable — a config that reshuffles itself would reload smbd for nothing.
pub fn share_grants(pool: &DbPool, share_id: &str) -> Result<Vec<NasShareAccess>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT user, mode FROM nas_share_grants WHERE share_id = ?1 ORDER BY user",
    )?;
    let rows = stmt
        .query_map(params![share_id], |r| {
            Ok(NasShareAccess {
                user: r.get(0)?,
                mode: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fill_grants(pool: &DbPool, share: &mut ShareRow) -> Result<()> {
    if let Some(smb) = share.smb.as_mut() {
        smb.users = share_grants(pool, &share.share_id)?;
    }
    Ok(())
}

pub fn list_shares(pool: &DbPool) -> Result<Vec<ShareRow>> {
    let mut shares = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        let mut stmt =
            conn.prepare_cached(&format!("SELECT {SHARE_COLUMNS} FROM nas_shares ORDER BY name"))?;
        let rows = stmt
            .query_map([], share_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for share in shares.iter_mut() {
        fill_grants(pool, share)?;
    }
    Ok(shares)
}

pub fn share(pool: &DbPool, share_id: &str) -> Result<Option<ShareRow>> {
    let row = {
        let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
        conn.query_row(
            &format!("SELECT {SHARE_COLUMNS} FROM nas_shares WHERE share_id = ?1"),
            params![share_id],
            share_from_row,
        )
        .optional()?
    };
    match row {
        Some(mut share) => {
            fill_grants(pool, &mut share)?;
            Ok(Some(share))
        }
        None => Ok(None),
    }
}

pub fn share_by_name(pool: &DbPool, name: &str) -> Result<Option<ShareRow>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            &format!("SELECT {SHARE_COLUMNS} FROM nas_shares WHERE name = ?1"),
            params![name],
            share_from_row,
        )
        .optional()?)
}

fn options_json(share: &ShareRow) -> Result<String> {
    // The grants live in their own table; storing them twice would let the two
    // copies disagree the moment a user is deleted.
    Ok(match (&share.smb, &share.nfs) {
        (Some(smb), _) => serde_json::to_string(&NasSmbOptions {
            users: Vec::new(),
            ..smb.clone()
        })?,
        (_, Some(nfs)) => serde_json::to_string(nfs)?,
        _ => "{}".to_string(),
    })
}

/// Writes the share and replaces its grants in one transaction: a share whose
/// section names a user that is not in `nas_share_grants` would export access
/// nobody granted.
pub fn upsert_share(pool: &DbPool, share: &ShareRow) -> Result<()> {
    let options = options_json(share)?;
    let grants = share
        .smb
        .as_ref()
        .map(|s| s.users.clone())
        .unwrap_or_default();
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO nas_shares (share_id, name, protocol, source_path, dataset, enabled,
                                 fleet_mount, options_json, state, state_detail, created_at,
                                 updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(share_id) DO UPDATE SET
            source_path = excluded.source_path, dataset = excluded.dataset,
            enabled = excluded.enabled, fleet_mount = excluded.fleet_mount,
            options_json = excluded.options_json, state = excluded.state,
            state_detail = excluded.state_detail, updated_at = excluded.updated_at",
        params![
            share.share_id,
            share.name,
            share.protocol,
            share.source_path,
            share.dataset,
            i64::from(share.enabled),
            i64::from(share.fleet_mount),
            options,
            share.state,
            share.state_detail,
            share.created_at,
            share.updated_at
        ],
    )?;
    tx.execute(
        "DELETE FROM nas_share_grants WHERE share_id = ?1",
        params![share.share_id],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO nas_share_grants (share_id, user, mode) VALUES (?1, ?2, ?3)",
        )?;
        for grant in &grants {
            stmt.execute(params![share.share_id, grant.user, grant.mode])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn set_share_state(pool: &DbPool, share_id: &str, state: &str, detail: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "UPDATE nas_shares SET state = ?2, state_detail = ?3 WHERE share_id = ?1",
        params![share_id, state, detail],
    )?;
    Ok(())
}

pub fn delete_share(pool: &DbPool, share_id: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM nas_share_grants WHERE share_id = ?1",
        params![share_id],
    )?;
    let removed = tx.execute("DELETE FROM nas_shares WHERE share_id = ?1", params![share_id])?;
    tx.commit()?;
    Ok(removed == 1)
}

/// Total shares and how many of them the last apply left in `error` — the two
/// numbers the fleet row of this node carries.
pub fn share_counts(pool: &DbPool) -> Result<(u32, u32)> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(state = 'error'), 0) FROM nas_shares",
        [],
        |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u32)),
    )?)
}

// ----- share users ---------------------------------------------------------------

pub fn list_share_users(pool: &DbPool) -> Result<Vec<NasShareUser>> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT u.name, u.description, u.created_at,
                COALESCE(GROUP_CONCAT(s.name, char(10)), '')
         FROM nas_share_users u
         LEFT JOIN nas_share_grants g ON g.user = u.name
         LEFT JOIN nas_shares s ON s.share_id = g.share_id
         GROUP BY u.name ORDER BY u.name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let shares: String = r.get(3)?;
            Ok(NasShareUser {
                name: r.get(0)?,
                description: r.get(1)?,
                created_at: r.get(2)?,
                shares: shares.lines().map(str::to_string).collect(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn share_user_exists(pool: &DbPool, name: &str) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM nas_share_users WHERE name = ?1",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

pub fn upsert_share_user(pool: &DbPool, name: &str, description: &str) -> Result<()> {
    let conn = write(pool)?;
    conn.execute(
        "INSERT INTO nas_share_users (name, description, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET description = excluded.description",
        params![name, description, now()],
    )?;
    Ok(())
}

/// Removes the user and every grant naming it — a grant to an account that no
/// longer exists would keep appearing in the generated `valid users` line.
pub fn delete_share_user(pool: &DbPool, name: &str) -> Result<bool> {
    let mut conn = write(pool)?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM nas_share_grants WHERE user = ?1", params![name])?;
    let removed = tx.execute("DELETE FROM nas_share_users WHERE name = ?1", params![name])?;
    tx.commit()?;
    Ok(removed == 1)
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
        assert_eq!(n, 13);
    }

    #[test]
    fn retention_rolls_minutes_into_hours_and_drops_what_is_past_the_window() {
        let p = pool();
        let now = chrono::Utc::now();
        let at = |ago: chrono::Duration| {
            (now - ago).format("%Y-%m-%dT%H:%M:00Z").to_string()
        };
        // Two minutes of ONE hour that is entirely past the 48 h window (both
        // pinned to the same hour, so the assertion below does not depend on
        // what minute the test happens to run at), one minute inside the
        // window, and one sample older than the 30-day history.
        let old_hour = (now - chrono::Duration::hours(50))
            .format("%Y-%m-%dT%H")
            .to_string();
        let old_a = format!("{old_hour}:10:00Z");
        let old_b = format!("{old_hour}:20:00Z");
        let fresh = at(chrono::Duration::minutes(5));
        let ancient = at(chrono::Duration::days(40));
        let rows: [(&str, i32, u64, u64); 4] = [
            (&old_a, 40, 1000, 4),
            (&old_b, 44, 3000, 6),
            (&fresh, 41, 7000, 2),
            (&ancient, 39, 100, 1),
        ];
        let samples: Vec<SampleInsert<'_>> = rows
            .iter()
            .map(|&(at, temp, bps, realloc)| SampleInsert {
                disk_id: "d1",
                at,
                temperature_c: Some(temp),
                reallocated: Some(realloc),
                pending: None,
                crc_errors: None,
                media_errors: None,
                read_bps: bps,
                write_bps: 0,
                await_ms: 1.0,
            })
            .collect();
        insert_samples(&p, &samples).unwrap();
        prune_samples(&p).unwrap();

        // The minute table keeps only the fresh sample…
        let minutes = samples_since(&p, "d1", "1970-01-01T00:00:00Z").unwrap();
        assert_eq!(minutes.len(), 1);
        assert_eq!(minutes[0].at, fresh);

        // …the whole history still reaches back past the minute window, with
        // the two old minutes averaged into one hourly row.
        let history = history_since(&p, "d1", "1970-01-01T00:00:00Z").unwrap();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].read_bps, 2000);
        assert_eq!(history[0].temperature_c, Some(42));
        // A monotonic counter takes the hour's maximum, never its average.
        assert_eq!(history[0].reallocated_sectors, Some(6));
        assert_eq!(history[1].at, fresh);
        // The 40-day-old sample is gone from both tables, not downsampled.
        assert!(history.iter().all(|s| s.at != ancient));
    }

    #[test]
    fn a_share_keeps_its_grants_in_their_own_table() {
        let p = pool();
        let mut share = ShareRow {
            share_id: "s1".into(),
            name: "projekty".into(),
            protocol: "smb".into(),
            source_path: "/mnt/tank/projekty".into(),
            dataset: Some("tank/projekty".into()),
            enabled: true,
            fleet_mount: true,
            smb: Some(NasSmbOptions {
                guests: false,
                previous_versions: true,
                recycle_bin: true,
                time_machine: false,
                users: vec![
                    NasShareAccess {
                        user: "anna".into(),
                        mode: "rw".into(),
                    },
                    NasShareAccess {
                        user: "jan".into(),
                        mode: "ro".into(),
                    },
                ],
            }),
            nfs: None,
            state: "active".into(),
            state_detail: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        upsert_share_user(&p, "anna", "projekt lead").unwrap();
        upsert_share_user(&p, "jan", "").unwrap();
        upsert_share(&p, &share).unwrap();
        let back = list_shares(&p).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].smb.as_ref().unwrap().users.len(), 2);
        assert!(back[0].smb.as_ref().unwrap().previous_versions);
        // The options blob never carries the grants, so there is one truth.
        let raw: String = p
            .read()
            .unwrap()
            .query_row("SELECT options_json FROM nas_shares", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("anna"), "{raw}");

        // A rewrite replaces the grants instead of adding to them.
        share.smb.as_mut().unwrap().users.pop();
        upsert_share(&p, &share).unwrap();
        assert_eq!(share_grants(&p, "s1").unwrap().len(), 1);

        let users = list_share_users(&p).unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "anna");
        assert_eq!(users[0].shares, vec!["projekty".to_string()]);
        assert!(users[1].shares.is_empty(), "jan lost his grant");

        // Deleting a user takes its grants with it.
        assert!(delete_share_user(&p, "anna").unwrap());
        assert!(share_grants(&p, "s1").unwrap().is_empty());
        assert!(!delete_share_user(&p, "anna").unwrap());

        set_share_state(&p, "s1", "error", "source path is not mounted").unwrap();
        assert_eq!(share_counts(&p).unwrap(), (1, 1));
        assert!(delete_share(&p, "s1").unwrap());
        assert_eq!(share_counts(&p).unwrap(), (0, 0));
        assert!(share_by_name(&p, "projekty").unwrap().is_none());
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
            kind: "packages_install".into(),
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

    fn weekly() -> NasSchedule {
        NasSchedule {
            every: "weekly".into(),
            hour: 2,
            minute: 0,
            weekday: 0,
            day: 1,
        }
    }

    #[test]
    fn a_scrub_schedule_survives_a_rewrite_and_records_its_runs() {
        let p = pool();
        assert!(scrub_schedule(&p, "tank").unwrap().is_none());
        set_scrub_schedule(&p, "tank", true, &weekly(), Some("2026-09-06T00:00:00Z")).unwrap();
        let row = scrub_schedule(&p, "tank").unwrap().unwrap();
        assert!(row.enabled);
        assert_eq!(row.schedule, weekly());
        assert_eq!(row.next_run_at.as_deref(), Some("2026-09-06T00:00:00Z"));
        assert!(row.last_run_at.is_none());

        record_scrub_run(&p, "tank", "started job j1", Some("2026-09-13T00:00:00Z")).unwrap();
        let row = scrub_schedule(&p, "tank").unwrap().unwrap();
        assert_eq!(row.last_result, "started job j1");
        assert!(row.last_run_at.is_some());
        assert_eq!(row.next_run_at.as_deref(), Some("2026-09-13T00:00:00Z"));

        // Disabling rewrites the same row rather than adding a second one.
        set_scrub_schedule(&p, "tank", false, &weekly(), None).unwrap();
        assert_eq!(list_scrub_schedules(&p).unwrap().len(), 1);
        assert!(!list_scrub_schedules(&p).unwrap()[0].enabled);

        delete_scrub_schedule(&p, "tank").unwrap();
        assert!(list_scrub_schedules(&p).unwrap().is_empty());
    }

    #[test]
    fn one_snapshot_schedule_per_dataset() {
        let p = pool();
        let mut s = NasSnapshotSchedule {
            schedule_id: "s1".into(),
            dataset: "tank/projekty".into(),
            enabled: true,
            recursive: true,
            schedule: NasSchedule {
                every: "15m".into(),
                ..Default::default()
            },
            keep_frequent: 96,
            keep_daily: 30,
            keep_monthly: 12,
            ..Default::default()
        };
        upsert_snapshot_schedule(&p, &s, Some("2026-09-01T14:45:00Z")).unwrap();
        // A second write for the same dataset replaces the first.
        s.schedule_id = "s2".into();
        s.keep_frequent = 48;
        upsert_snapshot_schedule(&p, &s, None).unwrap();
        let all = list_snapshot_schedules(&p).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].schedule_id, "s1", "the dataset keeps its original id");
        assert_eq!(all[0].keep_frequent, 48);
        assert_eq!(all[0].keep_daily, 30);
        assert!(all[0].recursive);

        record_snapshot_run(&p, "s1", "ok", Some("2026-09-01T15:00:00Z")).unwrap();
        assert_eq!(snapshot_schedule_result(&p, "s1").unwrap(), "ok");
        assert!(snapshot_schedule(&p, "s1").unwrap().unwrap().last_run_at.is_some());
        assert!(delete_snapshot_schedule(&p, "s1").unwrap());
        assert!(!delete_snapshot_schedule(&p, "s1").unwrap());
    }

    #[test]
    fn pool_samples_fold_latency_into_the_shared_sample_shape() {
        let p = pool();
        insert_pool_samples(
            &p,
            &[PoolSampleInsert {
                pool: "tank",
                sampled_at: "2026-09-01T14:45:00Z",
                read_bps: 335_544_320,
                write_bps: 146_800_640,
                read_iops: 1420.0,
                write_iops: 420.0,
                read_latency_ms: 2.8,
                write_latency_ms: 6.1,
            }],
        )
        .unwrap();
        let rows = pool_samples_since(&p, "tank", "2026-09-01T00:00:00Z").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].read_bps, 335_544_320);
        // (1420*2.8 + 420*6.1) / 1840 = 3.55…
        assert!((rows[0].await_ms - 3.55).abs() < 0.01, "{}", rows[0].await_ms);
        // A pool has no temperature and no sectors of its own.
        assert_eq!(rows[0].temperature_c, None);
        assert_eq!(rows[0].reallocated_sectors, None);
        assert!(pool_samples_since(&p, "tank", "2026-09-02T00:00:00Z").unwrap().is_empty());
        // Everything in the fixture is older than 24 h from now.
        assert_eq!(prune_pool_samples(&p).unwrap(), 1);
    }

    #[test]
    fn the_smart_schedule_defaults_to_disabled_and_round_trips() {
        let p = pool();
        let empty = smart_schedule(&p).unwrap();
        assert!(!empty.enabled);
        let smart = NasSmartSchedule {
            enabled: true,
            short: NasSchedule {
                every: "daily".into(),
                hour: 1,
                ..Default::default()
            },
            long: NasSchedule {
                every: "monthly".into(),
                hour: 1,
                minute: 30,
                day: 1,
                ..Default::default()
            },
            next_short_at: Some("2026-09-02T01:00:00Z".into()),
            ..Default::default()
        };
        set_smart_schedule(&p, &smart).unwrap();
        assert_eq!(smart_schedule(&p).unwrap(), smart);
    }
}
