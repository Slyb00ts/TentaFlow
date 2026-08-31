// ===== File: project_studio/runs.rs — test runs, run items and step execution (F2) =====
//
// SQL layer for run execution: run creation with pinned snapshots
// (case_version + case_title + copied steps — later case edits never mutate a
// running execution), the atomic pool claim (single UPDATE…RETURNING, no
// select-then-update race), step verdicts and item finish with derived
// results. Counters are always SQL aggregates over items, never denormalized.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use super::models::{RunCounts, RunItemRecord, RunRecord, RunStepRecord};
use super::tests::VISIBLE_CASES_PREDICATE;
use crate::db::DbPool;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio runs read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio runs write: {e}")
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        run_id: row.get(0)?,
        run_no: row.get::<_, i64>(1)? as u32,
        name: row.get(2)?,
        suite_id: row.get(3)?,
        run_type: row.get(4)?,
        environment_id: row.get(5)?,
        env_note: row.get(6)?,
        assignment_mode: row.get(7)?,
        status: row.get(8)?,
        created_by: row.get(9)?,
        started_at: row.get(10)?,
        finished_at: row.get(11)?,
    })
}

const RUN_COLS: &str = "run_id, run_no, name, suite_id, run_type, environment_id, env_note, \
     assignment_mode, status, created_by, started_at, finished_at";

fn run_counts(conn: &Connection, run_id: &str) -> Result<RunCounts> {
    let counts = conn.query_row(
        "SELECT COUNT(*), \
                COALESCE(SUM(status = 'passed'), 0), \
                COALESCE(SUM(status = 'failed'), 0), \
                COALESCE(SUM(status = 'blocked'), 0), \
                COALESCE(SUM(status = 'skipped'), 0), \
                COALESCE(SUM(status = 'pending'), 0), \
                COALESCE(SUM(status = 'in_progress'), 0) \
         FROM test_run_items WHERE run_id = ?1",
        params![run_id],
        |row| {
            Ok(RunCounts {
                total: row.get::<_, i64>(0)? as u32,
                passed: row.get::<_, i64>(1)? as u32,
                failed: row.get::<_, i64>(2)? as u32,
                blocked: row.get::<_, i64>(3)? as u32,
                skipped: row.get::<_, i64>(4)? as u32,
                pending: row.get::<_, i64>(5)? as u32,
                in_progress: row.get::<_, i64>(6)? as u32,
            })
        },
    )?;
    Ok(counts)
}

/// Latest run of a suite for the suites list (shared with tests.rs).
pub(crate) fn latest_run_for_suite(
    conn: &Connection,
    suite_id: &str,
) -> Result<Option<(RunRecord, RunCounts)>> {
    let record = conn
        .query_row(
            &format!(
                "SELECT {RUN_COLS} FROM test_runs WHERE suite_id = ?1 \
                 ORDER BY started_at DESC, run_no DESC LIMIT 1"
            ),
            params![suite_id],
            read_run,
        )
        .optional()?;
    match record {
        Some(record) => {
            let counts = run_counts(conn, &record.run_id)?;
            Ok(Some((record, counts)))
        }
        None => Ok(None),
    }
}

pub fn list_runs(
    pool: &DbPool,
    status: &str,
    run_type: &str,
    offset: u32,
    limit: u32,
) -> Result<(Vec<(RunRecord, RunCounts)>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let mut clauses: Vec<String> = vec!["1=1".to_string()];
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if !status.is_empty() {
        clauses.push(format!("status = ?{}", args.len() + 1));
        args.push(Box::new(status.to_string()));
    }
    if !run_type.is_empty() {
        clauses.push(format!("run_type = ?{}", args.len() + 1));
        args.push(Box::new(run_type.to_string()));
    }
    let where_sql = clauses.join(" AND ");
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM test_runs WHERE {where_sql}"),
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        |row| row.get(0),
    )?;
    let sql = format!(
        "SELECT {RUN_COLS} FROM test_runs WHERE {where_sql} \
         ORDER BY run_no DESC LIMIT ?{} OFFSET ?{}",
        args.len() + 1,
        args.len() + 2
    );
    args.push(Box::new(limit as i64));
    args.push(Box::new(offset as i64));
    let records = {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            read_run,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let counts = run_counts(&conn, &record.run_id)?;
        out.push((record, counts));
    }
    Ok((out, total as u32))
}

pub fn get_run(pool: &DbPool, run_id: &str) -> Result<Option<(RunRecord, RunCounts)>> {
    let conn = pool.read().map_err(read_err)?;
    let record = conn
        .query_row(
            &format!("SELECT {RUN_COLS} FROM test_runs WHERE run_id = ?1"),
            params![run_id],
            read_run,
        )
        .optional()?;
    match record {
        Some(record) => {
            let counts = run_counts(&conn, &record.run_id)?;
            Ok(Some((record, counts)))
        }
        None => Ok(None),
    }
}

fn read_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunItemRecord> {
    Ok(RunItemRecord {
        item_id: row.get(0)?,
        run_id: row.get(1)?,
        case_id: row.get(2)?,
        case_title: row.get(3)?,
        case_version: row.get::<_, i64>(4)? as u32,
        position: row.get::<_, i64>(5)? as u32,
        assigned_to: row.get(6)?,
        status: row.get(7)?,
        result_note: row.get(8)?,
        tester_config: row.get(9)?,
        duration_secs: row.get::<_, i64>(10)? as u32,
        attachments_json: row.get(11)?,
        claimed_at: row.get(12)?,
        finished_at: row.get(13)?,
        steps_total: row.get::<_, i64>(14)? as u32,
        steps_done: row.get::<_, i64>(15)? as u32,
    })
}

/// Item columns + step aggregates (total / with a recorded verdict).
const ITEM_COLS: &str = "i.item_id, i.run_id, i.case_id, i.case_title, i.case_version, \
     i.position, i.assigned_to, i.status, i.result_note, i.tester_config, i.duration_secs, \
     i.attachments_json, i.claimed_at, i.finished_at, \
     (SELECT COUNT(*) FROM test_run_steps s WHERE s.item_id = i.item_id), \
     (SELECT COUNT(*) FROM test_run_steps s WHERE s.item_id = i.item_id AND s.status <> '')";

pub fn list_run_items(pool: &DbPool, run_id: &str) -> Result<Vec<RunItemRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ITEM_COLS} FROM test_run_items i WHERE i.run_id = ?1 ORDER BY i.position"
    ))?;
    let rows = stmt.query_map(params![run_id], read_item)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_run_item(pool: &DbPool, item_id: &str) -> Result<Option<RunItemRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {ITEM_COLS} FROM test_run_items i WHERE i.item_id = ?1"),
        params![item_id],
        read_item,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_item_steps(pool: &DbPool, item_id: &str) -> Result<Vec<RunStepRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT step_index, action, expected, status, note, attachments_json \
         FROM test_run_steps WHERE item_id = ?1 ORDER BY step_index",
    )?;
    let rows = stmt.query_map(params![item_id], |row| {
        Ok(RunStepRecord {
            step_index: row.get::<_, i64>(0)? as u32,
            action: row.get(1)?,
            expected: row.get(2)?,
            status: row.get(3)?,
            note: row.get(4)?,
            attachments_json: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Snapshot payload of one case selected into a new run.
pub struct RunCaseSnapshot {
    pub case_id: String,
    pub case_title: String,
    pub case_version: u32,
    /// (action, expected) copied out of the case content at creation time.
    pub steps: Vec<(String, String)>,
}

/// Loads snapshots for approved, visible cases in the given id order. An id
/// that is not an approved visible case fails the whole selection.
pub fn approved_case_snapshots(pool: &DbPool, case_ids: &[String]) -> Result<Vec<RunCaseSnapshot>> {
    let conn = pool.read().map_err(read_err)?;
    let mut out = Vec::with_capacity(case_ids.len());
    for case_id in case_ids {
        let row: Option<(String, i64, String)> = conn
            .query_row(
                &format!(
                    "SELECT title, current_version, content_json FROM test_cases \
                     WHERE case_id = ?1 AND status = 'approved' AND {VISIBLE_CASES_PREDICATE}"
                ),
                params![case_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((title, version, content_json)) = row else {
            bail!("case '{case_id}' is not an approved test case");
        };
        out.push(RunCaseSnapshot {
            case_id: case_id.clone(),
            case_title: title,
            case_version: version as u32,
            steps: content_steps(&content_json),
        });
    }
    Ok(out)
}

/// Extracts `(action, expected)` pairs from a case content JSON.
pub fn content_steps(content_json: &str) -> Vec<(String, String)> {
    serde_json::from_str::<serde_json::Value>(content_json)
        .ok()
        .and_then(|v| v.get("steps").cloned())
        .and_then(|s| s.as_array().cloned())
        .map(|steps| {
            steps
                .iter()
                .map(|s| {
                    (
                        s.get("action")
                            .and_then(|a| a.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        s.get("expected")
                            .and_then(|e| e.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Case ids of the failed/blocked items of a source run (for `from_failed`
/// re-runs), in original position order.
pub fn failed_case_ids(pool: &DbPool, run_id: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT case_id FROM test_run_items \
         WHERE run_id = ?1 AND status IN ('failed','blocked') ORDER BY position",
    )?;
    let rows = stmt.query_map(params![run_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates the run with its item + step snapshots in ONE transaction.
/// `assignees[i]` is the assigned user of `snapshots[i]` ('' = pool item).
/// `run_no` is `COALESCE(MAX)+1` inside the same transaction (gap-free under
/// the SQLite single writer).
#[allow(clippy::too_many_arguments)]
pub fn create_run(
    pool: &DbPool,
    name: &str,
    suite_id: &str,
    env_note: &str,
    assignment_mode: &str,
    snapshots: &[RunCaseSnapshot],
    assignees: &[String],
    created_by: &str,
) -> Result<(String, u32)> {
    debug_assert_eq!(snapshots.len(), assignees.len());
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let run_no: i64 = tx.query_row(
        "SELECT COALESCE(MAX(run_no), 0) + 1 FROM test_runs",
        [],
        |row| row.get(0),
    )?;
    let run_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO test_runs (run_id, run_no, name, suite_id, env_note, assignment_mode, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![run_id, run_no, name, suite_id, env_note, assignment_mode, created_by],
    )?;
    for (position, (snapshot, assignee)) in snapshots.iter().zip(assignees.iter()).enumerate() {
        let item_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, case_version, \
                position, assigned_to) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                item_id,
                run_id,
                snapshot.case_id,
                snapshot.case_title,
                snapshot.case_version as i64,
                position as i64,
                assignee
            ],
        )?;
        for (step_index, (action, expected)) in snapshot.steps.iter().enumerate() {
            tx.execute(
                "INSERT INTO test_run_steps (item_id, step_index, action, expected) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![item_id, step_index as i64, action, expected],
            )?;
        }
    }
    tx.commit()?;
    Ok((run_id, run_no as u32))
}

/// Closes a run ('completed' or 'cancelled'). Only a running run closes.
pub fn close_run(pool: &DbPool, run_id: &str, cancelled: bool, closed_by: &str) -> Result<bool> {
    let status = if cancelled { "cancelled" } else { "completed" };
    // The write lock is released before `settle` runs: the writer is a plain
    // (non-reentrant) mutex, so taking it twice on this thread would deadlock.
    let closed = {
        let conn = pool.write().map_err(write_err)?;
        conn.execute(
            "UPDATE test_runs SET status = ?1, closed_by = ?2, finished_at = datetime('now') \
             WHERE run_id = ?3 AND status = 'running'",
            params![status, closed_by, run_id],
        )? > 0
    };
    if closed {
        // A manual run started by a schedule would otherwise stay 'started'
        // forever, keeping gate 1 closed on every following trigger.
        super::schedules::settle(pool, run_id, status);
    }
    Ok(closed)
}

/// Deletes a non-running run with its items and steps.
pub fn delete_run(pool: &DbPool, run_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM test_run_steps WHERE item_id IN \
         (SELECT item_id FROM test_run_items WHERE run_id = ?1)",
        params![run_id],
    )?;
    tx.execute(
        "DELETE FROM test_run_items WHERE run_id = ?1",
        params![run_id],
    )?;
    let n = tx.execute(
        "DELETE FROM test_runs WHERE run_id = ?1 AND status <> 'running'",
        params![run_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// Distinct non-empty assignees of a run's items (bulk notification target).
pub fn run_assignees(pool: &DbPool, run_id: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT assigned_to FROM test_run_items \
         WHERE run_id = ?1 AND assigned_to <> ''",
    )?;
    let rows = stmt.query_map(params![run_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Claims one item for `user_id` as a SINGLE atomic UPDATE…RETURNING — two
/// testers can never claim the same item (SQLite single-writer serializes the
/// statement; there is no separate SELECT to race). `item_id = None` claims
/// the nearest pending item that is unassigned or already assigned to the
/// caller; `Some` claims that specific item under the same guards.
pub fn claim_item(
    pool: &DbPool,
    run_id: &str,
    user_id: &str,
    item_id: Option<&str>,
) -> Result<Option<RunItemRecord>> {
    let conn = pool.write().map_err(write_err)?;
    let claimed_id: Option<String> = match item_id {
        Some(explicit) => conn
            .query_row(
                "UPDATE test_run_items SET status = 'in_progress', assigned_to = ?1, \
                    claimed_at = datetime('now') \
                 WHERE item_id = ?2 AND run_id = ?3 AND status = 'pending' \
                   AND (assigned_to = '' OR assigned_to = ?1) \
                   AND EXISTS (SELECT 1 FROM test_runs r \
                               WHERE r.run_id = ?3 AND r.status = 'running') \
                 RETURNING item_id",
                params![user_id, explicit, run_id],
                |row| row.get(0),
            )
            .optional()?,
        None => conn
            .query_row(
                "UPDATE test_run_items SET status = 'in_progress', assigned_to = ?1, \
                    claimed_at = datetime('now') \
                 WHERE item_id = (SELECT item_id FROM test_run_items \
                                  WHERE run_id = ?2 AND status = 'pending' \
                                    AND (assigned_to = '' OR assigned_to = ?1) \
                                  ORDER BY position LIMIT 1) \
                   AND EXISTS (SELECT 1 FROM test_runs r \
                               WHERE r.run_id = ?2 AND r.status = 'running') \
                 RETURNING item_id",
                params![user_id, run_id],
                |row| row.get(0),
            )
            .optional()?,
    };
    drop(conn);
    match claimed_id {
        Some(id) => get_run_item(pool, &id),
        None => Ok(None),
    }
}

/// The next claimable item AFTER a finish — same selection as the pool claim
/// but WITHOUT claiming (the tester decides whether to continue).
pub fn next_claimable(pool: &DbPool, run_id: &str, user_id: &str) -> Result<Option<RunItemRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let next: Option<String> = conn
        .query_row(
            "SELECT item_id FROM test_run_items \
             WHERE run_id = ?1 AND status = 'pending' \
               AND (assigned_to = '' OR assigned_to = ?2) \
             ORDER BY position LIMIT 1",
            params![run_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    drop(conn);
    match next {
        Some(id) => get_run_item(pool, &id),
        None => Ok(None),
    }
}

/// Releases an in-progress item back to pending. Pool runs clear the
/// assignee (the item returns to the pool); assigned runs keep it.
pub fn release_item(pool: &DbPool, item_id: &str, clear_assignee: bool) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = if clear_assignee {
        conn.execute(
            "UPDATE test_run_items SET status = 'pending', assigned_to = '', claimed_at = NULL \
             WHERE item_id = ?1 AND status = 'in_progress'",
            params![item_id],
        )?
    } else {
        conn.execute(
            "UPDATE test_run_items SET status = 'pending', claimed_at = NULL \
             WHERE item_id = ?1 AND status = 'in_progress'",
            params![item_id],
        )?
    };
    Ok(n > 0)
}

/// Records one step verdict of an in-progress item. The run must still be
/// running — the guard closes the claim-vs-close TOCTOU window.
pub fn set_step(
    pool: &DbPool,
    item_id: &str,
    step_index: u32,
    status: &str,
    note: &str,
    attachments_json: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE test_run_steps SET status = ?1, note = ?2, attachments_json = ?3 \
         WHERE item_id = ?4 AND step_index = ?5 \
           AND EXISTS (SELECT 1 FROM test_run_items i \
                       JOIN test_runs r ON r.run_id = i.run_id \
                       WHERE i.item_id = test_run_steps.item_id \
                         AND r.status = 'running')",
        params![status, note, attachments_json, item_id, step_index],
    )?;
    Ok(n > 0)
}

/// Derives the item verdict from its step results: fail > blocked > skip >
/// pass (an item with any failed step failed as a whole).
pub fn derive_item_status(pool: &DbPool, item_id: &str) -> Result<String> {
    let conn = pool.read().map_err(read_err)?;
    let (failed, blocked, skipped): (i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(status = 'failed'), 0), \
                COALESCE(SUM(status = 'blocked'), 0), \
                COALESCE(SUM(status = 'skipped'), 0) \
         FROM test_run_steps WHERE item_id = ?1",
        params![item_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(if failed > 0 {
        "failed"
    } else if blocked > 0 {
        "blocked"
    } else if skipped > 0 {
        "skipped"
    } else {
        "passed"
    }
    .to_string())
}

/// Finalizes an in-progress item with its verdict + execution metadata.
#[allow(clippy::too_many_arguments)]
pub fn finish_item(
    pool: &DbPool,
    item_id: &str,
    status: &str,
    result_note: &str,
    tester_config: &str,
    duration_secs: u32,
    attachments_json: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE test_run_items SET status = ?1, result_note = ?2, tester_config = ?3, \
            duration_secs = ?4, attachments_json = ?5, finished_at = datetime('now') \
         WHERE item_id = ?6 AND status = 'in_progress' \
           AND EXISTS (SELECT 1 FROM test_runs r \
                       WHERE r.run_id = test_run_items.run_id AND r.status = 'running')",
        params![
            status,
            result_note,
            tester_config,
            duration_secs as i64,
            attachments_json,
            item_id
        ],
    )?;
    Ok(n > 0)
}

/// Case-content extras (preconditions/test_data) of the version an item is
/// pinned to — read from the version history, NOT the live case row, so the
/// tester always executes the pinned snapshot.
pub fn item_pinned_content(pool: &DbPool, case_id: &str, version: u32) -> Result<(String, String)> {
    let conn = pool.read().map_err(read_err)?;
    let content: Option<String> = conn
        .query_row(
            "SELECT content_json FROM test_case_versions WHERE case_id = ?1 AND version = ?2",
            params![case_id, version as i64],
            |row| row.get(0),
        )
        .optional()?;
    let value = content
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .unwrap_or_default();
    Ok((
        value
            .get("preconditions")
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_string(),
        value
            .get("test_data")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
    ))
}

/// Cross-project "my work" counters for ONE project: per running run, the
/// caller's pending (assigned or claimable) and in-progress items.
pub fn my_work_rows(pool: &DbPool, user_id: &str) -> Result<Vec<(RunRecord, u32, u32)>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLS}, \
            (SELECT COUNT(*) FROM test_run_items i WHERE i.run_id = test_runs.run_id \
                AND i.status = 'pending' AND (i.assigned_to = ?1 OR i.assigned_to = '')), \
            (SELECT COUNT(*) FROM test_run_items i WHERE i.run_id = test_runs.run_id \
                AND i.status = 'in_progress' AND i.assigned_to = ?1) \
         FROM test_runs WHERE status = 'running' ORDER BY run_no DESC"
    ))?;
    let rows = stmt.query_map(params![user_id], |row| {
        Ok((
            read_run(row)?,
            row.get::<_, i64>(12)? as u32,
            row.get::<_, i64>(13)? as u32,
        ))
    })?;
    let all = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all
        .into_iter()
        .filter(|(_, pending, in_progress)| *pending > 0 || *in_progress > 0)
        .collect())
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::project_studio::tests::{
        create_case, update_case, CaseContentInput, CaseUpdateOutcome,
    };

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn approved_case(pool: &DbPool, title: &str, content: &str) -> String {
        let case_id = create_case(
            pool,
            &CaseContentInput {
                kind: "manual",
                title,
                priority: "high",
                content_json: content,
                tag_ids: &[],
                linked_source_ids: &[],
                attachments_json: "[]",
            },
            None,
            "",
            "author",
        )
        .expect("create case");
        let conn = pool.write().expect("write");
        conn.execute(
            "UPDATE test_cases SET status = 'approved' WHERE case_id = ?1",
            params![case_id],
        )
        .expect("approve");
        drop(conn);
        case_id
    }

    /// (b) Atomic pool claim: two users race for a single pool item; exactly
    /// one wins, the loser gets None.
    #[test]
    fn pool_claim_is_atomic_single_winner() {
        let pool = pool();
        let case_id = approved_case(
            &pool,
            "Claim me",
            r#"{"steps":[{"action":"a","expected":"e"}]}"#,
        );
        let snapshots = approved_case_snapshots(&pool, &[case_id]).expect("snapshots");
        let (run_id, _no) = create_run(
            &pool,
            "Run 1",
            "",
            "",
            "pool",
            &snapshots,
            &[String::new()],
            "creator",
        )
        .expect("create run");

        let first = claim_item(&pool, &run_id, "tester-a", None).expect("claim a");
        let second = claim_item(&pool, &run_id, "tester-b", None).expect("claim b");
        let winner = first.expect("tester-a claims the only item");
        assert_eq!(winner.status, "in_progress");
        assert_eq!(winner.assigned_to, "tester-a");
        assert!(second.is_none(), "second claim must find nothing");
    }

    /// (e) Run snapshots are immune to later case edits: the item keeps the
    /// pinned version, title and copied steps.
    #[test]
    fn run_snapshot_survives_case_edit() {
        let pool = pool();
        let case_id = approved_case(
            &pool,
            "Original title",
            r#"{"steps":[{"action":"old action","expected":"old expected"}]}"#,
        );
        let snapshots =
            approved_case_snapshots(&pool, std::slice::from_ref(&case_id)).expect("snapshots");
        let (run_id, _no) = create_run(
            &pool,
            "Run snap",
            "",
            "",
            "pool",
            &snapshots,
            &[String::new()],
            "creator",
        )
        .expect("create run");

        // Edit the case afterwards (status must be editable → flip to draft).
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "UPDATE test_cases SET status = 'draft' WHERE case_id = ?1",
                params![case_id],
            )
            .expect("back to draft");
        }
        let out = update_case(
            &pool,
            &case_id,
            1,
            &CaseContentInput {
                kind: "manual",
                title: "Edited title",
                priority: "low",
                content_json: r#"{"steps":[{"action":"NEW action","expected":"NEW expected"}]}"#,
                tag_ids: &[],
                linked_source_ids: &[],
                attachments_json: "[]",
            },
            "edit after run",
            "author",
        )
        .expect("edit");
        assert_eq!(out, CaseUpdateOutcome::Saved(2));

        let items = list_run_items(&pool, &run_id).expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].case_title, "Original title");
        assert_eq!(items[0].case_version, 1);
        let steps = list_item_steps(&pool, &items[0].item_id).expect("steps");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].action, "old action");
        assert_eq!(steps[0].expected, "old expected");
    }

    /// Closing a run must fence out concurrent mutations: claim, set_step and
    /// finish_item all refuse once the run is no longer 'running'.
    #[test]
    fn closed_run_refuses_claim_step_and_finish() {
        let pool = pool();
        let case_a = approved_case(
            &pool,
            "Case A",
            r#"{"steps":[{"action":"a","expected":"e"}]}"#,
        );
        let case_b = approved_case(
            &pool,
            "Case B",
            r#"{"steps":[{"action":"b","expected":"e"}]}"#,
        );
        let snapshots = approved_case_snapshots(&pool, &[case_a, case_b]).expect("snapshots");
        let (run_id, _no) = create_run(
            &pool,
            "Run close",
            "",
            "",
            "pool",
            &snapshots,
            &[String::new(), String::new()],
            "creator",
        )
        .expect("create run");

        // One item goes in_progress before the run closes.
        let claimed = claim_item(&pool, &run_id, "tester-a", None)
            .expect("claim")
            .expect("first claim wins");
        assert!(close_run(&pool, &run_id, true, "manager").expect("close"));

        let late_claim = claim_item(&pool, &run_id, "tester-b", None).expect("late claim");
        assert!(late_claim.is_none(), "claim after close must refuse");
        assert!(
            !set_step(&pool, &claimed.item_id, 0, "passed", "", "[]").expect("set_step"),
            "set_step after close must refuse"
        );
        assert!(
            !finish_item(&pool, &claimed.item_id, "passed", "", "", 5, "[]").expect("finish"),
            "finish after close must refuse"
        );
        let items = list_run_items(&pool, &run_id).expect("items");
        let item = items
            .iter()
            .find(|i| i.item_id == claimed.item_id)
            .expect("item");
        assert_eq!(item.status, "in_progress", "verdict must not be recorded");
    }
}
