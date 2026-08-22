// ============ File: services_repo/deployments.rs — CRUD over deployments (audit trail) ============

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::DbPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentStatus {
    Deploying,
    Success,
    Failed,
    Cancelled,
    Interrupted,
}

impl DeploymentStatus {
    pub fn as_db_tag(self) -> &'static str {
        match self {
            DeploymentStatus::Deploying => "deploying",
            DeploymentStatus::Success => "success",
            DeploymentStatus::Failed => "failed",
            DeploymentStatus::Cancelled => "cancelled",
            DeploymentStatus::Interrupted => "interrupted",
        }
    }

    pub fn parse(tag: &str) -> Result<Self> {
        Ok(match tag {
            "deploying" => Self::Deploying,
            "success" => Self::Success,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            other => return Err(anyhow!("unknown deployment status: {}", other)),
        })
    }
}

#[derive(Debug, Clone)]
pub struct DeploymentRow {
    pub id: i64,
    pub engine_id: String,
    pub deploy_method: String,
    pub status: DeploymentStatus,
    pub target_service_id: Option<i64>,
    pub node_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub error_text: Option<String>,
    pub config_json: Option<String>,
    pub slug: Option<String>,
    pub log_tail: String,
}

// Schema deployments uzywa nazw `deploy_id` i `error_message` (zgodnie z
// `db::repository::deployments`). Ten modul zachowuje stare aliasy w
// strukturze (slug/error_text) zeby nie psuc callerow, ale w SQL siegamy do
// rzeczywistych nazw kolumn.
const COLS: &str = "id, engine_id, deploy_method, status, started_at, finished_at, \
    error_message AS error_text, config_json, deploy_id AS slug, log_tail, \
    target_service_id, node_id, updated_at";

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
        target_service_id: row.get("target_service_id")?,
        node_id: row.get("node_id")?,
        started_at: row.get("started_at")?,
        updated_at: row.get("updated_at")?,
        finished_at: row.get("finished_at")?,
        error_text: row.get("error_text")?,
        config_json: row.get("config_json")?,
        slug: row.get("slug")?,
        log_tail: row.get("log_tail")?,
    })
}

pub fn create_with_slug(
    conn: &Connection,
    engine_id: &str,
    deploy_method: &str,
    slug: &str,
    node_id: &str,
    target_service_id: i64,
    config_json: &str,
) -> Result<i64> {
    conn.execute(
        // `updated_at` defaults to '' in the schema, so it is stamped here as
        // well: a row that never reached a progress update must still expose a
        // usable timestamp to the UI and to stale-deploy diagnostics.
        "INSERT INTO deployments (
             engine_id, deploy_method, status, deploy_id, node_id, target_service_id, config_json,
             updated_at
         ) VALUES (?1, ?2, 'deploying', ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)",
        params![
            engine_id,
            deploy_method,
            slug,
            node_id,
            target_service_id,
            config_json
        ],
    )
    .context("insert deployments with slug")?;
    Ok(conn.last_insert_rowid())
}

/// Appends one line to `log_tail`, clamped to `LOG_TAIL_MAX_LINES` so the
/// column does not grow without bound on long-running builds.
pub fn append_log_line(db: &DbPool, slug: &str, line: &str) -> Result<()> {
    let conn = db
        .write()
        .map_err(|e| anyhow!("pool lock poisoned: {}", e))?;
    let current: Option<String> = conn
        .query_row(
            "SELECT log_tail FROM deployments WHERE deploy_id = ?1",
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
        "UPDATE deployments SET log_tail = ?2 WHERE deploy_id = ?1",
        params![slug, new_tail],
    )?;
    Ok(())
}

/// Looks up a deployment row by its public slug. Used by the log stream
/// handler to honour client subscriptions even if the auto-increment id is
/// not known on the wire.
pub fn get_by_slug(db: &DbPool, slug: &str) -> Result<Option<DeploymentRow>> {
    let conn = db
        .read()
        .map_err(|e| anyhow!("pool lock poisoned: {}", e))?;
    let sql = format!("SELECT {} FROM deployments WHERE deploy_id = ?1", COLS);
    Ok(conn
        .query_row(&sql, params![slug], map_row)
        .optional()
        .context("get_by_slug deployments")?)
}

pub fn mark_finished(
    conn: &Connection,
    id: i64,
    status: DeploymentStatus,
    error_text: Option<&str>,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE deployments SET status = ?2, finished_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP, error_message = ?3 WHERE id = ?1",
        params![id, status.as_db_tag(), error_text],
    )?;
    if n == 0 {
        return Err(anyhow!("mark_finished: deployment id={} not found", id));
    }
    Ok(())
}

pub fn set_progress(
    conn: &Connection,
    slug: &str,
    status: DeploymentStatus,
    phase: &str,
    progress_pct: u32,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE deployments
            SET status = ?2,
                phase = ?3,
                progress_pct = ?4,
                updated_at = CURRENT_TIMESTAMP
          WHERE deploy_id = ?1",
        params![slug, status.as_db_tag(), phase, progress_pct as i64],
    )?;
    if n == 0 {
        return Err(anyhow!("set_progress: slug='{}' not found", slug));
    }
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<DeploymentRow>> {
    let sql = format!("SELECT {} FROM deployments WHERE id = ?1", COLS);
    Ok(conn
        .query_row(&sql, params![id], map_row)
        .optional()
        .context("get deployments")?)
}

pub fn list_recent(conn: &Connection, limit: i64) -> Result<Vec<DeploymentRow>> {
    let sql = format!("SELECT {} FROM deployments ORDER BY id DESC LIMIT ?1", COLS);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_resumable(conn: &Connection) -> Result<Vec<DeploymentRow>> {
    let sql = format!(
        "SELECT {} FROM deployments
          WHERE status = 'interrupted' AND target_service_id IS NOT NULL
          ORDER BY id ASC",
        COLS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn open_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn insert_service(conn: &Connection) -> i64 {
        crate::services_repo::services::insert(
            conn,
            &crate::services_repo::services::NewService::minimal(
                "vllm",
                crate::services_repo::services::DeployMethod::Docker,
                crate::services::transport::Transport::HttpDirect,
            ),
        )
        .unwrap()
    }

    #[test]
    fn slug_unique_constraint() {
        let db = open_db();
        let conn = db.write().unwrap();
        let service_id = insert_service(&conn);
        create_with_slug(
            &conn, "vllm", "docker", "abc123", "node-a", service_id, "{}",
        )
        .unwrap();
        let dup = create_with_slug(
            &conn, "vllm", "docker", "abc123", "node-a", service_id, "{}",
        );
        assert!(dup.is_err(), "duplicate slug must violate unique index");
    }

    #[test]
    fn append_log_line_persists() {
        let db = open_db();
        {
            let conn = db.write().unwrap();
            let service_id = insert_service(&conn);
            create_with_slug(
                &conn, "vllm", "docker", "slug-aa", "node-a", service_id, "{}",
            )
            .unwrap();
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
            let conn = db.write().unwrap();
            let service_id = insert_service(&conn);
            create_with_slug(
                &conn, "ollama", "external", "slug-bb", "node-a", service_id, "{}",
            )
            .unwrap()
        };
        let row = get_by_slug(&db, "slug-bb").unwrap().unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.engine_id, "ollama");
        assert_eq!(row.deploy_method, "external");
        assert_eq!(row.status, DeploymentStatus::Deploying);
    }
}
