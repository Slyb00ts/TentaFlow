// ===== File: project_studio/tasks.rs — tasks / defects with comments (F2) =====
//
// SQL layer for the task board: tasks and defects (severity-bearing) with a
// per-project sequential `task_no`, cross-object links (`links_json`),
// attachments and threaded comments. Authorization gates live in the
// dispatcher.

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::HashSet;

use super::models::{
    TaskCommentRecord, TaskEventRecord, TaskIndexSnapshot, TaskLinkRecord, TaskRecord,
    TaskStatusDurationRecord, TaskTypeRecord,
};
use crate::db::DbPool;
use tentaflow_protocol::project_studio::AttachmentWire;

pub const TASK_TYPES: &[&str] = &[
    "feature",
    "defect",
    "technical",
    "security",
    "subtask",
    "epic",
];
pub const TASK_STATUSES: &[&str] = &["todo", "in_progress", "review", "done"];
pub const TASK_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];
pub const TASK_SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tasks read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio tasks write: {e}")
}

fn admit_write(pool: &DbPool, task_id: Option<&str>) -> Result<()> {
    if let Some(project_id) = super::project_db::project_id_of(pool) {
        if let Some(task_id) = task_id {
            super::repository::task_write_admission(&project_id, task_id)
        } else {
            super::repository::project_write_admission(&project_id)
        }
    } else {
        // Unregistered pools are used only while validating an unpublished archive import.
        Ok(())
    }
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
        task_key: row.get(2)?,
        task_type: row.get(3)?,
        title: row.get(4)?,
        description_md: row.get(5)?,
        severity: row.get(6)?,
        priority: row.get(7)?,
        status: row.get(8)?,
        assigned_to: row.get(9)?,
        due_date: row.get(10)?,
        parent_task_id: row.get(11)?,
        links_json: row.get(12)?,
        attachments_json: row.get(13)?,
        comment_count: row.get::<_, i64>(14)? as u32,
        created_by: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
        archived_at: row.get(18)?,
        resolution: row.get(19)?,
        resolution_reason: row.get(20)?,
    })
}

const TASK_COLS: &str =
    "t.task_id, t.task_no, t.task_key, t.task_type, t.title, t.description_md, \
     t.severity, t.priority, t.status, t.assigned_to, t.due_date, t.parent_task_id, t.links_json, \
     t.attachments_json, \
     (SELECT COUNT(*) FROM task_comments c WHERE c.task_id = t.task_id), \
     t.created_by, t.created_at, t.updated_at, t.archived_at, t.resolution, t.resolution_reason";

pub fn task_index_snapshot(task: &TaskRecord) -> TaskIndexSnapshot {
    TaskIndexSnapshot {
        task_id: task.task_id.clone(),
        task_no: task.task_no,
        task_key: task.task_key.clone(),
        task_type: task.task_type.clone(),
        title: task.title.clone(),
        severity: task.severity.clone(),
        priority: task.priority.clone(),
        status: task.status.clone(),
        assigned_to: task.assigned_to.clone(),
        due_date: task.due_date.clone(),
        parent_task_id: task.parent_task_id.clone(),
        links_json: task.links_json.clone(),
        comment_count: task.comment_count,
        created_by: task.created_by.clone(),
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        archived_at: task.archived_at.clone(),
        resolution: task.resolution.clone(),
        resolution_reason: task.resolution_reason.clone(),
    }
}

#[derive(Debug, Default)]
pub struct TaskFilters<'a> {
    pub task_type: &'a str,
    pub status: &'a str,
    pub assigned_to: &'a str,
    pub search: &'a str,
    pub severity: &'a str,
    pub include_archived: bool,
}

pub fn list_tasks(
    pool: &DbPool,
    filters: &TaskFilters<'_>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<TaskRecord>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let mut clauses: Vec<String> = if filters.include_archived {
        vec!["1=1".to_string()]
    } else {
        vec!["t.archived_at IS NULL".to_string()]
    };
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
        clauses.push(format!(
            "(t.title LIKE ?{} ESCAPE '\\' OR t.task_key LIKE ?{} ESCAPE '\\')",
            args.len() + 1,
            args.len() + 1
        ));
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

/// A task somebody still has to do, as the handover screen lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTask {
    pub task_id: String,
    pub task_no: u32,
    pub title: String,
    pub status: String,
}

/// The unfinished tasks assigned to `user_id`, oldest number first.
pub fn open_tasks_of(pool: &DbPool, user_id: &str) -> Result<Vec<OpenTask>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT task_id, task_no, title, status FROM tasks \
         WHERE assigned_to = ?1 AND status <> 'done' AND archived_at IS NULL ORDER BY task_no",
    )?;
    let rows = stmt.query_map(params![user_id], |row| {
        Ok(OpenTask {
            task_id: row.get(0)?,
            task_no: row.get::<_, i64>(1)? as u32,
            title: row.get(2)?,
            status: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Keeps the handover note and assignee change in one transaction. The
/// current assignee check prevents a stale handover from taking a task back.
pub fn reassign_open(
    pool: &DbPool,
    task_id: &str,
    from: &str,
    to: &str,
    handover: &TaskHandoverInput<'_>,
) -> Result<Option<TaskMutation>> {
    if handover.note_md.trim().is_empty() {
        bail!("handover note is required");
    }
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    if old.status == "done" || old.archived_at.is_some() {
        return Ok(None);
    }
    let mut mutation = TaskMutation::from_task(&old);
    if from == to && handover.handover_id.is_some() {
        return Ok(None);
    }
    if old.assigned_to != from {
        if old.assigned_to == to && from != to {
            if let (Some(expected_id), Some(latest)) =
                (handover.handover_id, latest_handover(&tx, task_id)?)
            {
                if latest.handover_id.as_deref() == Some(expected_id)
                    && latest.direction == handover.direction
                    && latest.actor == handover.actor
                    && latest.from == from
                    && latest.to == to
                {
                    mutation.handover_comment_id = Some(latest.comment_id);
                    return Ok(Some(mutation));
                }
            }
        }
        return Ok(None);
    }
    if from != to {
        tx.execute(
            "UPDATE tasks SET assigned_to = ?1, updated_at = datetime('now') WHERE task_id = ?2",
            params![to, task_id],
        )?;
        let comment_id = uuid::Uuid::new_v4().to_string();
        let mentions = canonical_mentions(handover.mention_user_ids);
        tx.execute(
            "INSERT INTO task_comments(comment_id,task_id,author_user_id,body_md,mention_user_ids_json) \
             VALUES (?1,?2,?3,?4,?5)",
            params![comment_id, task_id, handover.actor, handover.note_md,
                serde_json::to_string(&mentions)?],
        )?;
        mutation.record(record_event(
            &tx,
            task_id,
            handover.actor,
            "comment_added",
            Value::Null,
            json!({"comment_id": comment_id, "body_md": handover.note_md,
                "mention_user_ids": mentions}),
        )?);
        mutation.record(record_event(
            &tx,
            task_id,
            handover.actor,
            match handover.direction {
                TaskHandoverDirection::Over => "handed_over",
                TaskHandoverDirection::Back => "handed_back",
            },
            json!({"assigned_to": from}),
            json!({"assigned_to": to, "comment_id": comment_id,
                "handover_id": handover.handover_id}),
        )?);
        mutation.previous_assignee = Some(from.to_string());
        mutation.current_assignee = to.to_string();
        mutation.handover_comment_id = Some(comment_id);
    }
    tx.commit()?;
    Ok(Some(mutation))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskHandoverDirection {
    Over,
    Back,
}

pub struct TaskHandoverInput<'a> {
    pub actor: &'a str,
    pub note_md: &'a str,
    pub mention_user_ids: &'a [String],
    pub direction: TaskHandoverDirection,
    pub handover_id: Option<&'a str>,
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
    pub parent_task_id: Option<&'a str>,
    pub links_json: &'a str,
    pub attachments_json: &'a str,
}

#[derive(Debug, Clone)]
pub struct TaskMutation {
    pub task_id: String,
    pub task_no: u32,
    pub task_key: String,
    pub changed: bool,
    pub event_ids: Vec<i64>,
    pub status_event_id: Option<i64>,
    pub previous_status: Option<String>,
    pub current_status: String,
    pub previous_assignee: Option<String>,
    pub current_assignee: String,
    pub archived: bool,
    pub handover_comment_id: Option<String>,
}

impl TaskMutation {
    fn from_task(task: &TaskRecord) -> Self {
        Self {
            task_id: task.task_id.clone(),
            task_no: task.task_no,
            task_key: task.task_key.clone(),
            changed: false,
            event_ids: Vec::new(),
            status_event_id: None,
            previous_status: None,
            current_status: task.status.clone(),
            previous_assignee: None,
            current_assignee: task.assigned_to.clone(),
            archived: task.archived_at.is_some(),
            handover_comment_id: None,
        }
    }

    fn record(&mut self, event_id: i64) {
        self.changed = true;
        self.event_ids.push(event_id);
    }
}

fn get_task_tx(tx: &Transaction<'_>, task_id: &str) -> Result<Option<TaskRecord>> {
    tx.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks t WHERE t.task_id = ?1"),
        params![task_id],
        read_task,
    )
    .optional()
    .map_err(Into::into)
}

fn record_event(
    tx: &Transaction<'_>,
    task_id: &str,
    actor: &str,
    kind: &str,
    before: Value,
    after: Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO task_events(task_id,actor_kind,actor_id,kind,before_json,after_json) \
         VALUES (?1,'user',?2,?3,?4,?5)",
        params![task_id, actor, kind, before.to_string(), after.to_string()],
    )?;
    Ok(tx.last_insert_rowid())
}

fn validate_task_type(conn: &rusqlite::Connection, task_type: &str, unchanged: bool) -> Result<()> {
    let active: Option<bool> = conn
        .query_row(
            "SELECT active FROM task_types WHERE type_id = ?1",
            params![task_type],
            |row| row.get(0),
        )
        .optional()?;
    match active {
        Some(true) => Ok(()),
        Some(false) if unchanged => Ok(()),
        _ => bail!("task type is unavailable"),
    }
}

fn type_source_before_write(pool: &DbPool) -> Result<Option<(String, String, Option<DbPool>)>> {
    let Some(project_id) = super::project_db::project_id_of(pool) else {
        return Ok(None);
    };
    let (_, source_id) = super::repository::effective_project_settings(&project_id)?;
    let source_pool = if source_id == project_id {
        None
    } else {
        Some(super::project_db::open(&source_id)?)
    };
    Ok(Some((project_id, source_id, source_pool)))
}

fn validate_type_source_after_lock(
    source: &Option<(String, String, Option<DbPool>)>,
) -> Result<()> {
    if let Some((project_id, source_id, _)) = source {
        let (_, current) = super::repository::effective_project_settings(project_id)?;
        if current != *source_id {
            bail!("task type source changed; retry task mutation");
        }
    }
    Ok(())
}

fn validate_parent(
    tx: &Transaction<'_>,
    task_id: Option<&str>,
    task_type: &str,
    parent_id: Option<&str>,
) -> Result<()> {
    if task_type == "epic" && parent_id.is_some() {
        bail!("an epic cannot have a parent task");
    }
    if task_type == "subtask" && parent_id.is_none() {
        bail!("a subtask requires a parent task");
    }
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if task_id == Some(parent_id) {
        bail!("task cannot be its own parent");
    }
    let parent_type: Option<String> = tx
        .query_row(
            "SELECT task_type FROM tasks WHERE task_id = ?1 AND archived_at IS NULL",
            params![parent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(parent_type) = parent_type else {
        bail!("parent task not found")
    };
    if task_type != "subtask" && parent_type != "epic" {
        bail!("only an epic can parent this task type");
    }
    if let Some(task_id) = task_id {
        let cycle: i64 = tx.query_row(
            "WITH RECURSIVE ancestors(task_id,parent_task_id) AS (
                SELECT task_id,parent_task_id FROM tasks WHERE task_id = ?1
                UNION ALL
                SELECT t.task_id,t.parent_task_id FROM tasks t JOIN ancestors a ON t.task_id = a.parent_task_id
             ) SELECT COUNT(*) FROM ancestors WHERE task_id = ?2",
            params![parent_id, task_id],
            |row| row.get(0),
        )?;
        if cycle != 0 {
            bail!("parent relationship would create a cycle");
        }
    }
    Ok(())
}

/// Creates a task and its initial event in one transaction.
pub fn create_task(pool: &DbPool, input: &TaskInput<'_>, created_by: &str) -> Result<TaskMutation> {
    let type_source = type_source_before_write(pool)?;
    let mut source_guard = None;
    let mut conn = if let Some((project_id, source_id, Some(source_pool))) = &type_source {
        if source_id < project_id {
            source_guard = Some(source_pool.write().map_err(write_err)?);
            pool.write().map_err(write_err)?
        } else {
            let child = pool.write().map_err(write_err)?;
            source_guard = Some(source_pool.write().map_err(write_err)?);
            child
        }
    } else {
        pool.write().map_err(write_err)?
    };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    admit_write(pool, None)?;
    validate_type_source_after_lock(&type_source)?;
    let catalogue: &rusqlite::Connection = match source_guard.as_ref() {
        Some(guard) => &**guard,
        None => &tx,
    };
    validate_task_type(catalogue, input.task_type, false)?;
    validate_parent(&tx, None, input.task_type, input.parent_task_id)?;
    if let Some(parent_id) = input.parent_task_id {
        admit_write(pool, Some(parent_id))?;
    }
    let task_no: i64 = tx.query_row(
        "SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'task_next_no'",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "UPDATE settings SET value = CAST(?1 AS TEXT) WHERE key = 'task_next_no'",
        params![task_no + 1],
    )?;
    tx.execute(
        "UPDATE settings SET value = '1' WHERE key = 'project_key_prefix_locked'",
        [],
    )?;
    let prefix: String = tx.query_row(
        "SELECT value FROM settings WHERE key = 'project_key_prefix'",
        [],
        |row| row.get(0),
    )?;
    let task_key = format!("{prefix}-{task_no}");
    let task_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO tasks (task_id, task_no, task_key, task_type, title, description_md, severity, \
            priority, status, assigned_to, due_date, parent_task_id, links_json, attachments_json, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            task_id,
            task_no,
            task_key,
            input.task_type,
            input.title,
            input.description_md,
            input.severity,
            input.priority,
            input.status,
            input.assigned_to,
            input.due_date,
            input.parent_task_id,
            input.links_json,
            input.attachments_json,
            created_by
        ],
    )?;
    let event_id = record_event(
        &tx,
        &task_id,
        created_by,
        "created",
        Value::Null,
        json!({"task_key": task_key, "task_type": input.task_type, "title": input.title,
            "description_md": input.description_md, "severity": input.severity,
            "priority": input.priority, "status": input.status, "assigned_to": input.assigned_to,
            "due_date": input.due_date, "parent_task_id": input.parent_task_id,
            "links_json": input.links_json, "attachments_json": input.attachments_json}),
    )?;
    tx.commit()?;
    Ok(TaskMutation {
        task_id,
        task_no: task_no as u32,
        task_key,
        changed: true,
        event_ids: vec![event_id],
        status_event_id: None,
        previous_status: None,
        current_status: input.status.to_string(),
        previous_assignee: None,
        current_assignee: input.assigned_to.to_string(),
        archived: false,
        handover_comment_id: None,
    })
}

/// Full-field update with one event per actual field change.
pub fn update_task(
    pool: &DbPool,
    task_id: &str,
    input: &TaskInput<'_>,
    actor: &str,
) -> Result<Option<TaskMutation>> {
    let type_source = type_source_before_write(pool)?;
    let mut source_guard = None;
    let conn = if let Some((project_id, source_id, Some(source_pool))) = &type_source {
        if source_id < project_id {
            source_guard = Some(source_pool.write().map_err(write_err)?);
            pool.write().map_err(write_err)?
        } else {
            let child = pool.write().map_err(write_err)?;
            source_guard = Some(source_pool.write().map_err(write_err)?);
            child
        }
    } else {
        pool.write().map_err(write_err)?
    };
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    validate_type_source_after_lock(&type_source)?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    if old.archived_at.is_some() {
        bail!("archived task cannot be edited");
    }
    let catalogue: &rusqlite::Connection = match source_guard.as_ref() {
        Some(guard) => &**guard,
        None => &tx,
    };
    validate_task_type(catalogue, input.task_type, input.task_type == old.task_type)?;
    validate_parent(&tx, Some(task_id), input.task_type, input.parent_task_id)?;
    if let Some(parent_id) = input.parent_task_id {
        admit_write(pool, Some(parent_id))?;
    }
    let mut mutation = TaskMutation::from_task(&old);
    for (kind, before, after) in [
        ("task_type", json!(old.task_type), json!(input.task_type)),
        ("title", json!(old.title), json!(input.title)),
        (
            "description_md",
            json!(old.description_md),
            json!(input.description_md),
        ),
        ("severity", json!(old.severity), json!(input.severity)),
        ("priority", json!(old.priority), json!(input.priority)),
        ("status_changed", json!(old.status), json!(input.status)),
        (
            "assigned_to",
            json!(old.assigned_to),
            json!(input.assigned_to),
        ),
        ("due_date", json!(old.due_date), json!(input.due_date)),
        (
            "parent_task_id",
            json!(old.parent_task_id),
            json!(input.parent_task_id),
        ),
        ("links_json", json!(old.links_json), json!(input.links_json)),
        (
            "attachments_json",
            json!(old.attachments_json),
            json!(input.attachments_json),
        ),
    ] {
        if before != after {
            let event_kind = if kind == "assigned_to" {
                if input.assigned_to.is_empty() {
                    "unassigned"
                } else if old.assigned_to.is_empty() {
                    "assigned"
                } else {
                    "reassigned"
                }
            } else {
                kind
            };
            let event_id = record_event(&tx, task_id, actor, event_kind, before, after)?;
            mutation.record(event_id);
            if kind == "status_changed" {
                mutation.status_event_id = Some(event_id);
            }
        }
    }
    if old.resolution.is_some() && input.status != "done" {
        let event_id = record_event(
            &tx,
            task_id,
            actor,
            "resolution_changed",
            json!({"status":old.status,"resolution":old.resolution,
                   "resolution_reason":old.resolution_reason}),
            json!({"status":input.status,"resolution":null,"resolution_reason":null}),
        )?;
        mutation.record(event_id);
    }
    if !mutation.changed {
        return Ok(Some(mutation));
    }
    if old.status != input.status {
        mutation.previous_status = Some(old.status.clone());
        mutation.current_status = input.status.to_string();
    }
    if old.assigned_to != input.assigned_to {
        mutation.previous_assignee = Some(old.assigned_to.clone());
        mutation.current_assignee = input.assigned_to.to_string();
    }
    tx.execute(
        "UPDATE tasks SET task_type = ?1, title = ?2, description_md = ?3, severity = ?4, \
            priority = ?5, status = ?6, assigned_to = ?7, due_date = ?8, parent_task_id = ?9, \
            links_json = ?10, attachments_json = ?11, \
            resolution = CASE WHEN ?6='done' THEN resolution ELSE NULL END, \
            resolution_reason = CASE WHEN ?6='done' THEN resolution_reason ELSE NULL END, \
            updated_at = datetime('now') \
         WHERE task_id = ?12",
        params![
            input.task_type,
            input.title,
            input.description_md,
            input.severity,
            input.priority,
            input.status,
            input.assigned_to,
            input.due_date,
            input.parent_task_id,
            input.links_json,
            input.attachments_json,
            task_id
        ],
    )?;
    tx.commit()?;
    Ok(Some(mutation))
}

/// Moves a task between board columns WITHOUT touching any other field, and
/// returns the new `updated_at`. The kanban card carries neither
/// `description_md` nor `attachments`, so a move routed through `update_task`
/// would write both back empty.
pub fn set_task_status(
    pool: &DbPool,
    task_id: &str,
    status: &str,
    actor: &str,
) -> Result<Option<TaskMutation>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    if old.archived_at.is_some() {
        bail!("archived task cannot change status");
    }
    let mut mutation = TaskMutation::from_task(&old);
    if status != old.status {
        tx.execute(
            "UPDATE tasks SET status = ?1, \
             resolution=CASE WHEN ?1='done' THEN resolution ELSE NULL END, \
             resolution_reason=CASE WHEN ?1='done' THEN resolution_reason ELSE NULL END, \
             updated_at = datetime('now') WHERE task_id = ?2",
            params![status, task_id],
        )?;
        let event_id = record_event(
            &tx,
            task_id,
            actor,
            "status_changed",
            json!(old.status),
            json!(status),
        )?;
        mutation.record(event_id);
        mutation.status_event_id = Some(event_id);
        mutation.previous_status = Some(old.status.clone());
        mutation.current_status = status.to_string();
        if old.resolution.is_some() && status != "done" {
            let event_id = record_event(
                &tx,
                task_id,
                actor,
                "resolution_changed",
                json!({"status":old.status,"resolution":old.resolution,
                       "resolution_reason":old.resolution_reason}),
                json!({"status":status,"resolution":null,"resolution_reason":null}),
            )?;
            mutation.record(event_id);
        }
    }
    tx.commit()?;
    Ok(Some(mutation))
}

pub fn mark_task_not_pursued(
    pool: &DbPool,
    task_id: &str,
    reason: &str,
    actor: &str,
) -> Result<Option<(TaskMutation, i64)>> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 2000 {
        bail!("not-pursued reason is required and cannot exceed 2000 characters");
    }
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    if old.archived_at.is_some() {
        bail!("archived task cannot be resolved");
    }
    let mut mutation = TaskMutation::from_task(&old);
    if old.status == "done"
        && old.resolution.as_deref() == Some("not_pursued")
        && old.resolution_reason.as_deref() == Some(reason)
    {
        let event_id: i64 = tx.query_row(
            "SELECT event_id FROM task_events WHERE task_id=?1 AND kind='resolution_changed' \
             ORDER BY event_id DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )?;
        return Ok(Some((mutation, event_id)));
    }
    tx.execute(
        "UPDATE tasks SET status='done',resolution='not_pursued',resolution_reason=?1, \
         updated_at=datetime('now') WHERE task_id=?2",
        params![reason, task_id],
    )?;
    if old.status != "done" {
        let event_id = record_event(
            &tx,
            task_id,
            actor,
            "status_changed",
            json!(old.status),
            json!("done"),
        )?;
        mutation.record(event_id);
        mutation.status_event_id = Some(event_id);
        mutation.previous_status = Some(old.status.clone());
        mutation.current_status = "done".into();
    }
    let event_id = record_event(
        &tx,
        task_id,
        actor,
        "resolution_changed",
        json!({"status":old.status,"resolution":old.resolution,
               "resolution_reason":old.resolution_reason}),
        json!({"status":"done","resolution":"not_pursued","resolution_reason":reason}),
    )?;
    mutation.record(event_id);
    tx.commit()?;
    Ok(Some((mutation, event_id)))
}

/// Removes an untouched mistake; archives any task that has acquired work.
pub fn delete_task(pool: &DbPool, task_id: &str, actor: &str) -> Result<Option<TaskMutation>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    let mut mutation = TaskMutation::from_task(&old);
    if old.archived_at.is_some() {
        return Ok(Some(mutation));
    }
    let event_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_events WHERE task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    let link_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_links WHERE source_task_id = ?1 OR target_task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    let child_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tasks WHERE parent_task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    let untouched = event_count == 1
        && old.comment_count == 0
        && link_count == 0
        && child_count == 0
        && old.description_md.is_empty()
        && old.links_json == "[]"
        && old.attachments_json == "[]"
        && old.assigned_to.is_empty()
        && old.parent_task_id.is_none()
        && old.status == "todo";
    if untouched {
        tx.execute(
            "DELETE FROM task_events WHERE task_id = ?1",
            params![task_id],
        )?;
        tx.execute("DELETE FROM tasks WHERE task_id = ?1", params![task_id])?;
        mutation.changed = true;
        mutation.archived = false;
    } else {
        tx.execute(
            "UPDATE tasks SET archived_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), \
             updated_at = datetime('now') WHERE task_id = ?1",
            params![task_id],
        )?;
        mutation.record(record_event(
            &tx,
            task_id,
            actor,
            "archived",
            Value::Null,
            json!({"archived": true}),
        )?);
        mutation.archived = true;
    }
    tx.commit()?;
    Ok(Some(mutation))
}

fn read_comment(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskCommentRecord> {
    Ok(TaskCommentRecord {
        comment_id: row.get(0)?,
        task_id: row.get(1)?,
        author_user_id: row.get(2)?,
        body_md: row.get(3)?,
        created_at: row.get(4)?,
        edited_at: row.get(5)?,
        mention_user_ids_json: row.get(6)?,
    })
}

const COMMENT_COLS: &str =
    "comment_id, task_id, author_user_id, body_md, created_at, edited_at, mention_user_ids_json";

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

#[derive(Debug, Clone)]
pub struct CommentMutation {
    pub comment: Option<TaskCommentRecord>,
    pub changed: bool,
    pub event_id: Option<i64>,
    pub added_mention_user_ids: Vec<String>,
}

fn canonical_mentions(mention_user_ids: &[String]) -> Vec<String> {
    let mut ids = mention_user_ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

pub fn add_comment(
    pool: &DbPool,
    task_id: &str,
    author: &str,
    body_md: &str,
    mention_user_ids: &[String],
) -> Result<CommentMutation> {
    let comment_id = uuid::Uuid::new_v4().to_string();
    let mentions = canonical_mentions(mention_user_ids);
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(task) = get_task_tx(&tx, task_id)? else {
        bail!("task not found")
    };
    if task.archived_at.is_some() {
        bail!("archived task cannot receive comments")
    }
    tx.execute(
        "INSERT INTO task_comments (comment_id, task_id, author_user_id, body_md, mention_user_ids_json) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![comment_id, task_id, author, body_md, serde_json::to_string(&mentions)?],
    )?;
    let event_id = record_event(
        &tx,
        task_id,
        author,
        "comment_added",
        Value::Null,
        json!({"comment_id": comment_id, "body_md": body_md, "mention_user_ids": mentions}),
    )?;
    let comment = tx.query_row(
        &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1"),
        params![comment_id],
        read_comment,
    )?;
    tx.commit()?;
    Ok(CommentMutation {
        comment: Some(comment),
        changed: true,
        event_id: Some(event_id),
        added_mention_user_ids: mentions,
    })
}

/// Edits a comment body; author-scoped by the caller. Stamps `edited_at`.
pub fn edit_comment(
    pool: &DbPool,
    comment_id: &str,
    author: &str,
    body_md: &str,
    mention_user_ids: &[String],
) -> Result<Option<CommentMutation>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, None)?;
    let old = tx.query_row(
        &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1 AND author_user_id = ?2"),
        params![comment_id, author], read_comment,
    ).optional()?;
    let Some(old) = old else { return Ok(None) };
    admit_write(pool, Some(&old.task_id))?;
    if get_task_tx(&tx, &old.task_id)?
        .ok_or_else(|| anyhow!("task not found"))?
        .archived_at
        .is_some()
    {
        bail!("archived task comments cannot be edited");
    }
    let old_mentions: Vec<String> = serde_json::from_str(&old.mention_user_ids_json)?;
    let mentions = canonical_mentions(mention_user_ids);
    if body_md == old.body_md && mentions == old_mentions {
        return Ok(Some(CommentMutation {
            comment: Some(old),
            changed: false,
            event_id: None,
            added_mention_user_ids: vec![],
        }));
    }
    tx.execute(
        "UPDATE task_comments SET body_md = ?1, mention_user_ids_json = ?2, edited_at = datetime('now') \
         WHERE comment_id = ?3",
        params![body_md, serde_json::to_string(&mentions)?, comment_id],
    )?;
    let event_id = record_event(
        &tx,
        &old.task_id,
        author,
        "comment_edited",
        json!({"comment_id": comment_id, "body_md": old.body_md, "mention_user_ids": old_mentions}),
        json!({"comment_id": comment_id, "body_md": body_md, "mention_user_ids": mentions}),
    )?;
    let comment = tx.query_row(
        &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1"),
        params![comment_id],
        read_comment,
    )?;
    tx.commit()?;
    let added = mentions
        .into_iter()
        .filter(|id| !old_mentions.contains(id))
        .collect();
    Ok(Some(CommentMutation {
        comment: Some(comment),
        changed: true,
        event_id: Some(event_id),
        added_mention_user_ids: added,
    }))
}

pub fn delete_comment(
    pool: &DbPool,
    comment_id: &str,
    actor: &str,
) -> Result<Option<CommentMutation>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, None)?;
    let old = tx
        .query_row(
            &format!("SELECT {COMMENT_COLS} FROM task_comments WHERE comment_id = ?1"),
            params![comment_id],
            read_comment,
        )
        .optional()?;
    let Some(old) = old else { return Ok(None) };
    admit_write(pool, Some(&old.task_id))?;
    if get_task_tx(&tx, &old.task_id)?
        .ok_or_else(|| anyhow!("task not found"))?
        .archived_at
        .is_some()
    {
        bail!("archived task comments cannot be deleted");
    }
    tx.execute(
        "DELETE FROM task_comments WHERE comment_id = ?1",
        params![comment_id],
    )?;
    let event_id = record_event(
        &tx,
        &old.task_id,
        actor,
        "comment_deleted",
        json!({"comment_id": comment_id, "body_md": old.body_md, "mention_user_ids": serde_json::from_str::<Vec<String>>(&old.mention_user_ids_json)?}),
        Value::Null,
    )?;
    tx.commit()?;
    Ok(Some(CommentMutation {
        comment: None,
        changed: true,
        event_id: Some(event_id),
        added_mention_user_ids: vec![],
    }))
}

pub fn list_task_types(pool: &DbPool) -> Result<Vec<TaskTypeRecord>> {
    let conn = pool.read().map_err(read_err)?;
    list_task_types_on(&conn)
}

pub(crate) fn list_task_types_on(conn: &rusqlite::Connection) -> Result<Vec<TaskTypeRecord>> {
    let mut stmt = conn.prepare(
        "SELECT type_id,name,description,sort_order,built_in,active FROM task_types ORDER BY sort_order,type_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TaskTypeRecord {
            type_id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            sort_order: row.get(3)?,
            built_in: row.get(4)?,
            active: row.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(crate) fn materialize_task_type_catalogue(
    source: &rusqlite::Connection,
    target: &rusqlite::Connection,
) -> Result<()> {
    let mut stmt = source.prepare(
        "SELECT type_id,name,description,sort_order,built_in,active FROM task_types ORDER BY sort_order,type_id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(TaskTypeRecord {
                type_id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                sort_order: row.get(3)?,
                built_in: row.get(4)?,
                active: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tx = target.unchecked_transaction()?;
    tx.execute("UPDATE task_types SET active=0 WHERE built_in=0", [])?;
    for row in rows {
        tx.execute(
            "INSERT INTO task_types(type_id,name,description,sort_order,built_in,active) \
             VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(type_id) DO UPDATE SET \
             name=excluded.name,description=excluded.description,sort_order=excluded.sort_order,\
             built_in=excluded.built_in,active=excluded.active",
            params![
                row.type_id,
                row.name,
                row.description,
                row.sort_order,
                row.built_in,
                row.active
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn validate_reinherited_task_types(
    source: &rusqlite::Connection,
    target: &rusqlite::Connection,
) -> Result<()> {
    let mut stmt = target.prepare("SELECT DISTINCT task_type FROM tasks ORDER BY task_type")?;
    let types = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for type_id in types {
        let exists: Option<i64> = source
            .query_row(
                "SELECT 1 FROM task_types WHERE type_id=?1",
                [&type_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            bail!("task type used by project is absent from inherited catalogue");
        }
    }
    Ok(())
}

pub fn save_task_type(
    pool: &DbPool,
    type_id: &str,
    name: &str,
    description: &str,
    sort_order: i32,
    active: bool,
) -> Result<TaskTypeRecord> {
    if type_id.len() < 2
        || type_id.len() > 32
        || !type_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || !type_id.as_bytes()[0].is_ascii_lowercase()
    {
        bail!("invalid task type id");
    }
    if name.trim().is_empty() || name.chars().count() > 80 || description.chars().count() > 500 {
        bail!("invalid task type name or description");
    }
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, None)?;
    if let Some(project_id) = super::project_db::project_id_of(pool) {
        let (_, source_id) = super::repository::effective_project_settings(&project_id)?;
        if source_id != project_id {
            bail!("inherited task types must be edited at their source");
        }
    }
    let built_in: Option<bool> = tx
        .query_row(
            "SELECT built_in FROM task_types WHERE type_id = ?1",
            params![type_id],
            |row| row.get(0),
        )
        .optional()?;
    if built_in == Some(true) {
        bail!("built-in task types cannot be edited");
    }
    tx.execute(
        "INSERT INTO task_types(type_id,name,description,sort_order,built_in,active) \
         VALUES (?1,?2,?3,?4,0,?5) ON CONFLICT(type_id) DO UPDATE SET \
         name=excluded.name,description=excluded.description,sort_order=excluded.sort_order,active=excluded.active",
        params![type_id, name.trim(), description.trim(), sort_order, active],
    )?;
    tx.commit()?;
    drop(conn);
    if let Some(project_id) = super::project_db::project_id_of(pool) {
        let project = super::repository::project_record(&project_id)?
            .ok_or_else(|| anyhow!("task catalogue project missing"))?;
        super::task_index::sync_catalogue(&project, pool)?;
    }
    Ok(TaskTypeRecord {
        type_id: type_id.to_string(),
        name: name.trim().to_string(),
        description: description.trim().to_string(),
        sort_order,
        built_in: false,
        active,
    })
}

fn read_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEventRecord> {
    Ok(TaskEventRecord {
        event_id: row.get(0)?,
        task_id: row.get(1)?,
        at: row.get(2)?,
        actor_kind: row.get(3)?,
        actor_id: row.get(4)?,
        kind: row.get(5)?,
        before_json: row.get(6)?,
        after_json: row.get(7)?,
    })
}

pub fn list_task_events(
    pool: &DbPool,
    task_id: &str,
    before_id: Option<i64>,
    limit: u32,
) -> Result<(Vec<TaskEventRecord>, bool)> {
    let conn = pool.read().map_err(read_err)?;
    let limit = limit.clamp(1, 100);
    let mut stmt = conn.prepare(
        "SELECT event_id,task_id,at,actor_kind,actor_id,kind,before_json,after_json \
         FROM task_events WHERE task_id = ?1 AND (?2 IS NULL OR event_id < ?2) \
         ORDER BY event_id DESC LIMIT ?3",
    )?;
    let mut events = stmt
        .query_map(params![task_id, before_id, limit + 1], read_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = events.len() > limit as usize;
    events.truncate(limit as usize);
    Ok((events, has_more))
}

pub fn task_status_durations(
    pool: &DbPool,
    task_id: &str,
) -> Result<Vec<TaskStatusDurationRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT at,after_json FROM task_events WHERE task_id = ?1 \
         AND kind IN ('created','imported','status_changed') ORDER BY event_id",
    )?;
    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut durations = Vec::new();
    let mut current: Option<(String, String)> = None;
    for (at, after_json) in rows {
        let value: Value = serde_json::from_str(&after_json)?;
        let status = value
            .as_str()
            .or_else(|| value.get("status").and_then(Value::as_str));
        let Some(status) = status else { continue };
        if let Some((previous, entered_at)) = current.replace((status.to_string(), at.clone())) {
            durations.push(TaskStatusDurationRecord {
                status: previous,
                seconds: elapsed_seconds(&entered_at, &at),
                entered_at,
                left_at: Some(at),
            });
        }
    }
    if let Some((status, entered_at)) = current {
        let now = chrono::Utc::now().to_rfc3339();
        durations.push(TaskStatusDurationRecord {
            status,
            seconds: elapsed_seconds(&entered_at, &now),
            entered_at,
            left_at: None,
        });
    }
    Ok(durations)
}

fn elapsed_seconds(start: &str, end: &str) -> u64 {
    fn parse(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| time.with_timezone(&chrono::Utc))
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                    .ok()
                    .map(|time| time.and_utc())
            })
    }
    match (parse(start), parse(end)) {
        (Some(start), Some(end)) => end.signed_duration_since(start).num_seconds().max(0) as u64,
        _ => 0,
    }
}

pub fn set_task_archived(
    pool: &DbPool,
    task_id: &str,
    archived: bool,
    actor: &str,
) -> Result<Option<TaskMutation>> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    admit_write(pool, Some(task_id))?;
    let Some(old) = get_task_tx(&tx, task_id)? else {
        return Ok(None);
    };
    let mut mutation = TaskMutation::from_task(&old);
    if old.archived_at.is_some() == archived {
        return Ok(Some(mutation));
    }
    tx.execute(
        "UPDATE tasks SET archived_at = CASE WHEN ?1 THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE NULL END, \
         updated_at = datetime('now') WHERE task_id = ?2",
        params![archived, task_id],
    )?;
    mutation.record(record_event(
        &tx,
        task_id,
        actor,
        if archived { "archived" } else { "restored" },
        json!({"archived": !archived}),
        json!({"archived": archived}),
    )?);
    mutation.archived = archived;
    tx.commit()?;
    Ok(Some(mutation))
}

pub fn project_key_prefix_locked(pool: &DbPool) -> Result<bool> {
    let conn = pool.read().map_err(read_err)?;
    let value: String = conn.query_row(
        "SELECT value FROM settings WHERE key = 'project_key_prefix_locked'",
        [],
        |row| row.get(0),
    )?;
    Ok(value == "1")
}

pub fn latest_handover_comment_id(pool: &DbPool, task_id: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    let assignee: Option<String> = conn
        .query_row(
            "SELECT assigned_to FROM tasks WHERE task_id = ?1 AND archived_at IS NULL",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(assignee) = assignee else {
        return Ok(None);
    };
    Ok(latest_handover(&conn, task_id)?
        .filter(|latest| latest.to == assignee)
        .map(|latest| latest.comment_id))
}

struct LatestHandover {
    direction: TaskHandoverDirection,
    actor: String,
    from: String,
    to: String,
    comment_id: String,
    handover_id: Option<String>,
}

fn latest_handover(conn: &rusqlite::Connection, task_id: &str) -> Result<Option<LatestHandover>> {
    let latest: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT kind,actor_id,before_json,after_json FROM task_events WHERE task_id = ?1 \
             AND kind IN ('assigned','reassigned','unassigned','handed_over','handed_back','assigned_to') \
             ORDER BY event_id DESC LIMIT 1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((kind, actor, before_json, after_json)) = latest else {
        return Ok(None);
    };
    let direction = match kind.as_str() {
        "handed_over" => TaskHandoverDirection::Over,
        "handed_back" => TaskHandoverDirection::Back,
        _ => return Ok(None),
    };
    let before: Value = serde_json::from_str(&before_json)?;
    let after: Value = serde_json::from_str(&after_json)?;
    let Some(from) = before.get("assigned_to").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(to) = after.get("assigned_to").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(comment_id) = after.get("comment_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM task_comments WHERE task_id = ?1 AND comment_id = ?2",
            params![task_id, comment_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.map(|_| LatestHandover {
        direction,
        actor,
        from: from.to_string(),
        to: to.to_string(),
        comment_id: comment_id.to_string(),
        handover_id: after
            .get("handover_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

pub fn task_attachment_metadata(
    pool: &DbPool,
    task_id: &str,
    sha256: &str,
) -> Result<Option<AttachmentWire>> {
    Ok(task_attachment_inventory(pool, task_id)?
        .into_iter()
        .find(|item| item.sha256 == sha256))
}

pub fn task_attachment_inventory(pool: &DbPool, task_id: &str) -> Result<Vec<AttachmentWire>> {
    let conn = pool.read().map_err(read_err)?;
    let current: Option<String> = conn
        .query_row(
            "SELECT attachments_json FROM tasks WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Ok(Vec::new());
    };
    let mut attachments = Vec::new();
    let mut seen = HashSet::new();
    collect_attachments(&current, &mut attachments, &mut seen)?;
    let mut stmt = conn.prepare(
        "SELECT kind,before_json,after_json FROM task_events WHERE task_id = ?1 \
         AND kind IN ('created','attachments_json') ORDER BY event_id DESC",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (kind, before, after) = row?;
        let after: Value = serde_json::from_str(&after)?;
        if kind == "created" {
            if let Some(raw) = after.get("attachments_json").and_then(Value::as_str) {
                collect_attachments(raw, &mut attachments, &mut seen)?;
            }
        } else {
            let before: Value = serde_json::from_str(&before)?;
            for value in [&before, &after] {
                if let Some(raw) = value.as_str() {
                    collect_attachments(raw, &mut attachments, &mut seen)?;
                }
            }
        }
    }
    Ok(attachments)
}

fn collect_attachments(
    raw: &str,
    attachments: &mut Vec<AttachmentWire>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    for item in serde_json::from_str::<Vec<AttachmentWire>>(raw)? {
        if seen.insert(item.sha256.clone()) {
            attachments.push(item);
        }
    }
    Ok(())
}

pub fn attachment_referenced(pool: &DbPool, task_id: &str, sha256: &str) -> Result<bool> {
    Ok(task_attachment_metadata(pool, task_id, sha256)?.is_some())
}

#[derive(Debug, Clone)]
pub struct TaskLinkMutation {
    pub link: TaskLinkRecord,
    pub source_event_id: i64,
    pub target_event_id: i64,
}

pub fn list_task_links(pool: &DbPool, task_id: &str) -> Result<Vec<TaskLinkRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT l.link_id,l.relation_id,l.source_task_id,l.target_task_id,\
         l.source_project_id,l.target_project_id,l.kind,l.lag_days, \
         COALESCE(other.task_key,''),COALESCE(other.title,'') \
         FROM task_links l LEFT JOIN tasks other ON other.task_id = \
         CASE WHEN l.source_task_id = ?1 THEN l.target_task_id ELSE l.source_task_id END \
         WHERE l.source_task_id = ?1 OR l.target_task_id = ?1 ORDER BY l.link_id",
    )?;
    let rows = stmt.query_map(params![task_id], read_task_link)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn read_task_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskLinkRecord> {
    Ok(TaskLinkRecord {
        link_id: row.get(0)?,
        relation_id: row.get(1)?,
        source_task_id: row.get(2)?,
        target_task_id: row.get(3)?,
        source_project_id: row.get(4)?,
        target_project_id: row.get(5)?,
        kind: row.get(6)?,
        lag_days: row.get(7)?,
        other_task_key: row.get(8)?,
        other_task_title: row.get(9)?,
    })
}

pub fn get_task_link(pool: &DbPool, link_id: i64) -> Result<Option<TaskLinkRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT l.link_id,l.relation_id,l.source_task_id,l.target_task_id,\
         l.source_project_id,l.target_project_id,l.kind,l.lag_days,\
         COALESCE(other.task_key,''),COALESCE(other.title,'') \
         FROM task_links l LEFT JOIN tasks other ON other.task_id=l.target_task_id \
         WHERE l.link_id=?1",
        [link_id],
        read_task_link,
    )
    .optional()
    .map_err(Into::into)
}

pub fn add_task_link(
    source_pool: &DbPool,
    target_pool: &DbPool,
    source_project_id: &str,
    target_project_id: &str,
    source_task_id: &str,
    target_task_id: &str,
    kind: &str,
    lag_days: i32,
    actor: &str,
) -> Result<TaskLinkMutation> {
    if source_task_id == target_task_id {
        bail!("task cannot link to itself");
    }
    if !["related", "duplicate", "fs", "ss", "ff", "sf"].contains(&kind) {
        bail!("invalid task link kind");
    }
    if !(-3650..=3650).contains(&lag_days) {
        bail!("task link lag is out of range");
    }
    if ["related", "duplicate"].contains(&kind) && lag_days != 0 {
        bail!("symmetric task links cannot have lag");
    }
    if (source_project_id == target_project_id) != std::sync::Arc::ptr_eq(source_pool, target_pool)
    {
        bail!("task relation pools do not match endpoint projects");
    }
    let project_pools = if source_project_id == target_project_id {
        vec![(source_project_id, source_pool)]
    } else {
        vec![
            (source_project_id, source_pool),
            (target_project_id, target_pool),
        ]
    };
    for (project_id, pool) in project_pools {
        let record = super::repository::project_record(project_id)?
            .ok_or_else(|| anyhow!("task relation project missing"))?;
        let status = super::task_index::sync_project(&record, pool, 4096)?;
        if status.lag != 0 {
            bail!("task relation endpoint index has not caught up");
        }
    }
    let relation_id = uuid::Uuid::new_v4().to_string();
    let pending = super::models::TaskRelationRoute {
        relation_id: relation_id.clone(),
        owning_project_id: source_project_id.to_string(),
        link_id: 0,
        source_task_id: source_task_id.to_string(),
        target_task_id: target_task_id.to_string(),
        source_project_id: source_project_id.to_string(),
        target_project_id: target_project_id.to_string(),
        kind: kind.to_string(),
    };
    super::repository::prepare_task_relation_route(&pending)?;
    let mut row_committed = false;
    let result = (|| -> Result<TaskLinkMutation> {
        let mut target_guard = None;
        let source_conn = if source_project_id == target_project_id {
            source_pool.write().map_err(write_err)?
        } else if source_project_id < target_project_id {
            let source = source_pool.write().map_err(write_err)?;
            target_guard = Some(target_pool.write().map_err(write_err)?);
            source
        } else {
            target_guard = Some(target_pool.write().map_err(write_err)?);
            source_pool.write().map_err(write_err)?
        };
        let tx = source_conn.unchecked_transaction()?;
        admit_write(source_pool, Some(source_task_id))?;
        if source_project_id != target_project_id {
            admit_write(target_pool, Some(target_task_id))?;
        }
        let source =
            get_task_tx(&tx, source_task_id)?.ok_or_else(|| anyhow!("source task not found"))?;
        let target = if source_project_id == target_project_id {
            get_task_tx(&tx, target_task_id)?
        } else {
            let conn = target_guard.as_ref().expect("target writer is held");
            conn.query_row(
                &format!("SELECT {TASK_COLS} FROM tasks t WHERE t.task_id=?1"),
                [target_task_id],
                read_task,
            )
            .optional()?
        }
        .ok_or_else(|| anyhow!("target task not found"))?;
        if source.archived_at.is_some() || target.archived_at.is_some() {
            bail!("cannot link archived tasks");
        }
        tx.execute(
            "INSERT INTO task_links(relation_id,source_task_id,target_task_id,             source_project_id,target_project_id,kind,lag_days,created_by)              VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![relation_id,source_task_id,target_task_id,source_project_id,
                    target_project_id,kind,lag_days,actor],
        )?;
        let link_id = tx.last_insert_rowid();
        let payload = json!({"relation_id":relation_id,"link_id":link_id,
            "source_task_id":source_task_id,"target_task_id":target_task_id,
            "source_project_id":source_project_id,"target_project_id":target_project_id,
            "kind":kind,"lag_days":lag_days});
        let source_event_id = record_event(
            &tx,
            source_task_id,
            actor,
            "link_added",
            Value::Null,
            payload.clone(),
        )?;
        let target_event_id = if source_project_id == target_project_id {
            record_event(
                &tx,
                target_task_id,
                actor,
                "link_added",
                Value::Null,
                payload.clone(),
            )?
        } else {
            0
        };
        tx.commit()?;
        row_committed = true;
        let target_event_id = if source_project_id == target_project_id {
            target_event_id
        } else {
            let conn = target_guard.as_ref().expect("target writer is held");
            let tx = conn.unchecked_transaction()?;
            admit_write(target_pool, Some(target_task_id))?;
            let event_id = record_event(
                &tx,
                target_task_id,
                actor,
                "link_added",
                Value::Null,
                payload,
            )?;
            tx.commit()?;
            event_id
        };
        Ok(TaskLinkMutation {
            link: TaskLinkRecord {
                link_id,
                relation_id: relation_id.clone(),
                source_task_id: source_task_id.to_string(),
                target_task_id: target_task_id.to_string(),
                source_project_id: source_project_id.to_string(),
                target_project_id: target_project_id.to_string(),
                kind: kind.to_string(),
                lag_days,
                other_task_key: target.task_key,
                other_task_title: target.title,
            },
            source_event_id,
            target_event_id,
        })
    })();
    let mutation = match result {
        Ok(mutation) => mutation,
        Err(error) => {
            if !row_committed {
                super::repository::abort_task_relation_route(&relation_id)?;
            }
            return Err(error);
        }
    };
    super::repository::publish_task_relation_route(&super::models::TaskRelationRoute {
        link_id: mutation.link.link_id,
        ..pending
    })?;
    Ok(mutation)
}

pub fn delete_task_link(
    source_pool: &DbPool,
    target_pool: &DbPool,
    link_id: i64,
    actor: &str,
) -> Result<Option<TaskLinkMutation>> {
    let Some(existing) = get_task_link(source_pool, link_id)? else {
        return Ok(None);
    };
    if (existing.source_project_id == existing.target_project_id)
        != std::sync::Arc::ptr_eq(source_pool, target_pool)
    {
        bail!("task relation pools do not match endpoint projects");
    }
    let mut target_guard = None;
    let source_conn = if existing.source_project_id == existing.target_project_id {
        source_pool.write().map_err(write_err)?
    } else if existing.source_project_id < existing.target_project_id {
        let source = source_pool.write().map_err(write_err)?;
        target_guard = Some(target_pool.write().map_err(write_err)?);
        source
    } else {
        target_guard = Some(target_pool.write().map_err(write_err)?);
        source_pool.write().map_err(write_err)?
    };
    let tx = source_conn.unchecked_transaction()?;
    admit_write(source_pool, Some(&existing.source_task_id))?;
    if existing.source_project_id != existing.target_project_id {
        admit_write(target_pool, Some(&existing.target_task_id))?;
    }
    let link = tx.query_row(
        "SELECT l.link_id,l.relation_id,l.source_task_id,l.target_task_id,         l.source_project_id,l.target_project_id,l.kind,l.lag_days,         COALESCE(t.task_key,''),COALESCE(t.title,'')          FROM task_links l LEFT JOIN tasks t ON t.task_id=l.target_task_id WHERE l.link_id=?1",
        [link_id],read_task_link,
    ).optional()?;
    let Some(link) = link else {
        return Ok(None);
    };
    if link.relation_id != existing.relation_id
        || link.source_project_id != existing.source_project_id
        || link.target_project_id != existing.target_project_id
    {
        bail!("task relation changed while deleting");
    }
    let source =
        get_task_tx(&tx, &link.source_task_id)?.ok_or_else(|| anyhow!("source task not found"))?;
    let target = if link.source_project_id == link.target_project_id {
        get_task_tx(&tx, &link.target_task_id)?
    } else {
        target_guard
            .as_ref()
            .expect("target writer is held")
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks t WHERE t.task_id=?1"),
                [&link.target_task_id],
                read_task,
            )
            .optional()?
    }
    .ok_or_else(|| anyhow!("target task not found"))?;
    if source.archived_at.is_some() || target.archived_at.is_some() {
        bail!("archived task links cannot be deleted");
    }
    let payload = json!({"relation_id":link.relation_id,"link_id":link.link_id,
        "source_task_id":link.source_task_id,"target_task_id":link.target_task_id,
        "source_project_id":link.source_project_id,"target_project_id":link.target_project_id,
        "kind":link.kind,"lag_days":link.lag_days});
    super::repository::prepare_task_relation_deletion(
        &super::models::TaskRelationRoute {
            relation_id: link.relation_id.clone(),
            owning_project_id: link.source_project_id.clone(),
            link_id: link.link_id,
            source_task_id: link.source_task_id.clone(),
            target_task_id: link.target_task_id.clone(),
            source_project_id: link.source_project_id.clone(),
            target_project_id: link.target_project_id.clone(),
            kind: link.kind.clone(),
        },
        actor,
    )?;
    let mut source_committed = false;
    let deletion = (|| -> Result<(i64, i64)> {
        tx.execute("DELETE FROM task_links WHERE link_id=?1", [link_id])?;
        let source_event_id = record_event(
            &tx,
            &link.source_task_id,
            actor,
            "link_deleted",
            payload.clone(),
            Value::Null,
        )?;
        let target_event_id = if link.source_project_id == link.target_project_id {
            record_event(
                &tx,
                &link.target_task_id,
                actor,
                "link_deleted",
                payload.clone(),
                Value::Null,
            )?
        } else {
            0
        };
        tx.commit()?;
        source_committed = true;
        if link.source_project_id == link.target_project_id {
            Ok((source_event_id, target_event_id))
        } else {
            let conn = target_guard.as_ref().expect("target writer is held");
            let tx = conn.unchecked_transaction()?;
            let exists: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
                 AND json_extract(before_json,'$.relation_id')=?2)",
                params![link.target_task_id, link.relation_id],
                |row| row.get(0),
            )?;
            let target_event_id = if exists == 0 {
                record_event(
                    &tx,
                    &link.target_task_id,
                    actor,
                    "link_deleted",
                    payload,
                    Value::Null,
                )?
            } else {
                tx.query_row(
                    "SELECT event_id FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
                     AND json_extract(before_json,'$.relation_id')=?2 ORDER BY event_id DESC LIMIT 1",
                    params![link.target_task_id, link.relation_id],
                    |row| row.get(0),
                )?
            };
            tx.commit()?;
            Ok((source_event_id, target_event_id))
        }
    })();
    let (source_event_id, target_event_id) = match deletion {
        Ok(ids) => ids,
        Err(error) => {
            if !source_committed {
                super::repository::abort_task_relation_deletion(&link.relation_id)?;
            }
            return Err(error);
        }
    };
    drop(target_guard);
    drop(source_conn);
    super::repository::finish_task_relation_deletion(&link.relation_id)?;
    Ok(Some(TaskLinkMutation {
        link,
        source_event_id,
        target_event_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_studio::project_db;

    fn storage() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let central_dir = tempfile::tempdir().expect("registry tempdir");
        let _ = crate::project_studio::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let project_id = uuid::Uuid::new_v4().to_string();
        let org_id = uuid::Uuid::new_v4().to_string();
        crate::project_studio::repository::create_project(
            &project_id,
            &org_id,
            "Test project",
            "",
            "custom",
            "[\"tasks\"]",
            "u1",
            dir.path().to_str().expect("path"),
            "PR",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("registered project");
        let pool = project_db::open(&project_id).expect("project schema");
        (dir, pool)
    }

    fn input<'a>(task_type: &'a str, title: &'a str, status: &'a str) -> TaskInput<'a> {
        TaskInput {
            task_type,
            title,
            description_md: "",
            severity: "",
            priority: "medium",
            status,
            assigned_to: "",
            due_date: "",
            parent_task_id: None,
            links_json: "[]",
            attachments_json: "[]",
        }
    }

    #[test]
    fn task_history_is_atomic_and_noop_does_not_emit() {
        let (_dir, pool) = storage();
        let created =
            create_task(&pool, &input("feature", "Feature", "todo"), "u1").expect("create");
        assert_eq!(created.task_key, "PR-1");
        assert!(project_key_prefix_locked(&pool).expect("lock"));
        assert_eq!(
            list_task_events(&pool, &created.task_id, None, 50)
                .expect("events")
                .0
                .len(),
            1
        );
        let unchanged = update_task(
            &pool,
            &created.task_id,
            &input("feature", "Feature", "todo"),
            "u1",
        )
        .expect("save")
        .expect("found");
        assert!(!unchanged.changed);
        assert!(unchanged.event_ids.is_empty());
        let moved = set_task_status(&pool, &created.task_id, "review", "u2")
            .expect("move")
            .expect("found");
        assert_eq!(moved.previous_status.as_deref(), Some("todo"));
        assert_eq!(moved.event_ids.len(), 1);
        let unchanged = set_task_status(&pool, &created.task_id, "review", "u2")
            .expect("repeat")
            .expect("found");
        assert!(!unchanged.changed);
        let (events, _) = list_task_events(&pool, &created.task_id, None, 50).expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "status_changed");
        assert_eq!(events[0].before_json, "\"todo\"");
        assert_eq!(events[0].after_json, "\"review\"");
        assert_eq!(
            task_status_durations(&pool, &created.task_id)
                .expect("durations")
                .len(),
            2
        );
    }

    #[test]
    fn assignment_events_distinguish_first_assignment_transfer_and_removal() {
        let (_dir, pool) = storage();
        let task = create_task(&pool, &input("feature", "Feature", "todo"), "u1").expect("create");
        for (assignee, expected_kind) in
            [("u2", "assigned"), ("u3", "reassigned"), ("", "unassigned")]
        {
            let mut updated = input("feature", "Feature", "todo");
            updated.assigned_to = assignee;
            let mutation = update_task(&pool, &task.task_id, &updated, "u1")
                .expect("update")
                .expect("task");
            assert_eq!(mutation.event_ids.len(), 1);
            let (events, _) = list_task_events(&pool, &task.task_id, None, 1).expect("events");
            assert_eq!(events[0].kind, expected_kind);
        }
    }

    #[test]
    fn handover_commits_note_and_assignment_together_and_pins_only_latest() {
        let (_dir, pool) = storage();
        let mut assigned = input("feature", "Feature", "todo");
        assigned.assigned_to = "u1";
        let task = create_task(&pool, &assigned, "u1").expect("create");
        let handover = TaskHandoverInput {
            actor: "u1",
            note_md: "The current context",
            mention_user_ids: &["u2".to_string()],
            direction: TaskHandoverDirection::Over,
            handover_id: Some("handover-1"),
        };
        let changed = reassign_open(&pool, &task.task_id, "u1", "u2", &handover)
            .expect("handover")
            .expect("task");
        assert_eq!(changed.event_ids.len(), 2);
        let comment_id = changed.handover_comment_id.expect("handover comment");
        assert_eq!(
            latest_handover_comment_id(&pool, &task.task_id).expect("pin"),
            Some(comment_id.clone())
        );
        let replay = reassign_open(&pool, &task.task_id, "u1", "u2", &handover)
            .expect("exact replay")
            .expect("proven handover");
        assert!(!replay.changed);
        assert!(replay.event_ids.is_empty());
        assert_eq!(
            replay.handover_comment_id.as_deref(),
            Some(comment_id.as_str())
        );
        for (from, actor, direction, handover_id) in [
            ("u1", "u1", TaskHandoverDirection::Over, Some("other-id")),
            (
                "u1",
                "other-actor",
                TaskHandoverDirection::Over,
                Some("handover-1"),
            ),
            (
                "other-from",
                "u1",
                TaskHandoverDirection::Over,
                Some("handover-1"),
            ),
            ("u1", "u1", TaskHandoverDirection::Back, Some("handover-1")),
        ] {
            let different = TaskHandoverInput {
                actor,
                note_md: handover.note_md,
                mention_user_ids: handover.mention_user_ids,
                direction,
                handover_id,
            };
            assert!(reassign_open(&pool, &task.task_id, from, "u2", &different)
                .expect("unproven replay")
                .is_none());
        }
        assert!(reassign_open(&pool, &task.task_id, "u1", "u3", &handover)
            .expect("stale attempt")
            .is_none());
        assert_eq!(
            list_comments(&pool, &task.task_id).expect("comments").len(),
            1
        );
        let mut ordinary = input("feature", "Feature", "todo");
        ordinary.assigned_to = "u3";
        update_task(&pool, &task.task_id, &ordinary, "u2")
            .expect("ordinary assignment")
            .expect("task");
        ordinary.assigned_to = "u2";
        update_task(&pool, &task.task_id, &ordinary, "u3")
            .expect("return by ordinary assignment")
            .expect("task");
        assert!(reassign_open(&pool, &task.task_id, "u1", "u2", &handover)
            .expect("old handover after intervening assignment")
            .is_none());
        assert_eq!(
            latest_handover_comment_id(&pool, &task.task_id).expect("pin"),
            None
        );
        let (events, _) = list_task_events(&pool, &task.task_id, None, 10).expect("events");
        assert!(events
            .iter()
            .any(|event| event.kind == "handed_over" && event.after_json.contains("handover-1")));
    }

    #[test]
    fn archived_task_rejects_comment_and_link_mutations_until_restored() {
        let (_dir, pool) = storage();
        let first =
            create_task(&pool, &input("feature", "First", "todo"), "u1").expect("first task");
        let second =
            create_task(&pool, &input("feature", "Second", "todo"), "u1").expect("second task");
        let comment = add_comment(&pool, &first.task_id, "u1", "Original", &[])
            .expect("comment")
            .comment
            .expect("comment row");
        let link = add_task_link(
            &pool,
            &pool,
            &project_db::project_id_of(&pool).expect("project id"),
            &project_db::project_id_of(&pool).expect("project id"),
            &first.task_id,
            &second.task_id,
            "related",
            0,
            "u1",
        )
        .expect("link");
        set_task_archived(&pool, &first.task_id, true, "u1")
            .expect("archive")
            .expect("task");
        let first_events = list_task_events(&pool, &first.task_id, None, 100)
            .expect("first events")
            .0
            .len();
        let second_events = list_task_events(&pool, &second.task_id, None, 100)
            .expect("second events")
            .0
            .len();
        assert!(edit_comment(&pool, &comment.comment_id, "u1", "Changed", &[]).is_err());
        assert!(delete_comment(&pool, &comment.comment_id, "u1").is_err());
        assert!(delete_task_link(&pool, &pool, link.link.link_id, "u1").is_err());
        assert_eq!(
            get_comment(&pool, &comment.comment_id)
                .expect("comment")
                .unwrap()
                .body_md,
            "Original"
        );
        assert_eq!(
            list_task_links(&pool, &first.task_id).expect("links").len(),
            1
        );
        assert_eq!(
            list_task_events(&pool, &first.task_id, None, 100)
                .expect("events")
                .0
                .len(),
            first_events
        );
        assert_eq!(
            list_task_events(&pool, &second.task_id, None, 100)
                .expect("events")
                .0
                .len(),
            second_events
        );

        set_task_archived(&pool, &first.task_id, false, "u1")
            .expect("restore")
            .expect("task");
        set_task_archived(&pool, &second.task_id, true, "u1")
            .expect("archive target")
            .expect("task");
        assert!(delete_task_link(&pool, &pool, link.link.link_id, "u1").is_err());
        set_task_archived(&pool, &second.task_id, false, "u1")
            .expect("restore target")
            .expect("task");
        assert!(
            edit_comment(&pool, &comment.comment_id, "u1", "Changed", &[])
                .expect("edit restored")
                .expect("comment")
                .changed
        );
        assert!(
            delete_comment(&pool, &comment.comment_id, "u1")
                .expect("delete restored")
                .expect("comment")
                .changed
        );
        assert!(delete_task_link(&pool, &pool, link.link.link_id, "u1")
            .expect("unlink restored")
            .is_some());
    }

    #[test]
    fn custom_types_parent_rules_and_dependency_cycles() {
        let (_dir, pool) = storage();
        let custom = save_task_type(&pool, "incident", "Incident", "Operational work", 70, true)
            .expect("custom type");
        assert_eq!(custom.type_id, "incident");
        let epic = create_task(&pool, &input("epic", "Epic", "todo"), "u1").expect("epic");
        let mut child_input = input("subtask", "Child", "todo");
        assert!(create_task(&pool, &child_input, "u1").is_err());
        child_input.parent_task_id = Some(&epic.task_id);
        let child = create_task(&pool, &child_input, "u1").expect("child");
        let other =
            create_task(&pool, &input("incident", "Incident", "todo"), "u1").expect("other");
        save_task_type(&pool, "incident", "Incident", "Operational work", 70, false)
            .expect("deactivate");
        assert!(create_task(&pool, &input("incident", "Another", "todo"), "u1").is_err());
        assert!(add_task_link(
            &pool,
            &pool,
            &project_db::project_id_of(&pool).expect("project id"),
            &project_db::project_id_of(&pool).expect("project id"),
            &child.task_id,
            &child.task_id,
            "related",
            0,
            "u1"
        )
        .is_err());
        let link = add_task_link(
            &pool,
            &pool,
            &project_db::project_id_of(&pool).expect("project id"),
            &project_db::project_id_of(&pool).expect("project id"),
            &child.task_id,
            &other.task_id,
            "fs",
            2,
            "u1",
        )
        .expect("dependency");
        assert_eq!(link.link.kind, "fs");
        assert_eq!(
            list_task_links(&pool, &other.task_id)
                .expect("reverse")
                .len(),
            1
        );
        assert!(add_task_link(
            &pool,
            &pool,
            &project_db::project_id_of(&pool).expect("project id"),
            &project_db::project_id_of(&pool).expect("project id"),
            &other.task_id,
            &child.task_id,
            "ss",
            0,
            "u1"
        )
        .is_err());
        assert!(add_task_link(
            &pool,
            &pool,
            &project_db::project_id_of(&pool).expect("project id"),
            &project_db::project_id_of(&pool).expect("project id"),
            &child.task_id,
            &other.task_id,
            "fs",
            2,
            "u1"
        )
        .is_err());
        assert!(delete_task_link(&pool, &pool, link.link.link_id, "u1")
            .expect("delete")
            .is_some());
        assert_eq!(
            list_task_events(&pool, &child.task_id, None, 50)
                .expect("history")
                .0
                .len(),
            3
        );
    }

    #[test]
    fn updating_existing_task_parent_clears_restores_and_rejects_cycles() {
        let (_dir, pool) = storage();
        let epic = create_task(&pool, &input("epic", "Epic", "todo"), "u1").expect("epic");
        let mut feature_input = input("feature", "Feature", "todo");
        feature_input.parent_task_id = Some(&epic.task_id);
        let feature = create_task(&pool, &feature_input, "u1").expect("feature under epic");

        let cleared = update_task(
            &pool,
            &feature.task_id,
            &input("feature", "Feature", "todo"),
            "u1",
        )
        .expect("clear parent")
        .expect("feature");
        assert!(cleared.changed);
        assert_eq!(
            get_task(&pool, &feature.task_id)
                .unwrap()
                .unwrap()
                .parent_task_id,
            None
        );
        let restored = update_task(&pool, &feature.task_id, &feature_input, "u1")
            .expect("restore epic parent")
            .expect("feature");
        assert!(restored.changed);
        assert_eq!(
            get_task(&pool, &feature.task_id)
                .unwrap()
                .unwrap()
                .parent_task_id,
            Some(epic.task_id.clone())
        );

        let mut self_parent = input("feature", "Feature", "todo");
        self_parent.parent_task_id = Some(&feature.task_id);
        assert!(update_task(&pool, &feature.task_id, &self_parent, "u1")
            .unwrap_err()
            .to_string()
            .contains("own parent"));
        let mut missing_parent = input("feature", "Feature", "todo");
        missing_parent.parent_task_id = Some("missing-task");
        assert!(update_task(&pool, &feature.task_id, &missing_parent, "u1")
            .unwrap_err()
            .to_string()
            .contains("parent task not found"));

        let mut first_input = input("subtask", "First child", "todo");
        first_input.parent_task_id = Some(&epic.task_id);
        let first = create_task(&pool, &first_input, "u1").expect("first child");
        let mut second_input = input("subtask", "Second child", "todo");
        second_input.parent_task_id = Some(&first.task_id);
        let second = create_task(&pool, &second_input, "u1").expect("second child");
        let mut cycle_input = input("subtask", "First child", "todo");
        cycle_input.parent_task_id = Some(&second.task_id);
        assert!(update_task(&pool, &first.task_id, &cycle_input, "u1")
            .unwrap_err()
            .to_string()
            .contains("cycle"));
        assert_eq!(
            get_task(&pool, &first.task_id)
                .unwrap()
                .unwrap()
                .parent_task_id,
            Some(epic.task_id)
        );
        assert_eq!(
            list_task_events(&pool, &first.task_id, None, 10)
                .unwrap()
                .0
                .len(),
            1
        );
        assert_eq!(
            list_task_events(&pool, &feature.task_id, None, 10)
                .unwrap()
                .0
                .len(),
            3
        );
    }

    #[test]
    fn comment_edits_and_historical_attachments_remain_auditable() {
        let (_dir, pool) = storage();
        let sha = "a".repeat(64);
        let attachment = format!("[{{\"sha256\":\"{sha}\",\"name\":\"clip.mp4\",\"size_bytes\":17,\"mime\":\"video/mp4\"}}]");
        let mut attached = input("defect", "Defect", "todo");
        attached.attachments_json = &attachment;
        attached.severity = "high";
        let created = create_task(&pool, &attached, "u1").expect("create");
        let comment = add_comment(&pool, &created.task_id, "u1", "Original", &["u2".into()])
            .expect("comment");
        let comment_id = comment.comment.expect("record").comment_id;
        let edited = edit_comment(
            &pool,
            &comment_id,
            "u1",
            "Changed",
            &["u2".into(), "u3".into()],
        )
        .expect("edit")
        .expect("found");
        assert_eq!(edited.added_mention_user_ids, vec!["u3"]);
        assert!(edit_comment(
            &pool,
            &comment_id,
            "u1",
            "Changed",
            &["u2".into(), "u3".into()]
        )
        .expect("noop")
        .expect("found")
        .event_id
        .is_none());
        delete_comment(&pool, &comment_id, "u1")
            .expect("delete")
            .expect("found");
        let mut clean = input("defect", "Defect", "todo");
        clean.severity = "high";
        update_task(&pool, &created.task_id, &clean, "u1").expect("remove attachment");
        assert!(attachment_referenced(&pool, &created.task_id, &sha).expect("historic reference"));
        let inventory = task_attachment_inventory(&pool, &created.task_id).expect("inventory");
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].sha256, sha);
        assert_eq!(
            crate::project_studio::repository::blob_ref_count(&pool, &sha).expect("gc ref"),
            2
        );
        let archived = delete_task(&pool, &created.task_id, "u1")
            .expect("delete")
            .expect("found");
        assert!(archived.archived);
        assert_eq!(
            list_tasks(&pool, &TaskFilters::default(), 0, 50)
                .expect("list")
                .1,
            0
        );
        assert_eq!(
            list_tasks(
                &pool,
                &TaskFilters {
                    include_archived: true,
                    ..TaskFilters::default()
                },
                0,
                50,
            )
            .expect("archive list")
            .1,
            1
        );
        assert!(get_task(&pool, &created.task_id).expect("get").is_some());
    }
}
