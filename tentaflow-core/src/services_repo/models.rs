// ============ File: services_repo/models.rs — CRUD over model_registry ============

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
        "INSERT INTO model_registry (service_id, model_name, display_name, capabilities, \
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
    .context("insert model_registry")?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_in_tx(tx: &Transaction<'_>, new: &NewModel) -> Result<i64> {
    tx.execute(
        "INSERT INTO model_registry (service_id, model_name, display_name, capabilities, \
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
    .context("insert model_registry (tx)")?;
    Ok(tx.last_insert_rowid())
}

pub fn get_by_name(conn: &Connection, model_name: &str) -> Result<Option<ModelRow>> {
    let sql = format!(
        "SELECT {} FROM model_registry WHERE model_name = ?1 LIMIT 1",
        COLS
    );
    Ok(conn
        .query_row(&sql, params![model_name], map_row)
        .optional()
        .context("get_by_name model_registry")?)
}

pub fn list_for_service(conn: &Connection, service_id: i64) -> Result<Vec<ModelRow>> {
    let sql = format!(
        "SELECT {} FROM model_registry WHERE service_id = ?1 ORDER BY id ASC",
        COLS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![service_id], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_all(conn: &Connection) -> Result<Vec<ModelRow>> {
    let sql = format!("SELECT {} FROM model_registry ORDER BY id ASC", COLS);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn delete_for_service(conn: &Connection, service_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM model_registry WHERE service_id = ?1",
        params![service_id],
    )?;
    Ok(())
}

/// Aggregate row joining `model_registry` with the parent `services`.
/// Used by the dashboard `GET /api/models` to surface which engine each
/// model is served by + the runtime transport / status.
#[derive(Debug, Clone)]
pub struct ModelWithService {
    pub id: i64,
    pub service_id: i64,
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: String,
    pub context_length: Option<i64>,
    pub quantization: Option<String>,
    pub is_default: bool,
    pub engine_id: String,
    pub status: String,
    pub transport: String,
    pub deploy_method: String,
    pub endpoint_url: Option<String>,
}

/// Lists all models attached to services in `running` or `degraded` state.
/// Models on `starting`/`failed`/`stopped` services are filtered so callers
/// only see usable engines.
pub fn list_alive(conn: &Connection) -> Result<Vec<ModelWithService>> {
    let sql = "SELECT m.id, m.service_id, m.model_name, m.display_name, m.capabilities, \
        m.context_length, m.quantization, m.is_default, \
        s.engine_id, s.status, s.transport, s.deploy_method, s.endpoint_url \
        FROM model_registry m \
        INNER JOIN services s ON s.id = m.service_id \
        WHERE s.status IN ('running','degraded') \
        ORDER BY m.id ASC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ModelWithService {
                id: row.get(0)?,
                service_id: row.get(1)?,
                model_name: row.get(2)?,
                display_name: row.get(3)?,
                capabilities: row.get(4)?,
                context_length: row.get(5)?,
                quantization: row.get(6)?,
                is_default: row.get::<_, i64>(7)? != 0,
                engine_id: row.get(8)?,
                status: row.get(9)?,
                transport: row.get(10)?,
                deploy_method: row.get(11)?,
                endpoint_url: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
