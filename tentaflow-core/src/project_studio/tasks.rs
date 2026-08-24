// ===== File: project_studio/tasks.rs — tasks / defects with comments (F2) =====
//
// SQL layer for the task board: tasks and defects (severity-bearing) with a
// per-project sequential `task_no`, cross-object links (`links_json`),
// attachments and threaded comments. Authorization gates live in the
// dispatcher.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{TaskCommentRecord, TaskRecord};
use crate::db::DbPool;

pub const TASK_TYPES: &[&str] = &["task", "defect"];
pub const TASK_STATUSES: &[&str] = &["todo", "in_progress", "review", "done"];
pub const TASK_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];
pub const TASK_SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tasks read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tasks write: {e}")
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn read_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        task_id: row.get(0)?,
        task_no: row.get::<_, i64>(1)? as u32,
        task_type: row.get(2)?,
        title: row.get(3)?,
        description_md: row.get(4)?,
        severity: row.get(5)?,
        priority: row.get(6)?,
        status: row.get(7)?,
        assigned_to: row.get(8)?,
        due_date: row.get(9)?,
        links_json: row.get(10)?,
        attachments_json: row.get(11)?,
        comment_count: row.get::<_, i64>(12)? as u32,
        created_by: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

const TASK_COLS: &str = "t.task_id, t.task_no, t.task_type, t.title, t.description_md, \
     t.severity, t.priority, t.status, t.assigned_to, t.due_date, t.links_json, \
     t.attachments_json, \
     (SELECT COUNT(*) FROM task_comments c WHERE c.task_id = t.task_id), \
     t.created_by, t.created_at, t.updated_at";

#[derive(Debug, Default)]
pub struct TaskFilters<'a> {
    pub task_type: &'a str,
    pub status: &'a str,
    pub assigned_to: &'a str,
    pub search: &'a str,
    pub severity: &'a str,
}

pub fn list_tasks(
    pool: &DbPool,
    filters: &TaskFilters<'_>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<TaskRecord>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let mut clauses: Vec<String> = vec!["1=1".to_string()];
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for (column, value) in [
        ("t.task_type", filters.task_type),
        ("t.status", filters.status),
        ("t.assigned_to", filters.assigned_to),
        ("t.severity", filters.severity),
    ] {
        if !value.is_empty() {
            clauses.push(format!("{column} = ?{}", args.len() + 1));
            args.push(Box::new(value.to_string()));
        }
    }
    if !filters.search.trim().is_empty() {
        clauses.push(format!("t.title LIKE ?{} ESCAPE '\\'", args.len() + 1));
        args.push(Box::new(format!(
            "%{}%",
            escape_like(filters.search.trim())
        )));
    }
    let where_sql = clauses.join(" AND ");
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM tasks t WHERE {where_sql}"),
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        |row| row.get(0),
    )?;
    let sql = format!(
        "SELECT {TASK_COLS} FROM tasks t WHERE {where_sql} \
         ORDER BY t.task_no DESC LIMIT ?{} OFFSET ?{}",
        args.len() + 1,
        args.len() + 2
    );
    args.push(Box::new(limit as i64));
    args.push(Box::new(offset as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        read_task,
    )?;
    let tasks = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((tasks, total as u32))
}

pub fn get_task(pool: &DbPool, task_id: &str) -> Result<Option<TaskRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks t WHERE t.task_id = ?1"),
        params![task_id],
        read_task,
    )
    .optional()
    .map_err(Into::into)
}

/// Field payload of a task create/update.
#[derive(Debug)]
pub struct TaskInput<'a> {
    pub task_type: &'a str,
    pub title: &'a str,
    pub description_md: &'a str,
    pub severity: &'a str,
    pub priority: &'a str,
    pub status: &'a str,
    pub assigned_to: &'a str,
    pub due_date: &'a str,
    pub links_json: &'a str,
    pub attachments_json: &'a str,
}

/// Creates a task; `task_no` is `COALESCE(MAX)+1` in the same transaction.
pub fn create_task(
    pool: &DbPool,
    input: &TaskInput<'_>,
    created_by: &str,
) -> Result<(String, u32)> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let task_no: i64 = tx.query_row(
        "SELECT COALESCE(MAX(task_no), 0) + 1 FROM tasks",
        [],
        |row| row.get(0),
    )?;
    let task_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO tasks (task_id, task_no, task_type, title, description_md, severity, \
            priority, status, assigned_to, due_date, links_json, attachments_json, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            task_id,
            task_no,
            input.task_type,
            input.title,
            input.description_md,
            input.severity,
            input.priority,
            input.status,
            input.assigned_to,
            input.due_date,
            input.links_json,
            input.attachments_json,
            created_by
        ],
    )?;
    tx.commit()?;
    Ok((task_id, task_no as u32))
}

/// Full-field update (the dispatcher validated permissions and enums).
pub fn update_task(pool: &DbPool, task_id: &str, input: &TaskInput<'_>) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE tasks SET task_type = ?1, title = ?2, description_md = ?3, severity = ?4, \
            priority = ?5, status = ?6, assigned_to = ?7, due_date = ?8, links_json = ?9, \
            attachments_json = ?10, updated_at = datetime('now') \
         WHERE task_id = ?11",
        params![
            input.task_type,
            input.title,
            input.description_md,
            input.severity,
            input.priority,
            input.status,
            input.assigned_to,
            input.due_date,
            input.links_json,
            input.attachments_json,
            task_id
        ],
    )?;
    Ok(n > 0)
}

/// Moves a task between board columns WITHOUT touching any other field, and
/// returns the new `updated_at`. The kanban card carries neither
/// `description_md` nor `attachments`, so a move routed through `update_task`
/// would write both back empty.
pub fn set_task_status(pool: &DbPool, task_id: &str, status: &str) -> Result<Option<String>> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE tasks SET status = ?1, updated_at = datetime('now') WHERE task_id = ?2",
        params![status, task_id],
    )?;
    if n == 0 {
        return Ok(None);
    }
    conn.query_row(
        "SELECT updated_at FROM tasks WHERE task_id = ?1",
        params![task_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Deletes a task with its comments.
pub fn delete_task(pool: &DbPool, task_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM task_comments WHERE task_id = ?1",
        params![task_id],
    )?;
    let n = tx.execute("DELETE FROM tasks WHERE task_id = ?1", params![task_id])?;
    tx.commit()?;
    Ok(n > 0)
}

fn read_comment(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskCommentRecord> {
    Ok(TaskCommentRecord {
        comment_id: row.get(0)?,
        task_id: row.get(1)?,
        author_user_id: row.get(2)?,
        body_md: row.get(3)?,
        created_at: row.get(4)?,
        edited_at: row.get(5)?,
    })
}

const COMMENT_COLS: &str = "comment_id, task_id, author_user_id, body_md, created_at, edited_at";

pub fn list_comments(pool: &DbPool, task_id: &str) -> Result<Vec<TaskCommentRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COMMENT_COLS} FROM task_comments WHERE task_id = ?1 ORDER BY created_at, comment_id"
    ))?;
    let rows = stmt.query_map(params![task_id], read_comment)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_comment(pool: &DbPool, comment_id: &str) -> Result<Option<TaskCommentRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1"),
        params![comment_id],
        read_comment,
    )
    .optional()
    .map_err(Into::into)
}

pub fn add_comment(
    pool: &DbPool,
    task_id: &str,
    author: &str,
    body_md: &str,
) -> Result<TaskCommentRecord> {
    let comment_id = uuid::Uuid::new_v4().to_string();
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO task_comments (comment_id, task_id, author_user_id, body_md) \
         VALUES (?1, ?2, ?3, ?4)",
        params![comment_id, task_id, author, body_md],
    )?;
    conn.query_row(
        &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1"),
        params![comment_id],
        read_comment,
    )
    .map_err(Into::into)
}

/// Edits a comment body; author-scoped by the caller. Stamps `edited_at`.
pub fn edit_comment(pool: &DbPool, comment_id: &str, author: &str, body_md: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE task_comments SET body_md = ?1, edited_at = datetime('now') \
         WHERE comment_id = ?2 AND author_user_id = ?3",
        params![body_md, comment_id, author],
    )?;
    Ok(n > 0)
}

pub fn delete_comment(pool: &DbPool, comment_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM task_comments WHERE comment_id = ?1",
        params![comment_id],
    )?;
    Ok(n > 0)
}
