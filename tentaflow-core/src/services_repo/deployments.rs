// ============ File: services_repo/deployments.rs — CRUD over deployments_v2 (audit trail) ============

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

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
}

const COLS: &str = "id, engine_id, deploy_method, status, started_at, finished_at, \
    error_text, config_json";

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
