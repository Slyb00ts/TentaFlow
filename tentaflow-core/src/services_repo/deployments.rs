// ============ File: services_repo/deployments.rs — CRUD over deployments_v2 (audit trail) ============

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::DbPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentStatus {
    Pending,
    Running,
    Success,
    Failed,
}

impl DeploymentStatus {
    pub fn as_db_tag(self) -> &'static str {
        match self {
            DeploymentStatus::Pending => "pending",
            DeploymentStatus::Running => "running",
            DeploymentStatus::Success => "success",
            DeploymentStatus::Failed => "failed",
        }
    }

    pub fn parse(tag: &str) -> Result<Self> {
        Ok(match tag {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "success" => Self::Success,
            "failed" => Self::Failed,
            other => return Err(anyhow!("unknown deployment status: {}", other)),
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewDeployment {
    pub engine_id: String,
    pub deploy_method: String,
    pub status: DeploymentStatus,
    pub config_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeploymentRow {
    pub id: i64,
    pub engine_id: String,
    pub deploy_method: String,
    pub status: DeploymentStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_text: Option<String>,
    pub config_json: Option<String>,
    pub slug: Option<String>,
    pub log_tail: String,
}

const COLS: &str = "id, engine_id, deploy_method, status, started_at, finished_at, \
    error_text, config_json, slug, log_tail";

/// Maximum number of log lines kept in `log_tail`. Older lines are dropped
/// FIFO-style when this limit is exceeded so the column stays bounded.
const LOG_TAIL_MAX_LINES: usize = 5_000;

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeploymentRow> {
    let status_tag: String = row.get("status")?;
    let status = DeploymentStatus::parse(&status_tag).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(DeploymentRow {
        id: row.get("id")?,
        engine_id: row.get("engine_id")?,
        deploy_method: row.get("deploy_method")?,
        status,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
        error_text: row.get("error_text")?,
        config_json: row.get("config_json")?,
        slug: row.get("slug")?,
        log_tail: row.get("log_tail")?,
    })
}

pub fn insert(conn: &Connection, new: &NewDeployment) -> Result<i64> {
    conn.execute(
        "INSERT INTO deployments_v2 (engine_id, deploy_method, status, config_json) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            new.engine_id,
            new.deploy_method,
            new.status.as_db_tag(),
            new.config_json,
        ],
    )
    .context("insert deployments_v2")?;
    Ok(conn.last_insert_rowid())
}

/// Inserts an audit row pre-bound to a client-supplied slug. Status is forced
/// to `Running` because callers always create the row at the moment they
/// kick off the deploy job. The slug must be unique (enforced at DB level).
pub fn create_with_slug(
    conn: &Connection,
    engine_id: &str,
    deploy_method: &str,
    slug: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO deployments_v2 (engine_id, deploy_method, status, slug) \
         VALUES (?1, ?2, 'running', ?3)",
        params![engine_id, deploy_method, slug],
    )
    .context("insert deployments_v2 with slug")?;
    Ok(conn.last_insert_rowid())
}

/// Appends one line to `log_tail`, clamped to `LOG_TAIL_MAX_LINES` so the
/// column does not grow without bound on long-running builds.
pub fn append_log_line(db: &DbPool, slug: &str, line: &str) -> Result<()> {
    let conn = db
        .lock()
        .map_err(|e| anyhow!("pool lock poisoned: {}", e))?;
    let current: Option<String> = conn
        .query_row(
            "SELECT log_tail FROM deployments_v2 WHERE slug = ?1",
            params![slug],
            |r| r.get(0),
        )
        .optional()?;
    let Some(mut tail) = current else {
        return Err(anyhow!("append_log_line: slug='{}' not found", slug));
    };
    if !tail.is_empty() && !tail.ends_with('\n') {
        tail.push('\n');
    }
    tail.push_str(line);
    tail.push('\n');

    // Trim from the front when the line count exceeds the cap.
    let lines: Vec<&str> = tail.lines().collect();
    let new_tail = if lines.len() > LOG_TAIL_MAX_LINES {
        lines[lines.len() - LOG_TAIL_MAX_LINES..].join("\n")
    } else {
        tail.trim_end_matches('\n').to_string()
    };

    conn.execute(
        "UPDATE deployments_v2 SET log_tail = ?2 WHERE slug = ?1",
        params![slug, new_tail],
    )?;
    Ok(())
}

/// Looks up a deployment row by its public slug. Used by the log stream
/// handler to honour client subscriptions even if the auto-increment id is
/// not known on the wire.
pub fn get_by_slug(db: &DbPool, slug: &str) -> Result<Option<DeploymentRow>> {
    let conn = db
        .lock()
        .map_err(|e| anyhow!("pool lock poisoned: {}", e))?;
    let sql = format!("SELECT {} FROM deployments_v2 WHERE slug = ?1", COLS);
    Ok(conn
        .query_row(&sql, params![slug], map_row)
        .optional()
        .context("get_by_slug deployments_v2")?)
}

pub fn mark_finished(
    conn: &Connection,
    id: i64,
    status: DeploymentStatus,
    error_text: Option<&str>,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE deployments_v2 SET status = ?2, finished_at = CURRENT_TIMESTAMP, \
         error_text = ?3 WHERE id = ?1",
        params![id, status.as_db_tag(), error_text],
    )?;
    if n == 0 {
        return Err(anyhow!("mark_finished: deployment id={} not found", id));
    }
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<DeploymentRow>> {
    let sql = format!("SELECT {} FROM deployments_v2 WHERE id = ?1", COLS);
    Ok(conn
        .query_row(&sql, params![id], map_row)
        .optional()
        .context("get deployments_v2")?)
}

pub fn list_recent(conn: &Connection, limit: i64) -> Result<Vec<DeploymentRow>> {
    let sql = format!(
        "SELECT {} FROM deployments_v2 ORDER BY id DESC LIMIT ?1",
        COLS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn open_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn slug_unique_constraint() {
        let db = open_db();
        let conn = db.lock().unwrap();
        create_with_slug(&conn, "vllm", "docker", "abc123").unwrap();
        let dup = create_with_slug(&conn, "vllm", "docker", "abc123");
        assert!(dup.is_err(), "duplicate slug must violate unique index");
    }

    #[test]
    fn append_log_line_persists() {
        let db = open_db();
        {
            let conn = db.lock().unwrap();
            create_with_slug(&conn, "vllm", "docker", "slug-aa").unwrap();
        }
        append_log_line(&db, "slug-aa", "hello").unwrap();
        append_log_line(&db, "slug-aa", "world").unwrap();
        let row = get_by_slug(&db, "slug-aa").unwrap().unwrap();
        assert_eq!(row.log_tail, "hello\nworld");
    }

    #[test]
    fn append_log_line_unknown_slug_errors() {
        let db = open_db();
        let err = append_log_line(&db, "missing", "x");
        assert!(err.is_err());
    }

    #[test]
    fn get_by_slug_roundtrip() {
        let db = open_db();
        let id = {
            let conn = db.lock().unwrap();
            create_with_slug(&conn, "ollama", "external", "slug-bb").unwrap()
        };
        let row = get_by_slug(&db, "slug-bb").unwrap().unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.engine_id, "ollama");
        assert_eq!(row.deploy_method, "external");
        assert_eq!(row.status, DeploymentStatus::Running);
    }
}
