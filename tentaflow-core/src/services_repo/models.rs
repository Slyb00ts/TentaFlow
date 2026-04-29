// ============ File: services_repo/models.rs — CRUD over model_registry_v2 ============

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

#[derive(Debug, Clone)]
pub struct NewModel {
    pub service_id: i64,
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: String,
    pub context_length: Option<i64>,
    pub quantization: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct ModelRow {
    pub id: i64,
    pub service_id: i64,
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: String,
    pub context_length: Option<i64>,
    pub quantization: Option<String>,
    pub is_default: bool,
    pub created_at: String,
}

const COLS: &str = "id, service_id, model_name, display_name, capabilities, context_length, \
    quantization, is_default, created_at";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelRow> {
    Ok(ModelRow {
        id: row.get("id")?,
        service_id: row.get("service_id")?,
        model_name: row.get("model_name")?,
        display_name: row.get("display_name")?,
        capabilities: row.get("capabilities")?,
        context_length: row.get("context_length")?,
        quantization: row.get("quantization")?,
        is_default: row.get::<_, i64>("is_default")? != 0,
        created_at: row.get("created_at")?,
    })
}

pub fn insert(conn: &Connection, new: &NewModel) -> Result<i64> {
    conn.execute(
        "INSERT INTO model_registry_v2 (service_id, model_name, display_name, capabilities, \
            context_length, quantization, is_default) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new.service_id,
            new.model_name,
            new.display_name,
            new.capabilities,
            new.context_length,
            new.quantization,
            new.is_default as i64,
        ],
    )
    .context("insert model_registry_v2")?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_in_tx(tx: &Transaction<'_>, new: &NewModel) -> Result<i64> {
    tx.execute(
        "INSERT INTO model_registry_v2 (service_id, model_name, display_name, capabilities, \
            context_length, quantization, is_default) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new.service_id,
            new.model_name,
            new.display_name,
            new.capabilities,
            new.context_length,
            new.quantization,
            new.is_default as i64,
        ],
    )
    .context("insert model_registry_v2 (tx)")?;
    Ok(tx.last_insert_rowid())
}

pub fn get_by_name(conn: &Connection, model_name: &str) -> Result<Option<ModelRow>> {
    let sql = format!(
        "SELECT {} FROM model_registry_v2 WHERE model_name = ?1 LIMIT 1",
        COLS
    );
    Ok(conn
        .query_row(&sql, params![model_name], map_row)
        .optional()
        .context("get_by_name model_registry_v2")?)
}

pub fn list_for_service(conn: &Connection, service_id: i64) -> Result<Vec<ModelRow>> {
    let sql = format!(
        "SELECT {} FROM model_registry_v2 WHERE service_id = ?1 ORDER BY id ASC",
        COLS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![service_id], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_all(conn: &Connection) -> Result<Vec<ModelRow>> {
    let sql = format!("SELECT {} FROM model_registry_v2 ORDER BY id ASC", COLS);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn delete_for_service(conn: &Connection, service_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM model_registry_v2 WHERE service_id = ?1",
        params![service_id],
    )?;
    Ok(())
}
