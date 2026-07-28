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
    "perf_trend",
    "perf_compare",
    "tester_activity",
];

/// Performance runs kept in the trend chart. Older runs say little about the
/// current build and would only stretch the x-axis.
const PERF_TREND_LIMIT: usize = 30;

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
/// caller resolves display names for (tester_stats, tester_activity) carry raw
/// user ids — the dispatcher enriches them before sending.
pub fn run_report(
    pool: &DbPool,
    report: &str,
    from_date: &str,
    to_date: &str,
    suite_id: &str,
    run_ids: &[String],
) -> Result<String> {
    let from = validate_date(from_date)?;
    let to = validate_date(to_date)?;
    let rows = match report {
        "runs_over_time" => runs_over_time(pool, from.as_deref(), to.as_deref())?,
        "suite_pass_rate" => suite_pass_rate(pool, suite_id)?,
        "tester_stats" => tester_stats(pool, from.as_deref(), to.as_deref())?,
        "source_coverage" => source_coverage(pool)?,
        "defects" => defects(pool)?,
        "perf_trend" => perf_trend(pool, suite_id)?,
        "perf_compare" => perf_compare(pool, run_ids)?,
        "tester_activity" => tester_activity(pool, from.as_deref(), to.as_deref())?,
        other => bail!("unknown report '{other}'"),
    };
    Ok(bounded_rows_json(rows))
}

/// Serializes rows, dropping trailing entries until the payload fits the
/// 200 KiB clamp. Public because the dispatcher enriches some reports with
/// display names AFTER this point and has to re-apply the bound.
pub fn bounded_rows_json(mut rows: Vec<serde_json::Value>) -> String {
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

/// One perf run as stored: the header plus the runner's per-endpoint summary
/// and the load profile it ran under.
struct PerfRun {
    run_id: String,
    run_no: i64,
    started_at: String,
    endpoints: Vec<serde_json::Value>,
    users: u64,
}

/// `users` is a property of the LOAD, not of a single endpoint, so it lives in
/// the profile the run was submitted with.
fn profile_users(perf_profile_json: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(perf_profile_json)
        .ok()
        .and_then(|v| {
            v.get("users")
                .or_else(|| v.get("profile").and_then(|p| p.get("users")))
                .and_then(|u| u.as_u64())
        })
        .unwrap_or(0)
}

/// Row mapper shared by the two performance reports.
fn read_perf_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<PerfRun> {
    let summary: String = row.get(3)?;
    let profile: String = row.get(4)?;
    Ok(PerfRun {
        run_id: row.get(0)?,
        run_no: row.get(1)?,
        started_at: row.get(2)?,
        endpoints: serde_json::from_str::<Vec<serde_json::Value>>(&summary).unwrap_or_default(),
        users: profile_users(&profile),
    })
}

/// Only COMPLETED performance runs carry a comparable summary: a run still in
/// flight has partial percentiles, and a failed one has whatever it managed.
const PERF_RUN_COLS: &str = "SELECT r.run_id, r.run_no, r.started_at, m.perf_summary_json, \
     m.perf_profile_json FROM test_runs r JOIN auto_run_meta m ON m.run_id = r.run_id \
     WHERE r.run_type = 'perf' AND r.status = 'completed'";

fn perf_number(entry: &serde_json::Value, key: &str) -> f64 {
    entry.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Report 7a — percentile trend of the completed performance runs, one row per
/// (run, endpoint).
fn perf_trend(pool: &DbPool, suite_id: &str) -> Result<Vec<serde_json::Value>> {
    let filter: Option<&str> = if suite_id.is_empty() {
        None
    } else {
        Some(suite_id)
    };
    let conn = pool.read().map_err(read_err)?;
    // Newest first inside SQL so the LIMIT keeps the LATEST runs, then reversed
    // for the chart, which needs them chronologically.
    let mut stmt = conn.prepare(&format!(
        "{PERF_RUN_COLS} AND (?1 IS NULL OR r.suite_id = ?1) ORDER BY r.started_at DESC LIMIT ?2"
    ))?;
    let mut runs = stmt
        .query_map(params![filter, PERF_TREND_LIMIT as i64], read_perf_run)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    runs.reverse();
    let mut out = Vec::new();
    for run in runs {
        for entry in &run.endpoints {
            out.push(serde_json::json!({
                "run_id": run.run_id,
                "run_no": run.run_no,
                "started_at": run.started_at,
                "endpoint": entry.get("endpoint").and_then(|e| e.as_str()).unwrap_or_default(),
                "p50_ms": round1(perf_number(entry, "p50_ms")),
                "p90_ms": round1(perf_number(entry, "p90_ms")),
                "p99_ms": round1(perf_number(entry, "p99_ms")),
                "rps": round1(perf_number(entry, "rps")),
                "failures": perf_number(entry, "failures") as i64,
                "requests": perf_number(entry, "requests") as i64,
                "users": run.users,
            }));
        }
    }
    Ok(out)
}

/// Report 7b — endpoint-by-endpoint delta between exactly two runs. An endpoint
/// present in only one of them is reported as 'added'/'removed' rather than
/// compared against zeros, which would render as a meaningless -100%.
fn perf_compare(pool: &DbPool, run_ids: &[String]) -> Result<Vec<serde_json::Value>> {
    if run_ids.len() != 2 {
        bail!("perf_compare requires exactly two run ids");
    }
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "{PERF_RUN_COLS} AND r.run_id IN (?1, ?2)"
    ))?;
    let runs = stmt
        .query_map(params![run_ids[0], run_ids[1]], read_perf_run)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let pick = |wanted: &str| runs.iter().find(|r| r.run_id == wanted);
    let (Some(a), Some(b)) = (pick(&run_ids[0]), pick(&run_ids[1])) else {
        bail!("both runs must be completed performance runs");
    };

    let metrics = ["p50_ms", "p90_ms", "p99_ms", "rps", "failures", "requests"];
    let index = |run: &PerfRun| -> Vec<(String, serde_json::Value)> {
        run.endpoints
            .iter()
            .map(|e| {
                (
                    e.get("endpoint")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    e.clone(),
                )
            })
            .collect()
    };
    let a_rows = index(a);
    let b_rows = index(b);
    let mut endpoints: Vec<String> = a_rows.iter().map(|(k, _)| k.clone()).collect();
    for (endpoint, _) in &b_rows {
        if !endpoints.contains(endpoint) {
            endpoints.push(endpoint.clone());
        }
    }
    endpoints.sort();

    let mut out = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        let left = a_rows.iter().find(|(k, _)| *k == endpoint).map(|(_, v)| v);
        let right = b_rows.iter().find(|(k, _)| *k == endpoint).map(|(_, v)| v);
        let status = match (left, right) {
            (Some(_), Some(_)) => "compared",
            (Some(_), None) => "removed",
            (None, Some(_)) => "added",
            (None, None) => continue,
        };
        let values = |entry: Option<&serde_json::Value>| -> serde_json::Value {
            let mut map = serde_json::Map::new();
            for metric in metrics {
                let value = entry.map(|e| perf_number(e, metric)).unwrap_or(0.0);
                map.insert(metric.to_string(), serde_json::json!(round1(value)));
            }
            serde_json::Value::Object(map)
        };
        let mut delta = serde_json::Map::new();
        for metric in metrics {
            let before = left.map(|e| perf_number(e, metric)).unwrap_or(0.0);
            let after = right.map(|e| perf_number(e, metric)).unwrap_or(0.0);
            let pct = if status == "compared" && before != 0.0 {
                round1((after - before) / before * 100.0)
            } else {
                0.0
            };
            delta.insert(metric.to_string(), serde_json::json!(pct));
        }
        out.push(serde_json::json!({
            "endpoint": endpoint,
            "status": status,
            "a": values(left),
            "b": values(right),
            "delta_pct": serde_json::Value::Object(delta),
        }));
    }
    Ok(out)
}

/// Report 8 — per-tester activity by DAY, with the case approvals the tester
/// signed off in the same window (they come from the activity log, not from the
/// run items, so a reviewer with no executions still shows up).
fn tester_activity(
    pool: &DbPool,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let conn = pool.read().map_err(read_err)?;
    let mut rows: Vec<serde_json::Value> = {
        let mut stmt = conn.prepare(
            "SELECT assigned_to, date(finished_at) AS day, COUNT(*), \
                    COALESCE(SUM(status = 'passed'), 0), \
                    COALESCE(SUM(status = 'failed'), 0), \
                    COALESCE(SUM(status = 'blocked'), 0), \
                    COALESCE(SUM(status = 'skipped'), 0), \
                    COALESCE(AVG(NULLIF(duration_secs, 0)), 0) \
             FROM test_run_items \
             WHERE assigned_to <> '' AND finished_at IS NOT NULL \
               AND status IN ('passed','failed','blocked','skipped') \
               AND (?1 IS NULL OR date(finished_at) >= ?1) \
               AND (?2 IS NULL OR date(finished_at) <= ?2) \
             GROUP BY assigned_to, day ORDER BY day, assigned_to",
        )?;
        let mapped = stmt.query_map(params![from, to], |row| {
            Ok(serde_json::json!({
                "user_id": row.get::<_, String>(0)?,
                "day": row.get::<_, String>(1)?,
                "executed": row.get::<_, i64>(2)?,
                "passed": row.get::<_, i64>(3)?,
                "failed": row.get::<_, i64>(4)?,
                "blocked": row.get::<_, i64>(5)?,
                "skipped": row.get::<_, i64>(6)?,
                "avg_duration_secs": row.get::<_, f64>(7)?.round() as i64,
                "approvals": 0,
            }))
        })?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let approvals: Vec<(String, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT actor_user_id, date(created_at) AS day, COUNT(*) FROM activity_log \
             WHERE action = 'case.status_changed' \
               AND json_extract(details_json, '$.status') = 'approved' \
               AND (?1 IS NULL OR date(created_at) >= ?1) \
               AND (?2 IS NULL OR date(created_at) <= ?2) \
             GROUP BY actor_user_id, day",
        )?;
        let mapped = stmt.query_map(params![from, to], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        mapped.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (user_id, day, count) in approvals {
        let existing = rows.iter_mut().find(|r| {
            r.get("user_id").and_then(|v| v.as_str()) == Some(user_id.as_str())
                && r.get("day").and_then(|v| v.as_str()) == Some(day.as_str())
        });
        match existing {
            Some(row) => row["approvals"] = serde_json::json!(count),
            None => rows.push(serde_json::json!({
                "user_id": user_id,
                "day": day,
                "executed": 0,
                "passed": 0,
                "failed": 0,
                "blocked": 0,
                "skipped": 0,
                "avg_duration_secs": 0,
                "approvals": count,
            })),
        }
    }
    rows.sort_by(|a, b| {
        let key = |v: &serde_json::Value| {
            (
                v.get("day")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default()
                    .to_string(),
                v.get("user_id")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(rows)
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

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    /// Seeds one completed perf run with a runner summary and its load profile.
    fn seed_perf_run(pool: &DbPool, run_id: &str, run_no: i64, started_at: &str, summary: &str) {
        let conn = pool.write().expect("write");
        conn.execute(
            "INSERT INTO test_runs (run_id, run_no, name, run_type, assignment_mode, status, \
                started_at, created_by) \
             VALUES (?1, ?2, 'perf', 'perf', 'pool', 'completed', ?3, 'u1')",
            params![run_id, run_no, started_at],
        )
        .expect("insert run");
        conn.execute(
            "INSERT INTO auto_run_meta (run_id, perf_summary_json, perf_profile_json) \
             VALUES (?1, ?2, '{\"users\":50}')",
            params![run_id, summary],
        )
        .expect("insert meta");
    }

    fn rows(json: &str) -> Vec<serde_json::Value> {
        serde_json::from_str(json).expect("rows json")
    }

    /// perf_trend expands the runner summary into one row per (run, endpoint),
    /// in chronological order, and carries the load size from the profile.
    /// A run that is not a COMPLETED perf run never appears.
    #[test]
    fn perf_trend_expands_the_runner_summary() {
        let pool = pool();
        seed_perf_run(
            &pool,
            "r1",
            1,
            "2026-07-01T10:00:00Z",
            r#"[{"endpoint":"/api/login","requests":1000,"failures":3,"rps":42.5,
                 "p50_ms":80.0,"p90_ms":150.0,"p99_ms":410.0}]"#,
        );
        seed_perf_run(
            &pool,
            "r2",
            2,
            "2026-07-02T10:00:00Z",
            r#"[{"endpoint":"/api/login","requests":1200,"failures":1,"rps":50.0,
                 "p50_ms":70.0,"p90_ms":120.0,"p99_ms":300.0},
                {"endpoint":"/api/orders","requests":600,"failures":0,"rps":20.0,
                 "p50_ms":95.0,"p90_ms":180.0,"p99_ms":520.0}]"#,
        );
        {
            // A running perf run and a completed non-perf run are both excluded.
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, run_type, assignment_mode, \
                    created_by) VALUES ('r3', 3, 'w toku', 'perf', 'pool', 'u1')",
                [],
            )
            .expect("insert running");
            conn.execute(
                "INSERT INTO auto_run_meta (run_id, perf_summary_json) \
                 VALUES ('r3', '[{\"endpoint\":\"/x\"}]')",
                [],
            )
            .expect("insert meta");
        }

        let out = rows(&run_report(&pool, "perf_trend", "", "", "", &[]).expect("report"));
        assert_eq!(out.len(), 3, "two runs, three endpoint rows");
        assert_eq!(out[0]["run_no"], 1);
        assert_eq!(out[0]["endpoint"], "/api/login");
        assert_eq!(out[0]["p90_ms"], 150.0);
        assert_eq!(out[0]["failures"], 3);
        assert_eq!(out[0]["users"], 50, "load size comes from the profile");
        assert_eq!(out[1]["run_no"], 2, "rows are chronological");
        assert_eq!(out[2]["endpoint"], "/api/orders");
    }

    /// perf_compare puts both runs side by side with a percentage delta, and
    /// flags endpoints that exist in only one of them instead of comparing them
    /// against zeros.
    #[test]
    fn perf_compare_reports_deltas_and_added_removed_endpoints() {
        let pool = pool();
        seed_perf_run(
            &pool,
            "r1",
            1,
            "2026-07-01T10:00:00Z",
            r#"[{"endpoint":"/api/login","requests":1000,"failures":0,"rps":40.0,
                 "p50_ms":100.0,"p90_ms":200.0,"p99_ms":400.0},
                {"endpoint":"/api/legacy","requests":10,"failures":0,"rps":1.0,
                 "p50_ms":10.0,"p90_ms":20.0,"p99_ms":30.0}]"#,
        );
        seed_perf_run(
            &pool,
            "r2",
            2,
            "2026-07-02T10:00:00Z",
            r#"[{"endpoint":"/api/login","requests":1000,"failures":0,"rps":50.0,
                 "p50_ms":150.0,"p90_ms":180.0,"p99_ms":400.0},
                {"endpoint":"/api/new","requests":5,"failures":0,"rps":0.5,
                 "p50_ms":5.0,"p90_ms":9.0,"p99_ms":12.0}]"#,
        );

        let ids = vec!["r1".to_string(), "r2".to_string()];
        let out = rows(&run_report(&pool, "perf_compare", "", "", "", &ids).expect("report"));
        assert_eq!(out.len(), 3);

        let login = out
            .iter()
            .find(|r| r["endpoint"] == "/api/login")
            .expect("login row");
        assert_eq!(login["status"], "compared");
        assert_eq!(login["a"]["p50_ms"], 100.0);
        assert_eq!(login["b"]["p50_ms"], 150.0);
        assert_eq!(login["delta_pct"]["p50_ms"], 50.0);
        assert_eq!(login["delta_pct"]["p90_ms"], -10.0);
        assert_eq!(login["delta_pct"]["p99_ms"], 0.0);

        let legacy = out
            .iter()
            .find(|r| r["endpoint"] == "/api/legacy")
            .expect("legacy row");
        assert_eq!(legacy["status"], "removed");
        assert_eq!(legacy["b"]["p50_ms"], 0.0);
        assert_eq!(
            legacy["delta_pct"]["p50_ms"], 0.0,
            "a missing side is never reported as -100%"
        );
        assert_eq!(
            out.iter()
                .find(|r| r["endpoint"] == "/api/new")
                .expect("new row")["status"],
            "added"
        );

        // Anything other than exactly two runs is refused.
        assert!(run_report(&pool, "perf_compare", "", "", "", &["r1".to_string()]).is_err());
        assert!(run_report(
            &pool,
            "perf_compare",
            "",
            "",
            "",
            &["r1".to_string(), "r2".to_string(), "r1".to_string()]
        )
        .is_err());
        // A run that is not a completed perf run cannot be compared.
        assert!(run_report(
            &pool,
            "perf_compare",
            "",
            "",
            "",
            &["r1".to_string(), "nieznany".to_string()]
        )
        .is_err());
    }

    /// tester_activity groups executions per (tester, day) and folds in case
    /// approvals from the activity log — including a reviewer who executed
    /// nothing that day.
    #[test]
    fn tester_activity_joins_executions_with_approvals() {
        let pool = pool();
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, created_by) \
                 VALUES ('r1', 1, 'sprint', 'pool', 'u1')",
                [],
            )
            .expect("insert run");
            for (item, user, status, finished, secs) in [
                ("i1", "tester-a", "passed", "2026-07-01T09:00:00Z", 120),
                ("i2", "tester-a", "failed", "2026-07-01T10:00:00Z", 60),
                ("i3", "tester-a", "passed", "2026-07-02T09:00:00Z", 0),
                ("i4", "tester-b", "blocked", "2026-07-01T11:00:00Z", 30),
            ] {
                conn.execute(
                    "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, \
                        case_version, assigned_to, status, duration_secs, finished_at) \
                     VALUES (?1, 'r1', ?1, 'przypadek', 1, ?2, ?3, ?4, ?5)",
                    params![item, user, status, secs, finished],
                )
                .expect("insert item");
            }
            for (actor, created, details) in [
                ("tester-a", "2026-07-01T12:00:00Z", r#"{"status":"approved"}"#),
                ("tester-a", "2026-07-01T13:00:00Z", r#"{"status":"approved"}"#),
                ("tester-a", "2026-07-01T14:00:00Z", r#"{"status":"draft"}"#),
                ("reviewer", "2026-07-03T09:00:00Z", r#"{"status":"approved"}"#),
            ] {
                conn.execute(
                    "INSERT INTO activity_log (actor_user_id, action, object_type, \
                        details_json, created_at) \
                     VALUES (?1, 'case.status_changed', 'case', ?2, ?3)",
                    params![actor, details, created],
                )
                .expect("insert activity");
            }
        }

        let out = rows(&run_report(&pool, "tester_activity", "", "", "", &[]).expect("report"));
        let pick = |user: &str, day: &str| {
            out.iter()
                .find(|r| r["user_id"] == user && r["day"] == day)
                .cloned()
                .unwrap_or_else(|| panic!("missing row {user} {day}"))
        };

        let a1 = pick("tester-a", "2026-07-01");
        assert_eq!(a1["executed"], 2);
        assert_eq!(a1["passed"], 1);
        assert_eq!(a1["failed"], 1);
        assert_eq!(a1["avg_duration_secs"], 90);
        assert_eq!(a1["approvals"], 2, "only 'approved' transitions count");

        let a2 = pick("tester-a", "2026-07-02");
        assert_eq!(a2["executed"], 1);
        assert_eq!(
            a2["avg_duration_secs"], 0,
            "a zero duration is ignored by the average, not counted as zero"
        );
        assert_eq!(a2["approvals"], 0);

        let reviewer = pick("reviewer", "2026-07-03");
        assert_eq!(reviewer["executed"], 0);
        assert_eq!(reviewer["approvals"], 1);

        // The date filter bounds both sides of the report.
        let bounded = rows(
            &run_report(
                &pool,
                "tester_activity",
                "2026-07-02",
                "2026-07-02",
                "",
                &[],
            )
            .expect("report"),
        );
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0]["user_id"], "tester-a");
    }
}
