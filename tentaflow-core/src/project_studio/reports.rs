// ===== File: project_studio/reports.rs — aggregate SQL reports (F2, T14) =====
//
// One generic entry point (`run_report`) serving the five F2 reports as
// `rows_json` (schema per report, wire never changes for new report kinds).
// Every case-touching query shares `VISIBLE_CASES_PREDICATE`; the serialized
// payload is bounded to 200 KiB by dropping trailing rows.

use anyhow::{anyhow, bail, Result};
use rusqlite::params;

use super::tests::VISIBLE_CASES_PREDICATE;
use crate::db::DbPool;

/// Upper bound of the serialized `rows_json` payload.
pub const MAX_ROWS_JSON_BYTES: usize = 200 * 1024;

pub const REPORT_KINDS: &[&str] = &[
    "runs_over_time",
    "suite_pass_rate",
    "tester_stats",
    "source_coverage",
    "defects",
];

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio reports read: {e}")
}

/// Normalizes a `YYYY-MM-DD` filter bound; empty = unbounded. Anything else
/// is rejected before touching SQL.
fn validate_date(raw: &str) -> Result<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = raw.as_bytes();
    let shape_ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
    if !shape_ok {
        bail!("invalid date '{raw}' (expected YYYY-MM-DD)");
    }
    Ok(Some(raw.to_string()))
}

/// Executes one report and returns the serialized `rows_json`. Rows the
/// caller resolves display names for (tester_stats) carry raw user ids —
/// the dispatcher enriches them before sending.
pub fn run_report(
    pool: &DbPool,
    report: &str,
    from_date: &str,
    to_date: &str,
    suite_id: &str,
) -> Result<String> {
    let from = validate_date(from_date)?;
    let to = validate_date(to_date)?;
    let rows = match report {
        "runs_over_time" => runs_over_time(pool, from.as_deref(), to.as_deref())?,
        "suite_pass_rate" => suite_pass_rate(pool, suite_id)?,
        "tester_stats" => tester_stats(pool, from.as_deref(), to.as_deref())?,
        "source_coverage" => source_coverage(pool)?,
        "defects" => defects(pool)?,
        other => bail!("unknown report '{other}'"),
    };
    Ok(bounded_rows_json(rows))
}

/// Serializes rows, dropping trailing entries until the payload fits the
/// 200 KiB clamp.
fn bounded_rows_json(mut rows: Vec<serde_json::Value>) -> String {
    loop {
        let json = serde_json::Value::Array(rows.clone()).to_string();
        if json.len() <= MAX_ROWS_JSON_BYTES || rows.is_empty() {
            return json;
        }
        let keep = rows.len().saturating_sub((rows.len() / 4).max(1));
        rows.truncate(keep);
    }
}

/// Runs per day (`started_at` date) with aggregated item verdicts.
fn runs_over_time(
    pool: &DbPool,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT date(r.started_at) AS day, COUNT(DISTINCT r.run_id), \
                COALESCE(SUM(i.status = 'passed'), 0), \
                COALESCE(SUM(i.status = 'failed'), 0), \
                COALESCE(SUM(i.status = 'blocked'), 0), \
                COALESCE(SUM(i.status = 'skipped'), 0) \
         FROM test_runs r LEFT JOIN test_run_items i ON i.run_id = r.run_id \
         WHERE (?1 IS NULL OR date(r.started_at) >= ?1) \
           AND (?2 IS NULL OR date(r.started_at) <= ?2) \
         GROUP BY day ORDER BY day",
    )?;
    let rows = stmt.query_map(params![from, to], |row| {
        Ok(serde_json::json!({
            "date": row.get::<_, String>(0)?,
            "runs": row.get::<_, i64>(1)?,
            "passed": row.get::<_, i64>(2)?,
            "failed": row.get::<_, i64>(3)?,
            "blocked": row.get::<_, i64>(4)?,
            "skipped": row.get::<_, i64>(5)?,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Pass rate per suite over all its runs (optionally one suite).
fn suite_pass_rate(pool: &DbPool, suite_id: &str) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let filter: Option<&str> = if suite_id.is_empty() {
        None
    } else {
        Some(suite_id)
    };
    let mut stmt = conn.prepare(
        "SELECT s.suite_id, s.name, COUNT(DISTINCT r.run_id), \
                COALESCE(SUM(i.status = 'passed'), 0), \
                COALESCE(SUM(i.status = 'failed'), 0), \
                COALESCE(SUM(i.status = 'blocked'), 0), \
                COALESCE(SUM(i.status = 'skipped'), 0), \
                COALESCE(SUM(i.status IN ('passed','failed','blocked','skipped')), 0) \
         FROM test_suites s \
         LEFT JOIN test_runs r ON r.suite_id = s.suite_id \
         LEFT JOIN test_run_items i ON i.run_id = r.run_id \
         WHERE (?1 IS NULL OR s.suite_id = ?1) \
         GROUP BY s.suite_id ORDER BY s.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map(params![filter], |row| {
        let executed: i64 = row.get(7)?;
        let passed: i64 = row.get(3)?;
        let pass_rate = if executed > 0 {
            (passed as f64) * 100.0 / (executed as f64)
        } else {
            0.0
        };
        Ok(serde_json::json!({
            "suite_id": row.get::<_, String>(0)?,
            "suite_name": row.get::<_, String>(1)?,
            "runs": row.get::<_, i64>(2)?,
            "passed": passed,
            "failed": row.get::<_, i64>(4)?,
            "blocked": row.get::<_, i64>(5)?,
            "skipped": row.get::<_, i64>(6)?,
            "pass_rate": (pass_rate * 10.0).round() / 10.0,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Per-tester execution stats over finished items. `user_id` is raw — the
/// dispatcher resolves display names.
fn tester_stats(
    pool: &DbPool,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT assigned_to, COUNT(*), \
                COALESCE(SUM(status = 'passed'), 0), \
                COALESCE(SUM(status = 'failed'), 0), \
                COALESCE(SUM(status = 'blocked'), 0), \
                COALESCE(SUM(status = 'skipped'), 0), \
                COALESCE(AVG(NULLIF(duration_secs, 0)), 0) \
         FROM test_run_items \
         WHERE assigned_to <> '' AND status IN ('passed','failed','blocked','skipped') \
           AND (?1 IS NULL OR date(finished_at) >= ?1) \
           AND (?2 IS NULL OR date(finished_at) <= ?2) \
         GROUP BY assigned_to ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map(params![from, to], |row| {
        Ok(serde_json::json!({
            "user_id": row.get::<_, String>(0)?,
            "executed": row.get::<_, i64>(1)?,
            "passed": row.get::<_, i64>(2)?,
            "failed": row.get::<_, i64>(3)?,
            "blocked": row.get::<_, i64>(4)?,
            "skipped": row.get::<_, i64>(5)?,
            "avg_duration_secs": row.get::<_, f64>(6)?.round() as i64,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Knowledge-source coverage: how many visible cases link each source.
fn source_coverage(pool: &DbPool) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT s.source_id, s.name, \
                (SELECT COUNT(*) FROM test_cases c, json_each(c.linked_sources_json) j \
                 WHERE j.value = s.source_id AND c.{VISIBLE_CASES_PREDICATE}), \
                (SELECT COUNT(*) FROM test_cases c, json_each(c.linked_sources_json) j \
                 WHERE j.value = s.source_id AND c.status = 'approved' \
                   AND c.{VISIBLE_CASES_PREDICATE}) \
         FROM sources s ORDER BY s.name COLLATE NOCASE"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "source_id": row.get::<_, String>(0)?,
            "source_name": row.get::<_, String>(1)?,
            "cases_total": row.get::<_, i64>(2)?,
            "cases_approved": row.get::<_, i64>(3)?,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Defect matrix: count per (severity, status).
fn defects(pool: &DbPool) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT severity, status, COUNT(*) FROM tasks WHERE task_type = 'defect' \
         GROUP BY severity, status \
         ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 \
                  WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END, status",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "severity": row.get::<_, String>(0)?,
            "status": row.get::<_, String>(1)?,
            "count": row.get::<_, i64>(2)?,
        }))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
