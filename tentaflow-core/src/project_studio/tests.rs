// ===== File: project_studio/tests.rs — manual test cases, versions, tags and suites (F2) =====
//
// SQL layer for the manual-testing module: cases with append-only version
// history and optimistic locking, case tags, test suites and the CSV import
// parser. Authorization gates live in `dispatch/project_studio.rs`; every
// function here operates on an already-authorized per-project pool.
//
// VISIBILITY INVARIANT: cases with `review_state = 'pending'` (agent output
// awaiting review) are excluded from EVERY query through the single
// `VISIBLE_CASES_PREDICATE` — list, get, pickers, run creation and reports all
// share it, so unreviewed agent output can never leak outside the generation
// review screen.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use super::models::{
    CaseListItem, CaseVersionRecord, RunCounts, RunRecord, SuiteRecord, TestCaseRecord,
};
use crate::db::DbPool;

/// The one predicate hiding unreviewed agent output. Append with `AND` to the
/// WHERE clause of every query that touches `test_cases`.
pub const VISIBLE_CASES_PREDICATE: &str = "review_state <> 'pending'";

pub const CASE_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];
pub const CASE_STATUSES: &[&str] = &["draft", "review", "approved", "deprecated"];

/// CSV import hard clamps (server-side, before any parsing work).
pub const CSV_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const CSV_MAX_ROWS: usize = 500;
pub const CSV_MAX_STEPS_PER_ROW: usize = 50;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tests read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tests write: {e}")
}

fn read_case(row: &rusqlite::Row<'_>) -> rusqlite::Result<TestCaseRecord> {
    Ok(TestCaseRecord {
        case_id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        priority: row.get(3)?,
        status: row.get(4)?,
        status_reason: row.get(5)?,
        review_state: row.get(6)?,
        origin: row.get(7)?,
        generation_run_id: row.get(8)?,
        linked_sources_json: row.get(9)?,
        attachments_json: row.get(10)?,
        language: row.get(11)?,
        current_version: row.get::<_, i64>(12)? as u32,
        content_json: row.get(13)?,
        created_by: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

const CASE_COLS: &str = "case_id, kind, title, priority, status, status_reason, review_state, \
     origin, generation_run_id, linked_sources_json, attachments_json, language, \
     current_version, content_json, created_by, created_at, updated_at";

/// Filters of the case list (empty string = no filter).
#[derive(Debug, Default)]
pub struct CaseFilters<'a> {
    pub kind: &'a str,
    pub status: &'a str,
    pub priority: &'a str,
    pub tag_id: &'a str,
    pub origin: &'a str,
    pub search: &'a str,
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Lists visible cases with filters + keyset-free offset pagination. Returns
/// `(rows, total)`; each row carries its tag ids and the latest run-item
/// verdict.
pub fn list_cases(
    pool: &DbPool,
    filters: &CaseFilters<'_>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<CaseListItem>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let mut clauses: Vec<String> = vec![VISIBLE_CASES_PREDICATE.to_string()];
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for (column, value) in [
        ("kind", filters.kind),
        ("status", filters.status),
        ("priority", filters.priority),
        ("origin", filters.origin),
    ] {
        if !value.is_empty() {
            clauses.push(format!("{column} = ?{}", args.len() + 1));
            args.push(Box::new(value.to_string()));
        }
    }
    if !filters.tag_id.is_empty() {
        clauses.push(format!(
            "case_id IN (SELECT case_id FROM case_tags WHERE tag_id = ?{})",
            args.len() + 1
        ));
        args.push(Box::new(filters.tag_id.to_string()));
    }
    if !filters.search.trim().is_empty() {
        clauses.push(format!("title LIKE ?{} ESCAPE '\\'", args.len() + 1));
        args.push(Box::new(format!(
            "%{}%",
            escape_like(filters.search.trim())
        )));
    }
    let where_sql = clauses.join(" AND ");

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM test_cases WHERE {where_sql}"),
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        |row| row.get(0),
    )?;

    let sql = format!(
        "SELECT {CASE_COLS} FROM test_cases WHERE {where_sql} \
         ORDER BY updated_at DESC, case_id LIMIT ?{} OFFSET ?{}",
        args.len() + 1,
        args.len() + 2
    );
    args.push(Box::new(limit as i64));
    args.push(Box::new(offset as i64));
    let mut stmt = conn.prepare(&sql)?;
    let records = stmt
        .query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            read_case,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let tag_ids = case_tag_ids(&conn, &record.case_id)?;
        let last_result = last_case_result(&conn, &record.case_id)?;
        out.push(CaseListItem {
            record,
            tag_ids,
            last_result,
        });
    }
    Ok((out, total as u32))
}

fn case_tag_ids(conn: &Connection, case_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag_id FROM case_tags WHERE case_id = ?1 ORDER BY tag_id")?;
    let rows = stmt.query_map(params![case_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Latest recorded verdict of the case across all runs (`None` = never
/// executed to a terminal item status).
fn last_case_result(conn: &Connection, case_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT status FROM test_run_items \
         WHERE case_id = ?1 AND status IN ('passed','failed','blocked','skipped') \
         ORDER BY finished_at DESC LIMIT 1",
        params![case_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Fetches one VISIBLE case (pending agent output answers as absent).
pub fn get_case(pool: &DbPool, case_id: &str) -> Result<Option<CaseListItem>> {
    let conn = pool.read().map_err(read_err)?;
    let record = conn
        .query_row(
            &format!(
                "SELECT {CASE_COLS} FROM test_cases \
                 WHERE case_id = ?1 AND {VISIBLE_CASES_PREDICATE}"
            ),
            params![case_id],
            read_case,
        )
        .optional()?;
    match record {
        Some(record) => {
            let tag_ids = case_tag_ids(&conn, &record.case_id)?;
            let last_result = last_case_result(&conn, &record.case_id)?;
            Ok(Some(CaseListItem {
                record,
                tag_ids,
                last_result,
            }))
        }
        None => Ok(None),
    }
}

pub fn list_versions(pool: &DbPool, case_id: &str) -> Result<Vec<CaseVersionRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT version, content_json, change_note, created_by, created_at \
         FROM test_case_versions WHERE case_id = ?1 ORDER BY version DESC",
    )?;
    let rows = stmt.query_map(params![case_id], |row| {
        Ok(CaseVersionRecord {
            version: row.get::<_, i64>(0)? as u32,
            content_json: row.get(1)?,
            change_note: row.get(2)?,
            created_by: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_version(
    pool: &DbPool,
    case_id: &str,
    version: u32,
) -> Result<Option<CaseVersionRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT version, content_json, change_note, created_by, created_at \
         FROM test_case_versions WHERE case_id = ?1 AND version = ?2",
        params![case_id, version as i64],
        |row| {
            Ok(CaseVersionRecord {
                version: row.get::<_, i64>(0)? as u32,
                content_json: row.get(1)?,
                change_note: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Finds an existing tag by name (NOCASE) or creates it. Shared by CSV import
/// and the generation tool (lazily-created tags).
pub fn ensure_tag(conn: &Connection, name: &str, created_by: &str) -> Result<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT tag_id FROM tags WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO tags (tag_id, name, created_by) VALUES (?1, ?2, ?3)",
        params![id, name, created_by],
    )?;
    Ok(id)
}

fn replace_case_tags(conn: &Connection, case_id: &str, tag_ids: &[String]) -> Result<()> {
    conn.execute("DELETE FROM case_tags WHERE case_id = ?1", params![case_id])?;
    for tag_id in tag_ids {
        conn.execute(
            "INSERT OR IGNORE INTO case_tags (case_id, tag_id) VALUES (?1, ?2)",
            params![case_id, tag_id],
        )?;
    }
    Ok(())
}

/// Content payload of a new case / new version.
#[derive(Debug)]
pub struct CaseContentInput<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub priority: &'a str,
    pub content_json: &'a str,
    pub tag_ids: &'a [String],
    pub linked_source_ids: &'a [String],
    pub attachments_json: &'a str,
}

/// Extra provenance for agent-generated cases (origin='agent',
/// review_state='pending').
#[derive(Debug)]
pub struct AgentProvenance<'a> {
    pub generation_run_id: &'a str,
    pub provenance_json: &'a str,
}

/// Creates a new case (v1). `agent` switches the row to unreviewed agent
/// output. Runs in ONE transaction: case row + version v1 + tag links.
pub fn create_case(
    pool: &DbPool,
    input: &CaseContentInput<'_>,
    agent: Option<&AgentProvenance<'_>>,
    change_note: &str,
    created_by: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let case_id = uuid::Uuid::new_v4().to_string();
    let (origin, review_state, generation_run_id, provenance_json) = match agent {
        Some(a) => ("agent", "pending", a.generation_run_id, a.provenance_json),
        None => ("user", "", "", "{}"),
    };
    tx.execute(
        "INSERT INTO test_cases (case_id, kind, title, priority, origin, review_state, \
            generation_run_id, provenance_json, linked_sources_json, attachments_json, \
            content_json, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            case_id,
            input.kind,
            input.title,
            input.priority,
            origin,
            review_state,
            generation_run_id,
            provenance_json,
            serde_json::to_string(input.linked_source_ids)?,
            input.attachments_json,
            input.content_json,
            created_by
        ],
    )?;
    tx.execute(
        "INSERT INTO test_case_versions (case_id, version, content_json, change_note, created_by) \
         VALUES (?1, 1, ?2, ?3, ?4)",
        params![case_id, input.content_json, change_note, created_by],
    )?;
    replace_case_tags(&tx, &case_id, input.tag_ids)?;
    tx.commit()?;
    Ok(case_id)
}

/// Outcome of an optimistic-locking mutation on a case.
#[derive(Debug, PartialEq, Eq)]
pub enum CaseUpdateOutcome {
    /// Saved; carries the resulting current version.
    Saved(u32),
    /// `expected_version` was stale — the caller must reload.
    Conflict,
    /// Case absent (or pending — invisible outside review).
    NotFound,
    /// Case status forbids editing (only draft/review are editable).
    NotEditable,
}

/// Edits a case under optimistic locking. A NEW version row is appended ONLY
/// when `content_json` actually changed; metadata-only edits keep the version.
/// The lock is a single conditional UPDATE (`WHERE current_version = expected`)
/// — 0 affected rows = conflict, no read-then-write race.
pub fn update_case(
    pool: &DbPool,
    case_id: &str,
    expected_version: u32,
    input: &CaseContentInput<'_>,
    change_note: &str,
    actor: &str,
) -> Result<CaseUpdateOutcome> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let current: Option<(String, String, i64)> = tx
        .query_row(
            &format!(
                "SELECT status, content_json, current_version FROM test_cases \
                 WHERE case_id = ?1 AND {VISIBLE_CASES_PREDICATE}"
            ),
            params![case_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((status, stored_content, _current_version)) = current else {
        return Ok(CaseUpdateOutcome::NotFound);
    };
    if status != "draft" && status != "review" {
        return Ok(CaseUpdateOutcome::NotEditable);
    }
    let content_changed = stored_content != input.content_json;
    let new_version = if content_changed {
        expected_version + 1
    } else {
        expected_version
    };
    let affected = tx.execute(
        "UPDATE test_cases SET title = ?1, priority = ?2, content_json = ?3, \
            linked_sources_json = ?4, attachments_json = ?5, current_version = ?6, \
            updated_at = datetime('now') \
         WHERE case_id = ?7 AND current_version = ?8",
        params![
            input.title,
            input.priority,
            input.content_json,
            serde_json::to_string(input.linked_source_ids)?,
            input.attachments_json,
            new_version as i64,
            case_id,
            expected_version as i64
        ],
    )?;
    if affected == 0 {
        return Ok(CaseUpdateOutcome::Conflict);
    }
    if content_changed {
        tx.execute(
            "INSERT INTO test_case_versions (case_id, version, content_json, change_note, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![case_id, new_version as i64, input.content_json, change_note, actor],
        )?;
    }
    replace_case_tags(&tx, case_id, input.tag_ids)?;
    tx.commit()?;
    Ok(CaseUpdateOutcome::Saved(new_version))
}

/// Restores an old version as a NEW head version (append-only history).
pub fn restore_version(
    pool: &DbPool,
    case_id: &str,
    version: u32,
    expected_version: u32,
    actor: &str,
) -> Result<CaseUpdateOutcome> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let exists: Option<String> = tx
        .query_row(
            &format!(
                "SELECT status FROM test_cases WHERE case_id = ?1 AND {VISIBLE_CASES_PREDICATE}"
            ),
            params![case_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(status) = exists else {
        return Ok(CaseUpdateOutcome::NotFound);
    };
    if status != "draft" && status != "review" {
        return Ok(CaseUpdateOutcome::NotEditable);
    }
    let old_content: Option<String> = tx
        .query_row(
            "SELECT content_json FROM test_case_versions WHERE case_id = ?1 AND version = ?2",
            params![case_id, version as i64],
            |row| row.get(0),
        )
        .optional()?;
    let Some(content) = old_content else {
        return Ok(CaseUpdateOutcome::NotFound);
    };
    let new_version = expected_version + 1;
    let affected = tx.execute(
        "UPDATE test_cases SET content_json = ?1, current_version = ?2, \
            updated_at = datetime('now') \
         WHERE case_id = ?3 AND current_version = ?4",
        params![
            content,
            new_version as i64,
            case_id,
            expected_version as i64
        ],
    )?;
    if affected == 0 {
        return Ok(CaseUpdateOutcome::Conflict);
    }
    tx.execute(
        "INSERT INTO test_case_versions (case_id, version, content_json, change_note, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            case_id,
            new_version as i64,
            content,
            format!("restored from v{version}"),
            actor
        ],
    )?;
    tx.commit()?;
    Ok(CaseUpdateOutcome::Saved(new_version))
}

/// Requirement of a status transition: the minimum project role and whether a
/// reason is mandatory (every downgrade). `None` = transition not allowed.
pub fn transition_requirement(from: &str, to: &str) -> Option<(super::models::ProjectRole, bool)> {
    use super::models::ProjectRole::{Editor, Manager};
    match (from, to) {
        ("draft", "review") => Some((Editor, false)),
        ("review", "approved") => Some((Manager, false)),
        ("approved", "deprecated") => Some((Manager, false)),
        // Downgrades — always with a reason.
        ("review", "draft") => Some((Editor, true)),
        ("approved", "review") | ("approved", "draft") => Some((Manager, true)),
        ("deprecated", "draft") => Some((Manager, true)),
        _ => None,
    }
}

/// Applies a status change guarded by the FROM status (idempotent-safe under
/// concurrency). Never touches `current_version` (risk F.1).
pub fn set_case_status(
    pool: &DbPool,
    case_id: &str,
    from: &str,
    to: &str,
    reason: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        &format!(
            "UPDATE test_cases SET status = ?1, status_reason = ?2, updated_at = datetime('now') \
             WHERE case_id = ?3 AND status = ?4 AND {VISIBLE_CASES_PREDICATE}"
        ),
        params![to, reason, case_id, from],
    )?;
    Ok(n > 0)
}

/// Duplicates a visible case as a fresh user draft (v1 = current content).
pub fn duplicate_case(pool: &DbPool, case_id: &str, actor: &str) -> Result<Option<String>> {
    let source = match get_case(pool, case_id)? {
        Some(item) => item,
        None => return Ok(None),
    };
    let title = format!("{} (kopia)", source.record.title);
    let title = if title.chars().count() > 200 {
        title.chars().take(200).collect()
    } else {
        title
    };
    let linked: Vec<String> =
        serde_json::from_str(&source.record.linked_sources_json).unwrap_or_default();
    let new_id = create_case(
        pool,
        &CaseContentInput {
            kind: &source.record.kind,
            title: &title,
            priority: &source.record.priority,
            content_json: &source.record.content_json,
            tag_ids: &source.tag_ids,
            linked_source_ids: &linked,
            attachments_json: &source.record.attachments_json,
        },
        None,
        &format!("duplicated from {case_id}"),
        actor,
    )?;
    Ok(Some(new_id))
}

/// How many run items reference the case — a referenced case cannot be
/// deleted (deprecate instead), running snapshots must stay resolvable.
pub fn case_run_item_refs(pool: &DbPool, case_id: &str) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_run_items WHERE case_id = ?1",
        params![case_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

/// Removes a case with its versions, tag links and suite memberships.
pub fn delete_case(pool: &DbPool, case_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM test_case_versions WHERE case_id = ?1",
        params![case_id],
    )?;
    tx.execute("DELETE FROM case_tags WHERE case_id = ?1", params![case_id])?;
    tx.execute(
        "DELETE FROM suite_cases WHERE case_id = ?1",
        params![case_id],
    )?;
    let n = tx.execute(
        "DELETE FROM test_cases WHERE case_id = ?1",
        params![case_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

// =============================================================================
// CSV import — `title;priority;preconditions;steps;test_data;tags`,
// steps encoded `action=>expected||action=>expected`.
// =============================================================================

#[derive(Debug)]
pub struct CsvRow {
    pub title: String,
    pub priority: String,
    pub content_json: String,
    pub tags: Vec<String>,
}

/// Parses the CSV text into rows + per-line errors (1-based line numbers).
/// The caller enforces the byte clamp before calling; the row and per-row
/// step clamps are enforced here. There is deliberately NO quoting support:
/// a literal `;` inside a field shifts the columns and fails that row.
pub fn parse_csv(text: &str) -> (Vec<CsvRow>, Vec<(u32, String)>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut data_rows = 0usize;
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        // Optional header row.
        if idx == 0 && line.to_ascii_lowercase().starts_with("title;") {
            continue;
        }
        data_rows += 1;
        if data_rows > CSV_MAX_ROWS {
            errors.push((line_no, format!("row limit exceeded (max {CSV_MAX_ROWS})")));
            break;
        }
        let fields: Vec<&str> = line.split(';').collect();
        if fields.len() < 2 {
            errors.push((
                line_no,
                "expected `title;priority;preconditions;steps;test_data;tags`".into(),
            ));
            continue;
        }
        let title = fields[0].trim();
        if title.is_empty() || title.chars().count() > 200 {
            errors.push((line_no, "title must be 1..200 characters".into()));
            continue;
        }
        let priority = fields[1].trim().to_ascii_lowercase();
        if !CASE_PRIORITIES.contains(&priority.as_str()) {
            errors.push((line_no, format!("unknown priority '{priority}'")));
            continue;
        }
        let preconditions = fields.get(2).map(|s| s.trim()).unwrap_or("");
        let steps_raw = fields.get(3).map(|s| s.trim()).unwrap_or("");
        let test_data = fields.get(4).map(|s| s.trim()).unwrap_or("");
        let tags_raw = fields.get(5).map(|s| s.trim()).unwrap_or("");

        let mut steps = Vec::new();
        let mut step_error = None;
        if !steps_raw.is_empty() {
            for (i, chunk) in steps_raw.split("||").enumerate() {
                if i >= CSV_MAX_STEPS_PER_ROW {
                    step_error = Some(format!("too many steps (max {CSV_MAX_STEPS_PER_ROW})"));
                    break;
                }
                let (action, expected) = match chunk.split_once("=>") {
                    Some((a, e)) => (a.trim(), e.trim()),
                    None => (chunk.trim(), ""),
                };
                if action.is_empty() {
                    step_error = Some(format!("step {} has an empty action", i + 1));
                    break;
                }
                steps.push(serde_json::json!({ "action": action, "expected": expected }));
            }
        }
        if let Some(message) = step_error {
            errors.push((line_no, message));
            continue;
        }
        let tags: Vec<String> = tags_raw
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let content_json = serde_json::json!({
            "preconditions": preconditions,
            "steps": steps,
            "test_data": test_data,
        })
        .to_string();
        rows.push(CsvRow {
            title: title.to_string(),
            priority,
            content_json,
            tags,
        });
    }
    (rows, errors)
}

/// Writes all parsed CSV rows in ONE transaction (all-or-nothing) with lazily
/// created tags. Returns the number of created cases.
pub fn import_cases(pool: &DbPool, rows: &[CsvRow], created_by: &str) -> Result<u32> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let mut created = 0u32;
    for row in rows {
        let case_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO test_cases (case_id, kind, title, priority, content_json, created_by) \
             VALUES (?1, 'manual', ?2, ?3, ?4, ?5)",
            params![
                case_id,
                row.title,
                row.priority,
                row.content_json,
                created_by
            ],
        )?;
        tx.execute(
            "INSERT INTO test_case_versions (case_id, version, content_json, change_note, created_by) \
             VALUES (?1, 1, ?2, 'CSV import', ?3)",
            params![case_id, row.content_json, created_by],
        )?;
        for tag in &row.tags {
            let tag_id = ensure_tag(&tx, tag, created_by)?;
            tx.execute(
                "INSERT OR IGNORE INTO case_tags (case_id, tag_id) VALUES (?1, ?2)",
                params![case_id, tag_id],
            )?;
        }
        created += 1;
    }
    tx.commit()?;
    Ok(created)
}

// =============================================================================
// Suites
// =============================================================================

fn read_suite(row: &rusqlite::Row<'_>) -> rusqlite::Result<SuiteRecord> {
    Ok(SuiteRecord {
        suite_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

const SUITE_COLS: &str = "suite_id, name, description, created_at, updated_at";

/// Suite row + list aggregates (visible case count, deprecated flag, last run).
pub struct SuiteListItem {
    pub record: SuiteRecord,
    pub case_count: u32,
    pub has_deprecated: bool,
    pub last_run: Option<(RunRecord, RunCounts)>,
}

fn suite_aggregates(conn: &Connection, suite_id: &str) -> Result<(u32, bool)> {
    let (count, deprecated): (i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(c.status = 'deprecated'), 0) \
             FROM suite_cases sc JOIN test_cases c ON c.case_id = sc.case_id \
             WHERE sc.suite_id = ?1 AND c.{VISIBLE_CASES_PREDICATE}"
        ),
        params![suite_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((count as u32, deprecated > 0))
}

pub fn list_suites(pool: &DbPool) -> Result<Vec<SuiteListItem>> {
    let conn = pool.read().map_err(read_err)?;
    let records = {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUITE_COLS} FROM test_suites ORDER BY name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([], read_suite)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let (case_count, has_deprecated) = suite_aggregates(&conn, &record.suite_id)?;
        let last_run = super::runs::latest_run_for_suite(&conn, &record.suite_id)?;
        out.push(SuiteListItem {
            record,
            case_count,
            has_deprecated,
            last_run,
        });
    }
    Ok(out)
}

pub fn get_suite(pool: &DbPool, suite_id: &str) -> Result<Option<SuiteListItem>> {
    let conn = pool.read().map_err(read_err)?;
    let record = conn
        .query_row(
            &format!("SELECT {SUITE_COLS} FROM test_suites WHERE suite_id = ?1"),
            params![suite_id],
            read_suite,
        )
        .optional()?;
    match record {
        Some(record) => {
            let (case_count, has_deprecated) = suite_aggregates(&conn, &record.suite_id)?;
            let last_run = super::runs::latest_run_for_suite(&conn, &record.suite_id)?;
            Ok(Some(SuiteListItem {
                record,
                case_count,
                has_deprecated,
                last_run,
            }))
        }
        None => Ok(None),
    }
}

/// Ordered case reference of a suite (list for the suite editor).
pub struct SuiteCaseRow {
    pub case_id: String,
    pub position: u32,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub priority: String,
}

pub fn suite_case_rows(pool: &DbPool, suite_id: &str) -> Result<Vec<SuiteCaseRow>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT sc.case_id, sc.position, c.title, c.kind, c.status, c.priority \
         FROM suite_cases sc JOIN test_cases c ON c.case_id = sc.case_id \
         WHERE sc.suite_id = ?1 AND c.{VISIBLE_CASES_PREDICATE} \
         ORDER BY sc.position"
    ))?;
    let rows = stmt.query_map(params![suite_id], |row| {
        Ok(SuiteCaseRow {
            case_id: row.get(0)?,
            position: row.get::<_, i64>(1)? as u32,
            title: row.get(2)?,
            kind: row.get(3)?,
            status: row.get(4)?,
            priority: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates or updates a suite; `case_ids` order defines member positions.
/// Every case must exist and be visible (not pending) — an unknown id fails
/// the whole save. UNIQUE COLLATE NOCASE clashes bubble up with "UNIQUE" in
/// the message (dispatcher maps them to BadRequest).
pub fn save_suite(
    pool: &DbPool,
    suite_id: Option<&str>,
    name: &str,
    description: &str,
    case_ids: &[String],
    actor: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    for case_id in case_ids {
        let visible: Option<i64> = tx
            .query_row(
                &format!(
                    "SELECT 1 FROM test_cases WHERE case_id = ?1 AND {VISIBLE_CASES_PREDICATE}"
                ),
                params![case_id],
                |row| row.get(0),
            )
            .optional()?;
        if visible.is_none() {
            bail!("unknown case '{case_id}'");
        }
    }
    let suite_id = match suite_id {
        Some(id) => {
            let n = tx.execute(
                "UPDATE test_suites SET name = ?1, description = ?2, updated_at = datetime('now') \
                 WHERE suite_id = ?3",
                params![name, description, id],
            )?;
            if n == 0 {
                bail!("suite not found");
            }
            tx.execute("DELETE FROM suite_cases WHERE suite_id = ?1", params![id])?;
            id.to_string()
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO test_suites (suite_id, name, description, created_by) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, name, description, actor],
            )?;
            id
        }
    };
    for (position, case_id) in case_ids.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO suite_cases (suite_id, case_id, position) VALUES (?1, ?2, ?3)",
            params![suite_id, case_id, position as i64],
        )?;
    }
    tx.commit()?;
    Ok(suite_id)
}

/// Deletes a suite. Runs that referenced it keep their snapshots (suite_id
/// stays on the run row; the name resolution then yields '').
pub fn delete_suite(pool: &DbPool, suite_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM suite_cases WHERE suite_id = ?1",
        params![suite_id],
    )?;
    let n = tx.execute(
        "DELETE FROM test_suites WHERE suite_id = ?1",
        params![suite_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// Resolves suite names for a set of ids (missing suite → absent key).
pub fn suite_names(
    pool: &DbPool,
    suite_ids: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut out = std::collections::HashMap::new();
    for id in suite_ids {
        if id.is_empty() || out.contains_key(id) {
            continue;
        }
        let name: Option<String> = conn
            .query_row(
                "SELECT name FROM test_suites WHERE suite_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(name) = name {
            out.insert(id.clone(), name);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        // Leak the tempdir so the SQLite file outlives the test body.
        std::mem::forget(tmp);
        pool
    }

    fn sample_input<'a>(title: &'a str, content: &'a str) -> CaseContentInput<'a> {
        CaseContentInput {
            kind: "manual",
            title,
            priority: "medium",
            content_json: content,
            tag_ids: &[],
            linked_source_ids: &[],
            attachments_json: "[]",
        }
    }

    /// (a) Optimistic locking: a stale expected_version yields Conflict and a
    /// content change bumps the version exactly once.
    #[test]
    fn optimistic_locking_conflicts_on_stale_version() {
        let pool = pool();
        let case_id = create_case(
            &pool,
            &sample_input("Case A", "{\"steps\":[]}"),
            None,
            "",
            "u1",
        )
        .expect("create");

        // Content change on the correct expected version → v2.
        let out = update_case(
            &pool,
            &case_id,
            1,
            &sample_input(
                "Case A",
                "{\"steps\":[{\"action\":\"x\",\"expected\":\"y\"}]}",
            ),
            "edit",
            "u1",
        )
        .expect("update");
        assert_eq!(out, CaseUpdateOutcome::Saved(2));

        // A second writer still holding expected_version=1 → Conflict.
        let out = update_case(
            &pool,
            &case_id,
            1,
            &sample_input("Case A stale", "{\"steps\":[]}"),
            "stale edit",
            "u2",
        )
        .expect("update stale");
        assert_eq!(out, CaseUpdateOutcome::Conflict);

        // Metadata-only edit (same content) does NOT append a version.
        let out = update_case(
            &pool,
            &case_id,
            2,
            &sample_input(
                "Case A renamed",
                "{\"steps\":[{\"action\":\"x\",\"expected\":\"y\"}]}",
            ),
            "rename",
            "u1",
        )
        .expect("rename");
        assert_eq!(out, CaseUpdateOutcome::Saved(2));
        let versions = list_versions(&pool, &case_id).expect("versions");
        assert_eq!(versions.len(), 2, "no version row for metadata-only edit");
    }

    /// (c) The shared visibility predicate hides pending agent output from the
    /// list, the getter and the suite pickers.
    #[test]
    fn pending_cases_excluded_by_shared_predicate() {
        let pool = pool();
        let visible = create_case(&pool, &sample_input("Visible", "{}"), None, "", "u1")
            .expect("create visible");
        let pending = create_case(
            &pool,
            &sample_input("Pending agent output", "{}"),
            Some(&AgentProvenance {
                generation_run_id: "gen-1",
                provenance_json: "{}",
            }),
            "",
            "u1",
        )
        .expect("create pending");

        let (rows, total) = list_cases(&pool, &CaseFilters::default(), 0, 50).expect("list");
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].record.case_id, visible);

        assert!(get_case(&pool, &pending).expect("get pending").is_none());
        assert!(get_case(&pool, &visible).expect("get visible").is_some());

        // Suite save refuses a pending member (picker parity).
        let err = save_suite(&pool, None, "S1", "", &[pending.clone()], "u1")
            .expect_err("pending case must not join a suite");
        assert!(err.to_string().contains("unknown case"));
    }

    #[test]
    fn csv_parser_validates_rows() {
        let (rows, errors) = parse_csv(
            "title;priority;preconditions;steps;test_data;tags\n\
             Login works;high;user exists;open page=>form shown||submit=>logged in;u/p;smoke,auth\n\
             ;low;;;;\n\
             Bad prio;urgent;;;;",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Login works");
        assert_eq!(rows[0].tags, vec!["smoke".to_string(), "auth".to_string()]);
        let content: serde_json::Value = serde_json::from_str(&rows[0].content_json).unwrap();
        assert_eq!(content["steps"].as_array().unwrap().len(), 2);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].0, 3);
        assert_eq!(errors[1].0, 4);
    }

    #[test]
    fn csv_parser_caps_steps_per_row() {
        let steps = vec!["a=>e"; CSV_MAX_STEPS_PER_ROW + 1].join("||");
        let (rows, errors) = parse_csv(&format!("Too many;high;;{steps};;"));
        assert!(rows.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].1.contains("too many steps"));

        let steps = vec!["a=>e"; CSV_MAX_STEPS_PER_ROW].join("||");
        let (rows, errors) = parse_csv(&format!("At the cap;high;;{steps};;"));
        assert_eq!(rows.len(), 1);
        assert!(errors.is_empty());
    }
}
