// ===== File: project_studio/schedules.rs — run schedules + the firing loop (F4, T13) =====
//
// A schedule is a stored run definition plus a firing rule ('once' RFC3339,
// 'interval' like `30m`, or a daily `minute hour * * *` cron evaluated in an
// IANA timezone). The background loop ticks every 30 s, but it never opens a
// per-project database speculatively: the central registry keeps a
// `project_schedule_hints` row per project, so one query decides which
// projects are due. That matters because the per-project pool cache holds at
// most 16 databases — a loop opening every project each tick would thrash the
// LRU and starve the rest of the module.
//
// Cron is evaluated with real tz rules (chrono-tz), NOT a fixed offset: a
// nightly 02:30 job must stay at 02:30 local across a DST switch. The two
// pathological cases are resolved explicitly — an ambiguous local time (clocks
// go back, 02:30 happens twice) fires at the EARLIER instant, and a
// nonexistent one (clocks go forward, 02:30 never happens) fires an hour later.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{params, OptionalExtension};

use super::models::{ProjectF4Kpis, ProjectRecord, ScheduleRecord, ScheduleRunRecord};
use crate::crypto::SettingsCipher;
use crate::db::DbPool;

/// Upper bound of schedules per project — the list screen is a flat table and
/// every enabled row costs a due-check on each tick.
pub const MAX_SCHEDULES_PER_PROJECT: u32 = 20;
/// Consecutive failing triggers after which the schedule stops itself. Resume
/// is manual only: something is structurally broken, and retrying forever just
/// buries the cause under noise.
pub const BREAKER_THRESHOLD: u32 = 5;
/// Automated runs a single project may have in flight before a scheduled
/// trigger backs off.
const MAX_ACTIVE_AUTO_RUNS: i64 = 3;
/// Shortest interval a schedule may use.
const MIN_INTERVAL_SECS: i64 = 300;
/// Longest interval a schedule may use.
const MAX_INTERVAL_SECS: i64 = 365 * 86_400;
/// Loop period.
const TICK_SECS: u64 = 30;
/// Projects inspected per tick.
const MAX_DUE_PROJECTS: usize = 10;
/// Schedules fired per project per tick.
const MAX_DUE_SCHEDULES: usize = 5;
/// Cases a schedule may select.
const MAX_SCHEDULE_CASES: usize = 200;

pub const RUN_TYPES: &[&str] = &["manual", "auto", "perf"];
pub const SCHEDULE_KINDS: &[&str] = &["once", "interval", "cron"];

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio schedules read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio schedules write: {e}")
}

// =============================================================================
// Firing rule arithmetic
// =============================================================================

/// Resolves an IANA timezone name; empty means UTC.
pub fn parse_timezone(name: &str) -> Result<Tz> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(Tz::UTC);
    }
    Tz::from_str(name).map_err(|_| anyhow!("unknown timezone '{name}'"))
}

/// The node's own IANA zone, for the "next run" hints in the UI. Resolved from
/// `TZ` and then from the `/etc/localtime` symlink (the two places an operating
/// system records the name rather than just the offset); an offset alone would
/// be wrong half the year, so anything unresolvable reports UTC.
pub fn server_timezone() -> String {
    if let Ok(name) = std::env::var("TZ") {
        let name = name.trim_start_matches(':');
        if parse_timezone(name).is_ok() && !name.is_empty() {
            return name.to_string();
        }
    }
    #[cfg(unix)]
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy();
        if let Some(idx) = path.find("zoneinfo/") {
            let name = &path[idx + "zoneinfo/".len()..];
            if parse_timezone(name).is_ok() {
                return name.to_string();
            }
        }
    }
    "UTC".to_string()
}

pub fn parse_interval_secs(expr: &str) -> Result<i64> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        bail!("interval is required");
    }
    let (num, mult) = match trimmed.chars().last().unwrap_or('s') {
        's' => (&trimmed[..trimmed.len() - 1], 1),
        'm' => (&trimmed[..trimmed.len() - 1], 60),
        'h' => (&trimmed[..trimmed.len() - 1], 3600),
        'd' => (&trimmed[..trimmed.len() - 1], 86_400),
        c if c.is_ascii_digit() => (trimmed, 1),
        _ => bail!("interval must use the s, m, h or d suffix"),
    };
    let value: i64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow!("invalid interval '{trimmed}'"))?;
    let secs = value.saturating_mul(mult);
    if secs < MIN_INTERVAL_SECS {
        bail!("the shortest interval is 5 minutes");
    }
    if secs > MAX_INTERVAL_SECS {
        bail!("the longest interval is 365 days");
    }
    Ok(secs)
}

/// Parses the daily `minute hour * * *` cron subset.
fn parse_daily_cron(expr: &str) -> Result<NaiveTime> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 || parts[2] != "*" || parts[3] != "*" || parts[4] != "*" {
        bail!("only a daily 'minute hour * * *' expression is supported");
    }
    let minute: u32 = parts[0]
        .parse()
        .map_err(|_| anyhow!("invalid cron minute '{}'", parts[0]))?;
    let hour: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("invalid cron hour '{}'", parts[1]))?;
    NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(|| anyhow!("invalid cron time"))
}

/// Maps a wall-clock instant in `tz` to UTC across DST discontinuities.
/// Ambiguous (clocks go back) resolves to the EARLIER instant so the job still
/// runs on the day it was scheduled for; nonexistent (clocks go forward) is
/// retried an hour later, which is the first wall-clock time that exists.
fn local_to_utc(tz: Tz, date: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _later) => Some(earlier.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            let shifted = naive + ChronoDuration::hours(1);
            match tz.from_local_datetime(&shifted) {
                chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
                chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
                chrono::LocalResult::None => None,
            }
        }
    }
}

/// Next fire instant strictly after `now`, or `None` for a rule that will not
/// fire again (a one-shot whose instant has passed).
pub fn compute_next_run(
    kind: &str,
    expr: &str,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    match kind {
        "once" => {
            let at = DateTime::parse_from_rfc3339(expr.trim())
                .map_err(|_| anyhow!("a one-shot schedule needs an RFC3339 instant"))?
                .with_timezone(&Utc);
            Ok(if at > now { Some(at) } else { None })
        }
        "interval" => Ok(Some(
            now + ChronoDuration::seconds(parse_interval_secs(expr)?),
        )),
        "cron" => {
            let tz = parse_timezone(timezone)?;
            let time = parse_daily_cron(expr)?;
            let local_today = now.with_timezone(&tz).date_naive();
            // Up to three days: a DST gap can push the first candidate past the
            // next day's wall clock, and a zone can have no valid instant at all
            // for one calendar day.
            for offset in 0..3 {
                let date = local_today + ChronoDuration::days(offset);
                if let Some(candidate) = local_to_utc(tz, date, time) {
                    if candidate > now {
                        return Ok(Some(candidate));
                    }
                }
            }
            bail!("cron expression yields no future instant in '{timezone}'")
        }
        other => bail!("unknown schedule kind '{other}'"),
    }
}

/// Server-rendered preview of the next `count` fire instants. The UI must never
/// recompute this: around a DST switch its arithmetic and the loop's would
/// disagree, and the loop is the one that actually fires.
pub fn next_runs_preview(
    kind: &str,
    expr: &str,
    timezone: &str,
    now: DateTime<Utc>,
    count: usize,
) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    let mut cursor = now;
    for _ in 0..count {
        match compute_next_run(kind, expr, timezone, cursor) {
            Ok(Some(next)) => {
                out.push(next.to_rfc3339());
                cursor = next;
            }
            _ => break,
        }
    }
    out
}

// =============================================================================
// SQL: schedules
// =============================================================================

const SCHEDULE_COLS: &str = "schedule_id, name, enabled, auto_disabled, run_type, suite_id, \
     case_ids_json, environment_id, runner_service_id, perf_profile_json, assignment_mode, \
     assignees_json, schedule_kind, schedule_expr, timezone, next_run_at, last_trigger_at, \
     last_run_id, last_status, last_reason, consecutive_failures, created_by, created_at, \
     updated_at";

fn read_schedule(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRecord> {
    Ok(ScheduleRecord {
        schedule_id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        auto_disabled: row.get::<_, i64>(3)? != 0,
        run_type: row.get(4)?,
        suite_id: row.get(5)?,
        case_ids_json: row.get(6)?,
        environment_id: row.get(7)?,
        runner_service_id: row.get(8)?,
        perf_profile_json: row.get(9)?,
        assignment_mode: row.get(10)?,
        assignees_json: row.get(11)?,
        schedule_kind: row.get(12)?,
        schedule_expr: row.get(13)?,
        timezone: row.get(14)?,
        next_run_at: row.get(15)?,
        last_trigger_at: row.get(16)?,
        last_run_id: row.get(17)?,
        last_status: row.get(18)?,
        last_reason: row.get(19)?,
        consecutive_failures: row.get::<_, i64>(20)? as u32,
        created_by: row.get(21)?,
        created_at: row.get(22)?,
        updated_at: row.get(23)?,
    })
}

pub fn list(pool: &DbPool) -> Result<Vec<ScheduleRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SCHEDULE_COLS} FROM schedules \
         ORDER BY enabled DESC, next_run_at IS NULL, next_run_at, name COLLATE NOCASE"
    ))?;
    let rows = stmt.query_map([], read_schedule)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get(pool: &DbPool, schedule_id: &str) -> Result<Option<ScheduleRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {SCHEDULE_COLS} FROM schedules WHERE schedule_id = ?1"),
        params![schedule_id],
        read_schedule,
    )
    .optional()
    .map_err(Into::into)
}

pub fn count(pool: &DbPool) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM schedules", [], |row| row.get(0))?;
    Ok(n as u32)
}

/// Field payload of a schedule create/update, already validated.
#[derive(Debug)]
pub struct ScheduleInput<'a> {
    pub name: &'a str,
    pub run_type: &'a str,
    pub suite_id: &'a str,
    pub case_ids_json: &'a str,
    pub environment_id: &'a str,
    pub runner_service_id: &'a str,
    pub perf_profile_json: &'a str,
    pub assignment_mode: &'a str,
    pub assignees_json: &'a str,
    pub schedule_kind: &'a str,
    pub schedule_expr: &'a str,
    pub timezone: &'a str,
    pub enabled: bool,
}

/// Inserts a schedule. `next_run_at` is `None` for a disabled one — it must not
/// be selectable by the due query until it is switched on.
pub fn insert(
    pool: &DbPool,
    schedule_id: &str,
    input: &ScheduleInput<'_>,
    next_run_at: Option<&str>,
    created_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO schedules (schedule_id, name, enabled, run_type, suite_id, case_ids_json, \
            environment_id, runner_service_id, perf_profile_json, assignment_mode, \
            assignees_json, schedule_kind, schedule_expr, timezone, next_run_at, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            schedule_id,
            input.name,
            i64::from(input.enabled),
            input.run_type,
            input.suite_id,
            input.case_ids_json,
            input.environment_id,
            input.runner_service_id,
            input.perf_profile_json,
            input.assignment_mode,
            input.assignees_json,
            input.schedule_kind,
            input.schedule_expr,
            input.timezone,
            next_run_at,
            created_by
        ],
    )?;
    Ok(())
}

/// Full-field update. Editing a schedule clears the breaker: the definition the
/// breaker reacted to no longer exists.
pub fn update(
    pool: &DbPool,
    schedule_id: &str,
    input: &ScheduleInput<'_>,
    next_run_at: Option<&str>,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE schedules SET name = ?1, enabled = ?2, auto_disabled = 0, run_type = ?3, \
            suite_id = ?4, case_ids_json = ?5, environment_id = ?6, runner_service_id = ?7, \
            perf_profile_json = ?8, assignment_mode = ?9, assignees_json = ?10, \
            schedule_kind = ?11, schedule_expr = ?12, timezone = ?13, next_run_at = ?14, \
            consecutive_failures = 0, updated_at = datetime('now') \
         WHERE schedule_id = ?15",
        params![
            input.name,
            i64::from(input.enabled),
            input.run_type,
            input.suite_id,
            input.case_ids_json,
            input.environment_id,
            input.runner_service_id,
            input.perf_profile_json,
            input.assignment_mode,
            input.assignees_json,
            input.schedule_kind,
            input.schedule_expr,
            input.timezone,
            next_run_at,
            schedule_id
        ],
    )?;
    Ok(n > 0)
}

pub fn delete(pool: &DbPool, schedule_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM schedule_runs WHERE schedule_id = ?1",
        params![schedule_id],
    )?;
    let n = tx.execute(
        "DELETE FROM schedules WHERE schedule_id = ?1",
        params![schedule_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// Enable/disable toggle. Enabling recomputes `next_run_at` and clears the
/// breaker (that is the only way a self-disabled schedule comes back).
pub fn set_enabled(
    pool: &DbPool,
    schedule_id: &str,
    enabled: bool,
    next_run_at: Option<&str>,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE schedules SET enabled = ?1, auto_disabled = 0, consecutive_failures = 0, \
            next_run_at = ?2, updated_at = datetime('now') WHERE schedule_id = ?3",
        params![i64::from(enabled), next_run_at, schedule_id],
    )?;
    Ok(n > 0)
}

/// Schedules whose next fire instant has passed.
fn due_schedules(pool: &DbPool, now: &str, limit: usize) -> Result<Vec<ScheduleRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SCHEDULE_COLS} FROM schedules \
         WHERE enabled = 1 AND auto_disabled = 0 AND next_run_at IS NOT NULL \
           AND next_run_at <= ?1 ORDER BY next_run_at LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![now, limit as i64], read_schedule)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Atomically claims a due schedule by advancing `next_run_at` BEFORE the
/// trigger runs. The guard on the old value means two ticks (or a tick racing a
/// restart) can never fire the same slot twice — the loser updates 0 rows.
/// A finished one-shot is switched off in the same statement.
fn claim(pool: &DbPool, schedule: &ScheduleRecord, now: DateTime<Utc>) -> Result<bool> {
    let next = compute_next_run(
        &schedule.schedule_kind,
        &schedule.schedule_expr,
        &schedule.timezone,
        now,
    )
    .unwrap_or(None)
    .map(|dt| dt.to_rfc3339());
    let still_enabled = i64::from(next.is_some());
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE schedules SET next_run_at = ?1, enabled = ?2, last_trigger_at = ?3, \
            updated_at = datetime('now') \
         WHERE schedule_id = ?4 AND next_run_at IS ?5",
        params![
            next,
            still_enabled,
            now.to_rfc3339(),
            schedule.schedule_id,
            schedule.next_run_at
        ],
    )?;
    Ok(n > 0)
}

/// Records the outcome of one trigger attempt: the history row plus the
/// schedule's `last_*` summary and the failure breaker.
#[allow(clippy::too_many_arguments)]
fn record_attempt(
    pool: &DbPool,
    schedule_id: &str,
    scheduled_for: &str,
    outcome: &str,
    reason: &str,
    run_id: &str,
    actor: &str,
) -> Result<bool> {
    let trigger_id = uuid::Uuid::new_v4().to_string();
    let started = outcome == "started";
    let run_status = if started { "running" } else { "" };
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO schedule_runs (trigger_id, schedule_id, scheduled_for, outcome, reason, \
            run_id, run_status, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            trigger_id,
            schedule_id,
            scheduled_for,
            outcome,
            reason,
            run_id,
            run_status,
            actor
        ],
    )?;
    // 'blocked' means a precondition (an unapproved environment) refused the
    // run — a configuration state, not a failure, so it never trips the breaker.
    let failures: i64 = match outcome {
        "started" => 0,
        "error" => tx
            .query_row(
                "SELECT consecutive_failures + 1 FROM schedules WHERE schedule_id = ?1",
                params![schedule_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(1),
        _ => tx
            .query_row(
                "SELECT consecutive_failures FROM schedules WHERE schedule_id = ?1",
                params![schedule_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0),
    };
    let tripped = failures >= BREAKER_THRESHOLD as i64;
    // An outcome that did NOT start a run keeps the pointer to the run that
    // did: blanking it would reopen gate 1 (`previous_run_open`) on the next
    // tick — a second run against a still-running one — and would detach
    // `settle` from the schedule, so the terminal status of the open run would
    // never reach `last_status` or the breaker.
    tx.execute(
        "UPDATE schedules SET last_trigger_at = datetime('now'), \
            last_run_id = CASE WHEN ?1 THEN ?2 ELSE last_run_id END, \
            last_status = ?3, last_reason = ?4, consecutive_failures = ?5, \
            auto_disabled = CASE WHEN ?6 THEN 1 ELSE auto_disabled END, \
            next_run_at = CASE WHEN ?6 THEN NULL ELSE next_run_at END, \
            updated_at = datetime('now') \
         WHERE schedule_id = ?7",
        params![
            started,
            run_id,
            outcome,
            reason,
            failures,
            tripped,
            schedule_id
        ],
    )?;
    tx.commit()?;
    Ok(tripped)
}

/// Terminal outcome of a scheduled run, applied from ONE place
/// (`auto_runs::finish_run`) so the watchdog, cancel and reconcile paths all
/// settle a schedule identically. A run ending in 'error' counts toward the
/// breaker; 'completed'/'cancelled' clear it (failing test cases are a normal
/// result, not a broken schedule).
pub fn settle(pool: &DbPool, run_id: &str, status: &str) {
    match settle_inner(pool, run_id, status) {
        // The lock taken by `settle_inner` is released here: the announcement
        // reads the schedule and writes an activity row of its own.
        Ok(Some(schedule_id)) => announce_breaker(pool, &schedule_id),
        Ok(None) => {}
        Err(e) => tracing::warn!(run_id, "schedule settle failed: {e}"),
    }
}

/// Returns the id of the schedule the breaker just stopped, if any.
fn settle_inner(pool: &DbPool, run_id: &str, status: &str) -> Result<Option<String>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE schedule_runs SET run_status = ?1 WHERE run_id = ?2",
        params![status, run_id],
    )?;
    let failed = status == "error";
    let updated = tx.execute(
        "UPDATE schedules SET last_status = ?1, \
            consecutive_failures = CASE WHEN ?2 THEN consecutive_failures + 1 ELSE 0 END, \
            updated_at = datetime('now') \
         WHERE last_run_id = ?3",
        params![status, failed, run_id],
    )?;
    let mut tripped = None;
    if updated > 0 && failed {
        tripped = tx
            .query_row(
                "SELECT schedule_id FROM schedules \
                 WHERE last_run_id = ?1 AND consecutive_failures >= ?2",
                params![run_id, BREAKER_THRESHOLD as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(schedule_id) = tripped.as_deref() {
            tx.execute(
                "UPDATE schedules SET auto_disabled = 1, next_run_at = NULL, \
                    last_reason = 'przerwane po serii bledow', updated_at = datetime('now') \
                 WHERE schedule_id = ?1",
                params![schedule_id],
            )?;
        }
    }
    tx.commit()?;
    Ok(tripped)
}

/// Records and announces a breaker trip. Both breaker paths (a trigger that
/// errored and a RUN that ended in 'error') end here, so a schedule can never
/// stop itself without an audit row and a bell entry for the managers.
fn announce_breaker(pool: &DbPool, schedule_id: &str) {
    let Ok(Some(schedule)) = get(pool, schedule_id) else {
        return;
    };
    super::activity::record(
        pool,
        "",
        "system",
        "schedule.auto_disabled",
        "schedule",
        schedule_id,
        &serde_json::json!({
            "consecutive_failures": schedule.consecutive_failures,
            "reason": schedule.last_reason,
        })
        .to_string(),
    );
    match owning_project(pool) {
        Some(project) => notify_breaker(&project, &schedule),
        None => tracing::warn!(
            schedule_id,
            "schedule stopped itself but its project could not be resolved — no notification sent"
        ),
    }
}

/// The project a per-project pool belongs to. `settle` is reached from
/// `auto_runs::finish_run`, which carries the pool and nothing else, while the
/// breaker notification needs the project and its org.
fn owning_project(pool: &DbPool) -> Option<DueProject> {
    let project_id = super::project_db::project_id_of(pool)?;
    let central = super::db::pool().ok()?;
    let conn = central.read().ok()?;
    conn.query_row(
        "SELECT org_id, name, dir_path FROM projects WHERE project_id = ?1",
        params![project_id],
        |row| {
            Ok(DueProject {
                project_id: project_id.clone(),
                org_id: row.get(0)?,
                name: row.get(1)?,
                dir_path: row.get(2)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

// =============================================================================
// SQL: trigger history
// =============================================================================

pub fn list_triggers(
    pool: &DbPool,
    schedule_id: &str,
    limit: u32,
) -> Result<Vec<ScheduleRunRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT trigger_id, schedule_id, scheduled_for, fired_at, outcome, reason, run_id, \
                run_status, actor FROM schedule_runs \
         WHERE schedule_id = ?1 ORDER BY fired_at DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![schedule_id, limit as i64], |row| {
        Ok(ScheduleRunRecord {
            trigger_id: row.get(0)?,
            schedule_id: row.get(1)?,
            scheduled_for: row.get(2)?,
            fired_at: row.get(3)?,
            outcome: row.get(4)?,
            reason: row.get(5)?,
            run_id: row.get(6)?,
            run_status: row.get(7)?,
            actor: row.get(8)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Run numbers of the runs referenced by a trigger list (the wire shows `#no`).
pub fn run_numbers(pool: &DbPool, run_ids: &[String]) -> Result<HashMap<String, u32>> {
    let mut out = HashMap::new();
    if run_ids.is_empty() {
        return Ok(out);
    }
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare("SELECT run_no FROM test_runs WHERE run_id = ?1")?;
    for run_id in run_ids {
        if run_id.is_empty() {
            continue;
        }
        if let Some(no) = stmt
            .query_row(params![run_id], |row| row.get::<_, i64>(0))
            .optional()?
        {
            out.insert(run_id.clone(), no as u32);
        }
    }
    Ok(out)
}

// =============================================================================
// KPI counters
// =============================================================================

/// Enabled schedules and, of those, the ones that cannot currently fire: the
/// breaker tripped, or an automated schedule points at an environment that is
/// no longer approved.
pub fn project_f4_kpis(pool: &DbPool) -> Result<ProjectF4Kpis> {
    let conn = pool.read().map_err(read_err)?;
    let (enabled, blocked): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM( \
             s.auto_disabled = 1 OR (s.run_type IN ('auto','perf') AND NOT EXISTS ( \
                 SELECT 1 FROM environments e WHERE e.environment_id = s.environment_id \
                   AND e.approval_status = 'approved')) \
           ), 0) \
         FROM schedules s WHERE s.enabled = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let links: i64 = conn.query_row("SELECT COUNT(*) FROM ml_links", [], |row| row.get(0))?;
    Ok(ProjectF4Kpis {
        schedules_enabled: enabled as u32,
        schedules_blocked: blocked as u32,
        ml_links: links as u32,
    })
}

// =============================================================================
// Central-registry hint
// =============================================================================

/// Recomputes the project's due-hint in the CENTRAL registry from its own
/// schedules. Called after every save/delete/toggle and after every tick that
/// touched the project — the hint is derived state, so a stale row self-heals
/// on the next pass instead of needing a distributed transaction.
pub fn refresh_hint(project_id: &str, org_id: &str) -> Result<()> {
    let pool = super::project_db::open(project_id)?;
    let (enabled_count, next_run_at): (i64, Option<String>) = {
        let conn = pool.read().map_err(read_err)?;
        conn.query_row(
            "SELECT COUNT(*), MIN(next_run_at) FROM schedules \
             WHERE enabled = 1 AND auto_disabled = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
    };
    write_hint(project_id, org_id, enabled_count, next_run_at.as_deref())
}

fn write_hint(
    project_id: &str,
    org_id: &str,
    enabled_count: i64,
    next_run_at: Option<&str>,
) -> Result<()> {
    let central = super::db::pool()?;
    let conn = central.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO project_schedule_hints (project_id, org_id, next_run_at, enabled_count, \
            updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(project_id) DO UPDATE SET org_id = ?2, next_run_at = ?3, \
            enabled_count = ?4, updated_at = datetime('now')",
        params![project_id, org_id, next_run_at, enabled_count],
    )?;
    Ok(())
}

/// Drops the hint of a deleted project so the loop never selects it again.
pub fn delete_hint(project_id: &str) {
    let Ok(central) = super::db::pool() else {
        return;
    };
    if let Ok(conn) = central.write() {
        let _ = conn.execute(
            "DELETE FROM project_schedule_hints WHERE project_id = ?1",
            params![project_id],
        );
    };
}

/// One due project as seen by the loop — ONE registry query per tick.
struct DueProject {
    project_id: String,
    org_id: String,
    name: String,
    dir_path: String,
}

fn due_projects(now: &str) -> Result<Vec<DueProject>> {
    let central = super::db::pool()?;
    let conn = central.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT h.project_id, h.org_id, p.name, p.dir_path FROM project_schedule_hints h \
         JOIN projects p ON p.project_id = h.project_id \
         WHERE p.status = 'active' AND h.enabled_count > 0 AND h.next_run_at IS NOT NULL \
           AND h.next_run_at <= ?1 ORDER BY h.next_run_at LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![now, MAX_DUE_PROJECTS as i64], |row| {
        Ok(DueProject {
            project_id: row.get(0)?,
            org_id: row.get(1)?,
            name: row.get(2)?,
            dir_path: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// =============================================================================
// Firing
// =============================================================================

/// Shared resources a trigger needs. The loop has no `HandlerContext`, and the
/// "run now" handler must take exactly the same path, so both build this.
#[derive(Clone)]
pub struct TriggerCtx {
    pub core_db: DbPool,
    pub settings_cipher: Arc<SettingsCipher>,
    pub node_id: Arc<str>,
}

/// What one trigger attempt did.
#[derive(Debug, Clone)]
pub struct TriggerOutcome {
    /// 'started' | 'skipped' | 'blocked' | 'error'.
    pub outcome: String,
    pub reason: String,
    pub run_id: String,
    pub run_no: u32,
}

impl TriggerOutcome {
    fn refused(outcome: &str, reason: impl Into<String>) -> Self {
        Self {
            outcome: outcome.to_string(),
            reason: reason.into(),
            run_id: String::new(),
            run_no: 0,
        }
    }
}

fn case_ids_of(schedule: &ScheduleRecord) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&schedule.case_ids_json).unwrap_or_default()
}

fn assignees_of(schedule: &ScheduleRecord) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&schedule.assignees_json).unwrap_or_default()
}

/// True while the run this schedule started last is still open.
fn previous_run_open(pool: &DbPool, run_id: &str) -> bool {
    if run_id.is_empty() {
        return false;
    }
    let Ok(conn) = pool.read() else {
        return false;
    };
    matches!(
        conn.query_row(
            "SELECT status FROM test_runs WHERE run_id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional(),
        Ok(Some(status)) if status == "running"
    )
}

fn active_auto_runs(pool: &DbPool) -> i64 {
    let Ok(conn) = pool.read() else {
        return 0;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM test_runs WHERE status = 'running' \
         AND run_id IN (SELECT run_id FROM auto_run_meta)",
        [],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Tells every manager of the project that a schedule is stuck on an
/// environment that is no longer approved. `link_json` carries the schedule id,
/// so the unread-duplicate guard collapses a nightly job into ONE bell entry
/// instead of one per night.
fn notify_blocked(project: &DueProject, schedule: &ScheduleRecord, reason: &str) {
    let Ok(members) = super::repository::list_members(&project.project_id) else {
        return;
    };
    let link = serde_json::json!({
        "project_id": project.project_id,
        "schedule_id": schedule.schedule_id,
    })
    .to_string();
    for member in members {
        if member.role != "owner" && member.role != "manager" {
            continue;
        }
        super::notifications::notify(
            &project.org_id,
            &member.user_id,
            &project.project_id,
            "schedule_blocked",
            "Harmonogram wstrzymany",
            &format!(
                "„{}” w projekcie „{}”: {reason}",
                schedule.name, project.name
            ),
            &link,
        );
    }
}

/// Schedules with a trigger in flight on this node. The loop claims its slot by
/// advancing `next_run_at` (a CAS), but "run now" deliberately leaves the slot
/// alone — so two manual triggers, or a manual one racing a tick, would both
/// pass gate 1 before either had created its run.
fn triggers_in_flight() -> &'static std::sync::Mutex<HashSet<String>> {
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

struct TriggerGuard(String);

impl TriggerGuard {
    /// The key carries the project: an import copies the `schedules` rows
    /// verbatim, so the same schedule id can exist in two projects and one must
    /// never block the other's trigger.
    fn acquire(project_id: &str, schedule_id: &str) -> Option<Self> {
        let key = format!("{project_id}/{schedule_id}");
        let mut in_flight = triggers_in_flight().lock().ok()?;
        if !in_flight.insert(key.clone()) {
            return None;
        }
        Some(Self(key))
    }
}

impl Drop for TriggerGuard {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = triggers_in_flight().lock() {
            in_flight.remove(&self.0);
        }
    }
}

/// Runs one trigger attempt through the full gate chain and records it. Used by
/// the loop AND by "run now" — a manual trigger must not be able to bypass a
/// single gate. `actor` is empty for the loop.
pub async fn trigger_once(
    ctx: &TriggerCtx,
    project: &ProjectRecord,
    pool: &DbPool,
    schedule: &ScheduleRecord,
    scheduled_for: &str,
    actor: &str,
) -> TriggerOutcome {
    // Nothing is recorded for this refusal: the trigger that holds the guard is
    // the one being recorded, and a second `last_status` write would clobber it.
    let Some(_guard) = TriggerGuard::acquire(&project.project_id, &schedule.schedule_id) else {
        return TriggerOutcome::refused("skipped", "trigger tego harmonogramu juz trwa");
    };
    let due = DueProject {
        project_id: project.project_id.clone(),
        org_id: project.org_id.clone(),
        name: project.name.clone(),
        dir_path: project.dir_path.clone(),
    };
    let archived = project.status == "archived";
    let outcome = fire(ctx, &due, archived, pool, schedule, actor).await;
    // Only the loop announces a block: a human who just pressed "run now" is
    // already looking at the reason, and a bell entry per click is noise.
    if outcome.outcome == "blocked" && actor.is_empty() {
        notify_blocked(&due, schedule, &outcome.reason);
    }
    if let Err(e) = record_attempt(
        pool,
        &schedule.schedule_id,
        scheduled_for,
        &outcome.outcome,
        &outcome.reason,
        &outcome.run_id,
        actor,
    ) {
        tracing::warn!(schedule_id = %schedule.schedule_id, "schedule attempt record failed: {e}");
    }
    outcome
}

/// The gate chain itself. Order is deliberate: cheap local state first, the
/// runner probe and the DNS re-check last, so a schedule that cannot run for a
/// trivial reason never touches the network.
async fn fire(
    ctx: &TriggerCtx,
    project: &DueProject,
    archived: bool,
    pool: &DbPool,
    schedule: &ScheduleRecord,
    actor: &str,
) -> TriggerOutcome {
    // 1. The previous run of THIS schedule is still open.
    if previous_run_open(pool, &schedule.last_run_id) {
        return TriggerOutcome::refused("skipped", "poprzedni przebieg nadal trwa");
    }
    // 2. An archived project is read-only.
    if archived {
        return TriggerOutcome::refused("skipped", "projekt jest zarchiwizowany");
    }

    let automated = schedule.run_type == "auto" || schedule.run_type == "perf";

    // 3. An automated schedule may only run against an APPROVED environment.
    //    Checked BEFORE the case set, so an approval withdrawn after the save
    //    reports as 'blocked' (a decision to reverse) and not as 'skipped'.
    let environment = if automated {
        let environment = match super::environments::get(pool, &schedule.environment_id) {
            Ok(Some(env)) => env,
            Ok(None) => return TriggerOutcome::refused("blocked", "srodowisko nie istnieje"),
            Err(e) => return TriggerOutcome::refused("error", format!("odczyt srodowiska: {e}")),
        };
        if environment.approval_status != "approved" {
            return TriggerOutcome::refused(
                "blocked",
                format!(
                    "srodowisko „{}” nie jest zatwierdzone (status '{}')",
                    environment.name, environment.approval_status
                ),
            );
        }
        Some(environment)
    } else {
        None
    };

    // 4. Nothing executable left (cases deleted, unapproved, or all manual) is a
    //    skip: the schedule itself is still sound.
    let case_ids = case_ids_of(schedule);
    let cases = match super::auto_runs::resolve_cases(
        pool,
        &schedule.suite_id,
        &case_ids,
        "",
        MAX_SCHEDULE_CASES,
        false,
    ) {
        Ok(cases) => cases,
        Err(e) => return TriggerOutcome::refused("skipped", e.to_string()),
    };
    if cases.is_empty() {
        return TriggerOutcome::refused("skipped", "brak wykonywalnych przypadkow");
    }

    let Some(environment) = environment else {
        return fire_manual(project, pool, schedule, &cases, actor);
    };

    let runnable_language = cases
        .iter()
        .find(|c| super::generation::is_code_kind(&c.kind))
        .map(|c| c.language.clone());
    let Some(runnable_language) = runnable_language else {
        return TriggerOutcome::refused("skipped", "wybor nie zawiera przypadkow automatycznych");
    };

    if active_auto_runs(pool) >= MAX_ACTIVE_AUTO_RUNS {
        return TriggerOutcome::refused("skipped", "projekt ma juz 3 trwajace przebiegi");
    }

    // 5. No runner (or one this node may not use) is a transient environment
    //    problem, NOT a schedule failure — the schedule stays enabled.
    let core_db = ctx.core_db.clone();
    let discovered =
        match tokio::task::spawn_blocking(move || super::auto_runs::list_runners(&core_db)).await {
            Ok(Ok(runners)) => runners,
            Ok(Err(e)) => return TriggerOutcome::refused("skipped", format!("brak runnera: {e}")),
            Err(_) => return TriggerOutcome::refused("error", "runner discovery task panicked"),
        };
    let runner = match super::auto_runs::select_runner(
        discovered,
        &schedule.runner_service_id,
        &runnable_language,
    ) {
        Ok(runner) => runner,
        Err(e) => return TriggerOutcome::refused("skipped", format!("brak runnera: {e}")),
    };
    if let Some(reason) = super::auto_runs::isolation_refusal(&ctx.core_db, &runner) {
        return TriggerOutcome::refused("skipped", reason);
    }

    // 6. Re-resolve the address: an environment approved as public may point at
    //    a private host by now (DNS rebinding).
    let recheck = environment.clone();
    match tokio::task::spawn_blocking(move || super::environments::recheck_private(&recheck)).await
    {
        Ok(Ok(now_private)) if now_private && !environment.is_private_address => {
            super::activity::record_org_security(
                &ctx.core_db,
                &ctx.node_id,
                actor,
                "project_studio.environment_address_rebinding_denied",
                &format!(
                    "project:{}/environment:{}",
                    project.project_id, environment.environment_id
                ),
                &serde_json::json!({ "base_url": environment.base_url }).to_string(),
            );
            return TriggerOutcome::refused(
                "blocked",
                format!(
                    "srodowisko „{}” wskazuje teraz adres prywatny",
                    environment.name
                ),
            );
        }
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return TriggerOutcome::refused("blocked", e.to_string()),
        Err(_) => return TriggerOutcome::refused("error", "address classification task panicked"),
    }

    // 7. Everything checks out — create the run and hand it to the runner.
    let prepared = match super::auto_runs::create_and_prepare_run(
        pool,
        &schedule.name,
        &schedule.suite_id,
        &schedule.run_type,
        &schedule.environment_id,
        &cases,
        &runner,
        &schedule.perf_profile_json,
        &schedule.created_by,
    ) {
        Ok(prepared) => prepared,
        Err(e) => return TriggerOutcome::refused("error", e.to_string()),
    };
    if prepared.submit_items.is_empty() {
        let _ = super::auto_runs::finish_run(
            pool,
            &prepared.run_id,
            "completed",
            "nothing to execute on this runner",
        );
        return TriggerOutcome::refused("skipped", "runner nie obsluguje zadnego przypadku");
    }

    let secret = if environment.auth_type == "none" {
        String::new()
    } else {
        match super::environments::decrypt_secret(&ctx.settings_cipher, &environment) {
            Ok(secret) => secret,
            Err(e) => {
                let _ = super::auto_runs::finish_run(
                    pool,
                    &prepared.run_id,
                    "error",
                    "environment secret unavailable",
                );
                return TriggerOutcome::refused("error", format!("sekret srodowiska: {e}"));
            }
        }
    };
    let submit_env = super::auto_runs::SubmitEnvironment {
        base_url: environment.base_url.clone(),
        auth_type: environment.auth_type.clone(),
        secret,
        extra_headers: serde_json::from_str(&environment.extra_headers_json)
            .unwrap_or_else(|_| serde_json::json!({})),
        host_allowlist: super::environments::host_allowlist_of(&environment),
    };
    let deadline = super::auto_runs::get_meta(pool, &prepared.run_id)
        .ok()
        .flatten()
        .map(|meta| meta.watchdog_deadline_ms)
        .unwrap_or_default();
    if let Err(e) = super::auto_runs::submit_and_watch(
        pool.clone(),
        prepared.run_id.clone(),
        std::path::PathBuf::from(&project.dir_path),
        runner.endpoint_url.clone(),
        prepared.submit_items,
        submit_env,
        deadline,
    )
    .await
    {
        let _ = super::auto_runs::finish_run(pool, &prepared.run_id, "error", &e.to_string());
        return TriggerOutcome::refused("error", format!("runner odrzucil przebieg: {e}"));
    }

    super::activity::record(
        pool,
        actor,
        if actor.is_empty() { "system" } else { "user" },
        "run.started_auto",
        "run",
        &prepared.run_id,
        &serde_json::json!({
            "run_no": prepared.run_no,
            "schedule_id": schedule.schedule_id,
            "environment_id": schedule.environment_id,
            "runner_service_id": runner.service_id,
            "items": cases.len(),
        })
        .to_string(),
    );
    let _ = super::repository::touch_project(&project.project_id);
    TriggerOutcome {
        outcome: "started".to_string(),
        reason: String::new(),
        run_id: prepared.run_id,
        run_no: prepared.run_no,
    }
}

/// Manual schedules create a normal execution sheet: no environment, no runner
/// and no address re-check — a human executes it.
fn fire_manual(
    project: &DueProject,
    pool: &DbPool,
    schedule: &ScheduleRecord,
    cases: &[super::auto_runs::AutoCase],
    actor: &str,
) -> TriggerOutcome {
    let case_ids: Vec<String> = cases.iter().map(|c| c.case_id.clone()).collect();
    let snapshots = match super::runs::approved_case_snapshots(pool, &case_ids) {
        Ok(snapshots) => snapshots,
        Err(e) => return TriggerOutcome::refused("skipped", e.to_string()),
    };
    let picked = assignees_of(schedule);
    let assignees: Vec<String> = match schedule.assignment_mode.as_str() {
        "single" => vec![picked.first().cloned().unwrap_or_default(); snapshots.len()],
        // 'per_case' cannot be resolved from a stored list once the case set
        // changes, so a schedule distributes round-robin over the picked
        // testers; an empty list leaves every item in the pool.
        "per_case" if !picked.is_empty() => (0..snapshots.len())
            .map(|i| picked[i % picked.len()].clone())
            .collect(),
        _ => vec![String::new(); snapshots.len()],
    };
    let (run_id, run_no) = match super::runs::create_run(
        pool,
        &schedule.name,
        &schedule.suite_id,
        "",
        &schedule.assignment_mode,
        &snapshots,
        &assignees,
        &schedule.created_by,
    ) {
        Ok(created) => created,
        Err(e) => return TriggerOutcome::refused("error", e.to_string()),
    };

    super::activity::record(
        pool,
        actor,
        if actor.is_empty() { "system" } else { "user" },
        "run.created",
        "run",
        &run_id,
        &serde_json::json!({
            "name": schedule.name,
            "run_no": run_no,
            "schedule_id": schedule.schedule_id,
            "cases": snapshots.len(),
        })
        .to_string(),
    );
    let mut per_user: HashMap<String, u32> = HashMap::new();
    for assignee in &assignees {
        if !assignee.is_empty() {
            *per_user.entry(assignee.clone()).or_default() += 1;
        }
    }
    for (user_id, count) in per_user {
        super::notifications::notify(
            &project.org_id,
            &user_id,
            &project.project_id,
            "run_item_assigned",
            "Przydzielono Ci testy",
            &format!(
                "{count} przypadkow w przebiegu #{run_no} „{}”",
                schedule.name
            ),
            &serde_json::json!({ "project_id": project.project_id, "run_id": run_id }).to_string(),
        );
    }
    let _ = super::repository::touch_project(&project.project_id);
    TriggerOutcome {
        outcome: "started".to_string(),
        reason: String::new(),
        run_id,
        run_no,
    }
}

/// Tells the managers that a schedule stopped itself.
fn notify_breaker(project: &DueProject, schedule: &ScheduleRecord) {
    let Ok(members) = super::repository::list_members(&project.project_id) else {
        return;
    };
    let link = serde_json::json!({
        "project_id": project.project_id,
        "schedule_id": schedule.schedule_id,
    })
    .to_string();
    for member in members {
        if member.role != "owner" && member.role != "manager" {
            continue;
        }
        super::notifications::notify(
            &project.org_id,
            &member.user_id,
            &project.project_id,
            "schedule_auto_disabled",
            "Harmonogram zatrzymany",
            &format!(
                "„{}” w projekcie „{}” zatrzymal sie po {BREAKER_THRESHOLD} nieudanych probach",
                schedule.name, project.name
            ),
            &link,
        );
    }
}

// =============================================================================
// Background loop
// =============================================================================

static LOOP_STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Starts the schedule loop. Idempotent — a second call is a no-op, so tests
/// and a restarted server never run two loops over the same registry.
pub fn start(core_db: DbPool, settings_cipher: Arc<SettingsCipher>, node_id: Arc<str>) {
    if LOOP_STARTED.set(()).is_err() {
        return;
    }
    let ctx = TriggerCtx {
        core_db,
        settings_cipher,
        node_id,
    };
    tokio::spawn(async move {
        loop {
            if let Err(e) = run_due_once(&ctx).await {
                tracing::warn!("project studio schedule tick failed: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
        }
    });
}

/// One tick: ONE registry query, then only the projects that are actually due
/// get their database opened.
async fn run_due_once(ctx: &TriggerCtx) -> Result<()> {
    let now = Utc::now();
    let projects = due_projects(&now.to_rfc3339())?;
    for project in projects {
        match fire_project(ctx, &project, now).await {
            Ok(()) => {
                let _ = refresh_hint(&project.project_id, &project.org_id);
            }
            Err(e) => {
                tracing::warn!(project_id = %project.project_id, "schedule project tick failed: {e}");
                // `refresh_hint` would have to OPEN the project database, which
                // is exactly what just failed. The hint is cleared directly, so
                // a broken project stops occupying one of the ten due slots
                // every 30 s; a later save/toggle recomputes it.
                let _ = write_hint(&project.project_id, &project.org_id, 0, None);
            }
        }
    }
    Ok(())
}

async fn fire_project(ctx: &TriggerCtx, project: &DueProject, now: DateTime<Utc>) -> Result<()> {
    let pool = super::project_db::open(&project.project_id)?;
    // A run left 'running' by a restart has no watcher any more: gate 1 would
    // skip this schedule forever, without a breaker and without a notification.
    // The dashboard reconciles lazily, but a node with nobody looking at it —
    // the very case schedules exist for — would never get there.
    super::auto_runs::reconcile_running(&pool);
    let due = due_schedules(&pool, &now.to_rfc3339(), MAX_DUE_SCHEDULES)?;
    for schedule in due {
        // Claiming BEFORE the trigger runs is what keeps a long trigger from
        // being fired again by the next tick.
        if !claim(&pool, &schedule, now)? {
            continue;
        }
        let scheduled_for = schedule.next_run_at.clone().unwrap_or_default();
        let ctx = ctx.clone();
        let pool = pool.clone();
        let project = DueProject {
            project_id: project.project_id.clone(),
            org_id: project.org_id.clone(),
            name: project.name.clone(),
            dir_path: project.dir_path.clone(),
        };
        tokio::spawn(async move {
            let record = ProjectRecord {
                project_id: project.project_id.clone(),
                org_id: project.org_id.clone(),
                name: project.name.clone(),
                description: String::new(),
                status: "active".to_string(),
                template: String::new(),
                modules_json: String::new(),
                owner_user_id: String::new(),
                dir_path: project.dir_path.clone(),
                created_at: String::new(),
                updated_at: String::new(),
            };
            let outcome = trigger_once(&ctx, &record, &pool, &schedule, &scheduled_for, "").await;
            if outcome.outcome == "error" {
                if let Ok(Some(after)) = get(&pool, &schedule.schedule_id) {
                    if after.auto_disabled {
                        announce_breaker(&pool, &schedule.schedule_id);
                    }
                }
            }
            let _ = refresh_hint(&project.project_id, &project.org_id);
        });
    }
    Ok(())
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

    fn seed(pool: &DbPool, schedule_id: &str, kind: &str, expr: &str, next: Option<&str>) {
        let conn = pool.write().expect("write");
        conn.execute(
            "INSERT INTO schedules (schedule_id, name, run_type, suite_id, schedule_kind, \
                schedule_expr, timezone, next_run_at, created_by) \
             VALUES (?1, 'nocny', 'manual', 's1', ?2, ?3, 'Europe/Warsaw', ?4, 'u1')",
            params![schedule_id, kind, expr, next],
        )
        .expect("insert schedule");
    }

    /// An automated schedule with one approved, runnable case.
    fn seed_auto(pool: &DbPool, schedule_id: &str, environment_id: &str) {
        let conn = pool.write().expect("write");
        conn.execute(
            "INSERT INTO test_cases (case_id, kind, title, language, content_json, status, \
                created_by) VALUES ('c1', 'api', 'Logowanie', 'python', '{}', 'approved', 'u1')",
            [],
        )
        .expect("insert case");
        conn.execute(
            "INSERT INTO environments (environment_id, name, base_url, approval_status, \
                requested_by) VALUES (?1, 'staging', 'https://staging.example', 'pending', 'u1')",
            params![environment_id],
        )
        .expect("insert env");
        conn.execute(
            "INSERT INTO schedules (schedule_id, name, run_type, case_ids_json, \
                environment_id, schedule_kind, schedule_expr, timezone, next_run_at, created_by) \
             VALUES (?1, 'nocny', 'auto', '[\"c1\"]', ?2, 'interval', '30m', 'UTC', \
                '2026-07-01T10:00:00Z', 'u1')",
            params![schedule_id, environment_id],
        )
        .expect("insert schedule");
    }

    fn set_env_status(pool: &DbPool, environment_id: &str, status: &str) {
        let conn = pool.write().expect("write");
        conn.execute(
            "UPDATE environments SET approval_status = ?1 WHERE environment_id = ?2",
            params![status, environment_id],
        )
        .expect("set status");
    }

    fn trigger_ctx() -> TriggerCtx {
        let state = crate::dispatch::state::AppState::for_test();
        TriggerCtx {
            core_db: state.db.clone(),
            settings_cipher: state.settings_cipher.clone(),
            node_id: state.local_node_id.clone(),
        }
    }

    fn project_record() -> ProjectRecord {
        ProjectRecord {
            project_id: format!("sched-{}", uuid::Uuid::new_v4()),
            org_id: "org-t".to_string(),
            name: "Projekt".to_string(),
            description: String::new(),
            status: "active".to_string(),
            template: "tests".to_string(),
            modules_json: "[]".to_string(),
            owner_user_id: "u1".to_string(),
            dir_path: "/tmp/none".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn ts(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    /// The two DST edges of Europe/Warsaw in 2026 with a nightly 02:30 job.
    /// Spring forward (2026-03-29): 02:30 local never happens, so the job runs
    /// an hour later (03:30 local = 01:30 UTC). Autumn back (2026-10-25): 02:30
    /// local happens twice, and the EARLIER instant wins (00:30 UTC, still CEST).
    #[test]
    fn compute_next_run_resolves_dst_edges() {
        let gap = compute_next_run(
            "cron",
            "30 2 * * *",
            "Europe/Warsaw",
            ts("2026-03-28T23:00:00Z"),
        )
        .expect("gap day")
        .expect("instant");
        assert_eq!(
            gap.to_rfc3339(),
            "2026-03-29T01:30:00+00:00",
            "a nonexistent local time fires one hour later"
        );

        let ambiguous = compute_next_run(
            "cron",
            "30 2 * * *",
            "Europe/Warsaw",
            ts("2026-10-24T23:00:00Z"),
        )
        .expect("ambiguous day")
        .expect("instant");
        assert_eq!(
            ambiguous.to_rfc3339(),
            "2026-10-25T00:30:00+00:00",
            "an ambiguous local time fires at the earlier instant"
        );

        // A plain day is a plain +02:00 (CEST) / +01:00 (CET) conversion.
        let summer = compute_next_run(
            "cron",
            "30 2 * * *",
            "Europe/Warsaw",
            ts("2026-07-01T10:00:00Z"),
        )
        .expect("summer")
        .expect("instant");
        assert_eq!(summer.to_rfc3339(), "2026-07-02T00:30:00+00:00");
        let winter = compute_next_run(
            "cron",
            "30 2 * * *",
            "Europe/Warsaw",
            ts("2026-12-01T10:00:00Z"),
        )
        .expect("winter")
        .expect("instant");
        assert_eq!(winter.to_rfc3339(), "2026-12-02T01:30:00+00:00");

        // The preview walks the same arithmetic, so it never disagrees with the
        // loop across the transition.
        let preview = next_runs_preview(
            "cron",
            "30 2 * * *",
            "Europe/Warsaw",
            ts("2026-03-27T23:00:00Z"),
            3,
        );
        assert_eq!(
            preview,
            vec![
                "2026-03-28T01:30:00+00:00".to_string(),
                "2026-03-29T01:30:00+00:00".to_string(),
                "2026-03-30T00:30:00+00:00".to_string(),
            ]
        );
    }

    /// Interval and one-shot bounds.
    #[test]
    fn interval_and_once_bounds_are_enforced() {
        assert!(parse_interval_secs("30m").is_ok());
        assert!(parse_interval_secs("1m").is_err(), "under 5 minutes");
        assert!(parse_interval_secs("400d").is_err(), "over 365 days");
        assert!(parse_interval_secs("").is_err());
        assert!(parse_interval_secs("10x").is_err());

        let now = ts("2026-07-01T10:00:00Z");
        let next = compute_next_run("interval", "30m", "UTC", now)
            .expect("interval")
            .expect("instant");
        assert_eq!(next.to_rfc3339(), "2026-07-01T10:30:00+00:00");

        // A one-shot that already passed never fires again.
        assert!(compute_next_run("once", "2020-01-01T00:00:00Z", "UTC", now)
            .expect("once")
            .is_none());
    }

    /// A trigger while the previous run is still open is skipped, and the
    /// skip does NOT count toward the breaker.
    #[test]
    fn previous_running_run_skips_the_trigger() {
        let pool = pool();
        seed(
            &pool,
            "sc1",
            "interval",
            "30m",
            Some("2026-07-01T10:00:00Z"),
        );
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, created_by) \
                 VALUES ('r1', 1, 'nocny', 'pool', 'u1')",
                [],
            )
            .expect("insert run");
            conn.execute(
                "UPDATE schedules SET last_run_id = 'r1' WHERE schedule_id = 'sc1'",
                [],
            )
            .expect("bind run");
        }
        let schedule = get(&pool, "sc1").expect("get").expect("row");
        assert!(previous_run_open(&pool, &schedule.last_run_id));

        record_attempt(
            &pool,
            "sc1",
            "2026-07-01T10:00:00Z",
            "skipped",
            "poprzedni przebieg nadal trwa",
            "",
            "",
        )
        .expect("record");
        let after = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(after.last_status, "skipped");
        assert_eq!(after.consecutive_failures, 0, "a skip is not a failure");
        assert!(!after.auto_disabled);
        assert_eq!(
            after.last_run_id, "r1",
            "a refused trigger keeps pointing at the run that is still open"
        );
        assert!(
            previous_run_open(&pool, &after.last_run_id),
            "so the gate stays closed on the NEXT tick too"
        );

        // A closed run stops blocking the next trigger.
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "UPDATE test_runs SET status = 'completed' WHERE run_id = 'r1'",
                [],
            )
            .expect("close run");
        }
        assert!(!previous_run_open(&pool, "r1"));
    }

    /// A manual schedule whose previous execution sheet is still open does NOT
    /// get a second one on the next tick, and closing the sheet releases it.
    /// This is the whole point of keeping `last_run_id` across a refusal: with
    /// the pointer blanked, tick two would create run #2 against a run #1 nobody
    /// has finished.
    #[tokio::test]
    async fn second_tick_does_not_start_a_second_run() {
        let pool = pool();
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO test_cases (case_id, kind, title, content_json, status, created_by) \
                 VALUES ('c1', 'manual', 'Logowanie', \
                    '{\"steps\":[{\"action\":\"otworz\",\"expected\":\"ekran\"}]}', \
                    'approved', 'u1')",
                [],
            )
            .expect("insert case");
            conn.execute(
                "INSERT INTO schedules (schedule_id, name, run_type, case_ids_json, \
                    schedule_kind, schedule_expr, timezone, next_run_at, created_by) \
                 VALUES ('sc1', 'nocny', 'manual', '[\"c1\"]', 'interval', '30m', 'UTC', \
                    '2026-07-01T10:00:00Z', 'u1')",
                [],
            )
            .expect("insert schedule");
        }
        let ctx = trigger_ctx();
        let project = project_record();
        let runs = |pool: &DbPool| -> i64 {
            pool.read()
                .expect("read")
                .query_row("SELECT COUNT(*) FROM test_runs", [], |row| row.get(0))
                .expect("count")
        };

        let schedule = get(&pool, "sc1").expect("get").expect("row");
        let first = trigger_once(&ctx, &project, &pool, &schedule, "", "").await;
        assert_eq!(first.outcome, "started", "{}", first.reason);
        assert_eq!(runs(&pool), 1);
        let after_first = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(after_first.last_run_id, first.run_id);

        // Tick two, with the sheet from tick one still open.
        let second = trigger_once(&ctx, &project, &pool, &after_first, "", "").await;
        assert_eq!(second.outcome, "skipped", "{}", second.reason);
        assert!(second.reason.contains("poprzedni przebieg"));
        assert_eq!(runs(&pool), 1, "the second tick must not create a run");
        let after_second = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(
            after_second.last_run_id, first.run_id,
            "the skip left the pointer alone"
        );

        // Closing the sheet settles the schedule and releases the gate.
        assert!(super::super::runs::close_run(&pool, &first.run_id, false, "u1").expect("close"));
        let settled = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(
            settled.last_status, "completed",
            "closing a manual run settles the schedule"
        );
        let third = trigger_once(&ctx, &project, &pool, &settled, "", "").await;
        assert_eq!(third.outcome, "started", "{}", third.reason);
        assert_eq!(runs(&pool), 2);
    }

    /// Five consecutive errors stop the schedule: `auto_disabled` is set and
    /// `next_run_at` is cleared, so the due query can never select it again.
    #[test]
    fn breaker_stops_the_schedule_after_five_errors() {
        let pool = pool();
        seed(
            &pool,
            "sc1",
            "interval",
            "30m",
            Some("2026-07-01T10:00:00Z"),
        );
        for i in 1..BREAKER_THRESHOLD {
            let tripped =
                record_attempt(&pool, "sc1", "", "error", "runner padl", "", "").expect("record");
            assert!(!tripped, "breaker tripped early at attempt {i}");
            let row = get(&pool, "sc1").expect("get").expect("row");
            assert_eq!(row.consecutive_failures, i);
            assert!(!row.auto_disabled);
        }
        let tripped =
            record_attempt(&pool, "sc1", "", "error", "runner padl", "", "").expect("record");
        assert!(tripped);
        let row = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(row.consecutive_failures, BREAKER_THRESHOLD);
        assert!(row.auto_disabled);
        assert!(row.next_run_at.is_none());
        assert!(
            due_schedules(&pool, "2030-01-01T00:00:00Z", 5)
                .expect("due")
                .is_empty(),
            "a stopped schedule is never due again"
        );

        // Re-enabling is the only way back, and it clears the counter.
        set_enabled(&pool, "sc1", true, Some("2026-07-01T11:00:00Z")).expect("enable");
        let row = get(&pool, "sc1").expect("get").expect("row");
        assert!(!row.auto_disabled);
        assert_eq!(row.consecutive_failures, 0);
    }

    /// A terminal run status settles the schedule from ONE place. A failing run
    /// counts toward the breaker; a completed one clears it.
    #[test]
    fn settle_applies_the_terminal_run_status() {
        let pool = pool();
        seed(
            &pool,
            "sc1",
            "interval",
            "30m",
            Some("2026-07-01T10:00:00Z"),
        );
        record_attempt(&pool, "sc1", "", "started", "", "r1", "").expect("record");
        assert_eq!(
            get(&pool, "sc1").expect("get").expect("row").last_status,
            "started"
        );

        settle(&pool, "r1", "error");
        let row = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(row.last_status, "error");
        assert_eq!(row.consecutive_failures, 1);
        let triggers = list_triggers(&pool, "sc1", 10).expect("triggers");
        assert_eq!(triggers[0].run_status, "error");

        settle(&pool, "r1", "completed");
        let row = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(row.last_status, "completed");
        assert_eq!(row.consecutive_failures, 0, "success clears the breaker");
    }

    /// The atomic claim advances the slot exactly once; a stale claim (the row
    /// already moved) loses without firing.
    #[test]
    fn claim_advances_the_slot_exactly_once() {
        let pool = pool();
        seed(
            &pool,
            "sc1",
            "interval",
            "30m",
            Some("2026-07-01T10:00:00Z"),
        );
        let schedule = get(&pool, "sc1").expect("get").expect("row");
        let now = ts("2026-07-01T10:00:05Z");
        assert!(claim(&pool, &schedule, now).expect("claim"));
        let after = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(
            after.next_run_at.as_deref(),
            Some("2026-07-01T10:30:05+00:00")
        );
        // The second claim carries the stale next_run_at and must lose.
        assert!(!claim(&pool, &schedule, now).expect("second claim"));

        // A one-shot claim disables the schedule instead of rescheduling it.
        seed(
            &pool,
            "sc2",
            "once",
            "2026-07-01T10:00:00Z",
            Some("2026-07-01T10:00:00Z"),
        );
        let once = get(&pool, "sc2").expect("get").expect("row");
        assert!(claim(&pool, &once, now).expect("claim once"));
        let after = get(&pool, "sc2").expect("get").expect("row");
        assert!(after.next_run_at.is_none());
        assert!(!after.enabled);
    }

    /// An environment whose approval was WITHDRAWN after the schedule was saved
    /// blocks the trigger. 'blocked' is not a failure: it never advances the
    /// breaker, because the fix is an admin decision, not a retry.
    #[tokio::test]
    async fn withdrawn_environment_blocks_instead_of_failing() {
        let pool = pool();
        seed_auto(&pool, "sc1", "e1");
        let ctx = trigger_ctx();
        let project = project_record();

        // Approved: the gate lets it through and fails later, on the runner.
        set_env_status(&pool, "e1", "approved");
        let schedule = get(&pool, "sc1").expect("get").expect("row");
        let outcome = trigger_once(&ctx, &project, &pool, &schedule, "", "").await;
        assert_ne!(outcome.outcome, "blocked", "an approved environment passes");

        // Withdrawn: blocked, with the reason naming the environment.
        set_env_status(&pool, "e1", "pending");
        let schedule = get(&pool, "sc1").expect("get").expect("row");
        let outcome = trigger_once(&ctx, &project, &pool, &schedule, "", "").await;
        assert_eq!(outcome.outcome, "blocked");
        assert!(outcome.reason.contains("staging"), "{}", outcome.reason);
        assert!(outcome.run_id.is_empty());

        let row = get(&pool, "sc1").expect("get").expect("row");
        assert_eq!(row.last_status, "blocked");
        assert_eq!(
            row.consecutive_failures, 0,
            "a blocked trigger must not advance the breaker"
        );
        assert!(!row.auto_disabled);
        assert!(row.next_run_at.is_some(), "the schedule stays scheduled");

        // A missing environment is blocked too — same class of problem.
        {
            let conn = pool.write().expect("write");
            conn.execute("DELETE FROM environments WHERE environment_id = 'e1'", [])
                .expect("delete env");
        }
        let schedule = get(&pool, "sc1").expect("get").expect("row");
        let outcome = trigger_once(&ctx, &project, &pool, &schedule, "", "").await;
        assert_eq!(outcome.outcome, "blocked");
    }

    /// No runner answers: the trigger is SKIPPED, not errored — the schedule
    /// itself is fine, the environment around it is not. It must survive with
    /// its next fire instant intact.
    #[tokio::test]
    async fn missing_runner_skips_and_keeps_the_schedule() {
        let pool = pool();
        seed_auto(&pool, "sc1", "e1");
        set_env_status(&pool, "e1", "approved");
        let ctx = trigger_ctx();
        let project = project_record();

        let before = get(&pool, "sc1").expect("get").expect("row");
        let outcome = trigger_once(&ctx, &project, &pool, &before, "", "").await;
        assert_eq!(
            outcome.outcome, "skipped",
            "a missing runner is not a schedule failure ({})",
            outcome.reason
        );
        assert!(outcome.reason.contains("runner"), "{}", outcome.reason);

        let after = get(&pool, "sc1").expect("get").expect("row");
        assert!(after.enabled, "the schedule stays enabled");
        assert!(!after.auto_disabled);
        assert_eq!(after.consecutive_failures, 0);
        assert_eq!(
            after.next_run_at, before.next_run_at,
            "a refused trigger does not move the slot"
        );
        // The attempt is still recorded, so "never fired" and "fired and
        // refused" are distinguishable in the history.
        let triggers = list_triggers(&pool, "sc1", 10).expect("triggers");
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].outcome, "skipped");
        assert!(triggers[0].run_id.is_empty());
    }

    /// KPI counters: an enabled schedule bound to a non-approved environment is
    /// reported as blocked, and so is one the breaker stopped.
    #[test]
    fn kpis_count_enabled_and_blocked_schedules() {
        let pool = pool();
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO environments (environment_id, name, base_url, approval_status, \
                    requested_by) VALUES ('e-ok', 'staging', 'https://a.test', 'approved', 'u1')",
                [],
            )
            .expect("env approved");
            conn.execute(
                "INSERT INTO environments (environment_id, name, base_url, approval_status, \
                    requested_by) VALUES ('e-pending', 'lan', 'http://10.0.0.1', 'pending', 'u1')",
                [],
            )
            .expect("env pending");
            for (id, env, disabled) in [
                ("s-ok", "e-ok", 0),
                ("s-pending", "e-pending", 0),
                ("s-broken", "e-ok", 1),
            ] {
                conn.execute(
                    "INSERT INTO schedules (schedule_id, name, run_type, environment_id, \
                        auto_disabled, schedule_kind, schedule_expr, created_by) \
                     VALUES (?1, 'n', 'auto', ?2, ?3, 'interval', '30m', 'u1')",
                    params![id, env, disabled],
                )
                .expect("insert schedule");
            }
            conn.execute(
                "INSERT INTO ml_links (link_id, ml_project_id, origin, created_by) \
                 VALUES ('l1', 'ml1', 'linked_existing', 'u1')",
                [],
            )
            .expect("insert link");
        }
        let kpis = project_f4_kpis(&pool).expect("kpis");
        assert_eq!(kpis.schedules_enabled, 3);
        assert_eq!(kpis.schedules_blocked, 2);
        assert_eq!(kpis.ml_links, 1);
    }
}
