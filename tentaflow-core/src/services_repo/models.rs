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

pub fn count_for_service(conn: &Connection, service_id: i64) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM model_registry WHERE service_id = ?1",
        params![service_id],
        |row| row.get(0),
    )
    .context("count model_registry for service")
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

pub fn delete_for_service_in_tx(tx: &Transaction<'_>, service_id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM model_registry WHERE service_id = ?1",
        params![service_id],
    )
    .context("delete_for_service model_registry (tx)")?;
    Ok(())
}

/// One model the admin chose to expose for an external provider service.
/// `modality` (chat/embedding/tts/stt/...) becomes the single capability tag.
#[derive(Debug, Clone)]
pub struct SelectedModel {
    pub model_name: String,
    pub display_name: Option<String>,
    pub modality: String,
    pub context_length: Option<i64>,
}

/// Reconcile `model_registry` for `service_id` to EXACTLY the given selection:
/// deselected models are removed, newly-selected ones inserted, and rows that
/// stay selected are left untouched (preserving their `is_default`). When the
/// resulting set has no default, the first row becomes the default so the
/// service still resolves a model. Used by the external-provider model picker.
pub fn replace_selection(
    conn: &Connection,
    service_id: i64,
    selected: &[SelectedModel],
) -> Result<()> {
    use std::collections::HashSet;

    let existing = list_for_service(conn, service_id)?;
    let existing_names: HashSet<&str> = existing.iter().map(|m| m.model_name.as_str()).collect();
    let selected_names: HashSet<&str> = selected.iter().map(|m| m.model_name.as_str()).collect();

    // Remove rows no longer selected.
    for row in &existing {
        if !selected_names.contains(row.model_name.as_str()) {
            conn.execute("DELETE FROM model_registry WHERE id = ?1", params![row.id])
                .context("delete deselected model_registry row")?;
        }
    }

    // Insert newly-selected rows (UNIQUE(service_id, model_name) protects against
    // races; ignore the conflict if a concurrent insert won).
    for m in selected {
        if existing_names.contains(m.model_name.as_str()) {
            continue;
        }
        let capabilities = format!("[\"{}\"]", m.modality);
        conn.execute(
            "INSERT OR IGNORE INTO model_registry (service_id, model_name, display_name, \
                capabilities, context_length, quantization, is_default) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0)",
            params![
                service_id,
                m.model_name,
                m.display_name,
                capabilities,
                m.context_length,
            ],
        )
        .context("insert selected model_registry row")?;
    }

    // Guarantee a default so the service can resolve a model.
    let has_default: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM model_registry WHERE service_id = ?1 AND is_default = 1)",
            params![service_id],
            |row| row.get::<_, i64>(0),
        )
        .context("check default model")?
        != 0;
    if !has_default {
        conn.execute(
            "UPDATE model_registry SET is_default = 1 \
             WHERE id = (SELECT id FROM model_registry WHERE service_id = ?1 ORDER BY id ASC LIMIT 1)",
            params![service_id],
        )
        .context("set default model")?;
    }
    Ok(())
}

/// Reconcile models discovered from a managed runtime with the registry rows
/// owned by that service. Metadata and the default marker are authoritative;
/// rows no longer advertised by the runtime are removed.
pub fn replace_discovered(
    conn: &Connection,
    service_id: i64,
    discovered: &[NewModel],
) -> Result<()> {
    use std::collections::HashSet;

    let names: HashSet<&str> = discovered
        .iter()
        .map(|model| model.model_name.as_str())
        .collect();
    for row in list_for_service(conn, service_id)? {
        if !names.contains(row.model_name.as_str()) {
            conn.execute("DELETE FROM model_registry WHERE id = ?1", params![row.id])
                .context("delete stale discovered model")?;
        }
    }
    for model in discovered {
        conn.execute(
            "INSERT INTO model_registry (service_id, model_name, display_name, capabilities, \
                context_length, quantization, is_default) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(service_id, model_name) DO UPDATE SET \
                display_name = excluded.display_name, capabilities = excluded.capabilities, \
                context_length = excluded.context_length, quantization = excluded.quantization, \
                is_default = excluded.is_default",
            params![
                service_id,
                model.model_name,
                model.display_name,
                model.capabilities,
                model.context_length,
                model.quantization,
                model.is_default as i64,
            ],
        )
        .context("upsert discovered model")?;
    }
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

/// The model an agent falls back to when its definition names none.
///
/// „Default" means, in order: a model a service explicitly marked default, then
/// the oldest model of any service that can actually serve. A model whose
/// service is down would only move the failure one step later, so the join is
/// part of the answer rather than a filter applied by the caller.
///
/// Returns `None` on a node with no usable model at all — the caller turns that
/// into an actionable message instead of a bare "no model".
pub fn default_llm_model(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT m.model_name FROM model_registry m \
         JOIN services s ON s.id = m.service_id \
         WHERE s.status IN ('running', 'degraded') \
         ORDER BY m.is_default DESC, m.id ASC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
}
