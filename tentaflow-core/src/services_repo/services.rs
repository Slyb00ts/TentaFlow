// ============ File: services_repo/services.rs — CRUD over services_v2 ============

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::services::transport::Transport;

/// Method by which a service was deployed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployMethod {
    Docker,
    NativeEmbedded,
    NativeBinary,
    NativePythonBundle,
    External,
}

impl DeployMethod {
    pub fn as_db_tag(self) -> &'static str {
        match self {
            DeployMethod::Docker => "docker",
            DeployMethod::NativeEmbedded => "native_embedded",
            DeployMethod::NativeBinary => "native_binary",
            DeployMethod::NativePythonBundle => "native_python_bundle",
            DeployMethod::External => "external",
        }
    }
}

pub fn parse_deploy_method(tag: &str) -> Result<DeployMethod> {
    Ok(match tag {
        "docker" => DeployMethod::Docker,
        "native_embedded" => DeployMethod::NativeEmbedded,
        "native_binary" => DeployMethod::NativeBinary,
        "native_python_bundle" => DeployMethod::NativePythonBundle,
        "external" => DeployMethod::External,
        other => return Err(anyhow!("unknown deploy_method tag: {}", other)),
    })
}

/// Runtime status of a deployed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Starting,
    Running,
    Degraded,
    Failed,
    Stopped,
}

impl ServiceStatus {
    pub fn as_db_tag(self) -> &'static str {
        match self {
            ServiceStatus::Starting => "starting",
            ServiceStatus::Running => "running",
            ServiceStatus::Degraded => "degraded",
            ServiceStatus::Failed => "failed",
            ServiceStatus::Stopped => "stopped",
        }
    }
}

pub fn parse_status(tag: &str) -> Result<ServiceStatus> {
    Ok(match tag {
        "starting" => ServiceStatus::Starting,
        "running" => ServiceStatus::Running,
        "degraded" => ServiceStatus::Degraded,
        "failed" => ServiceStatus::Failed,
        "stopped" => ServiceStatus::Stopped,
        other => return Err(anyhow!("unknown service status tag: {}", other)),
    })
}

/// Input row for inserting a new service.
#[derive(Debug, Clone)]
pub struct NewService {
    pub engine_id: String,
    pub deploy_method: DeployMethod,
    pub transport: Transport,
    pub status: ServiceStatus,
    pub runtime_pid: Option<i64>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub endpoint_url: Option<String>,
    pub config_json: String,
}

impl NewService {
    pub fn minimal(
        engine_id: impl Into<String>,
        deploy_method: DeployMethod,
        transport: Transport,
    ) -> Self {
        Self {
            engine_id: engine_id.into(),
            deploy_method,
            transport,
            status: ServiceStatus::Starting,
            runtime_pid: None,
            runtime_port: None,
            sidecar_quic_port: None,
            endpoint_url: None,
            config_json: "{}".to_string(),
        }
    }
}

/// Row read from `services_v2`.
#[derive(Debug, Clone)]
pub struct ServiceRow {
    pub id: i64,
    pub engine_id: String,
    pub deploy_method: DeployMethod,
    pub transport: Transport,
    pub status: ServiceStatus,
    pub runtime_pid: Option<i64>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub endpoint_url: Option<String>,
    pub config_json: String,
    pub health_last_ok: Option<String>,
    pub health_last_err: Option<String>,
    pub restart_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceRow> {
    let deploy_method_tag: String = row.get("deploy_method")?;
    let transport_tag: String = row.get("transport")?;
    let status_tag: String = row.get("status")?;

    let deploy_method = parse_deploy_method(&deploy_method_tag).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    let transport = Transport::from_db_tag(&transport_tag).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;
    let status = parse_status(&status_tag).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })?;

    Ok(ServiceRow {
        id: row.get("id")?,
        engine_id: row.get("engine_id")?,
        deploy_method,
        transport,
        status,
        runtime_pid: row.get("runtime_pid")?,
        runtime_port: row.get::<_, Option<i64>>("runtime_port")?.map(|v| v as u16),
        sidecar_quic_port: row
            .get::<_, Option<i64>>("sidecar_quic_port")?
            .map(|v| v as u16),
        endpoint_url: row.get("endpoint_url")?,
        config_json: row.get("config_json")?,
        health_last_ok: row.get("health_last_ok")?,
        health_last_err: row.get("health_last_err")?,
        restart_count: row.get("restart_count")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const SELECT_COLUMNS: &str = "id, engine_id, deploy_method, transport, status, runtime_pid, \
    runtime_port, sidecar_quic_port, endpoint_url, config_json, health_last_ok, health_last_err, \
    restart_count, created_at, updated_at";

/// Inserts a new service row. Returns the assigned id.
pub fn insert(conn: &Connection, new: &NewService) -> Result<i64> {
    conn.execute(
        "INSERT INTO services_v2 (engine_id, deploy_method, transport, status, runtime_pid, \
            runtime_port, sidecar_quic_port, endpoint_url, config_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new.engine_id,
            new.deploy_method.as_db_tag(),
            new.transport.as_db_tag(),
            new.status.as_db_tag(),
            new.runtime_pid,
            new.runtime_port.map(|v| v as i64),
            new.sidecar_quic_port.map(|v| v as i64),
            new.endpoint_url,
            new.config_json,
        ],
    )
    .context("insert services_v2")?;
    Ok(conn.last_insert_rowid())
}

/// Inserts using an open transaction (for atomicity with related rows).
pub fn insert_in_tx(tx: &Transaction<'_>, new: &NewService) -> Result<i64> {
    tx.execute(
        "INSERT INTO services_v2 (engine_id, deploy_method, transport, status, runtime_pid, \
            runtime_port, sidecar_quic_port, endpoint_url, config_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new.engine_id,
            new.deploy_method.as_db_tag(),
            new.transport.as_db_tag(),
            new.status.as_db_tag(),
            new.runtime_pid,
            new.runtime_port.map(|v| v as i64),
            new.sidecar_quic_port.map(|v| v as i64),
            new.endpoint_url,
            new.config_json,
        ],
    )
    .context("insert services_v2 (tx)")?;
    Ok(tx.last_insert_rowid())
}

/// Fetches a service by id.
pub fn get(conn: &Connection, id: i64) -> Result<Option<ServiceRow>> {
    let sql = format!("SELECT {} FROM services_v2 WHERE id = ?1", SELECT_COLUMNS);
    Ok(conn
        .query_row(&sql, params![id], map_row)
        .optional()
        .context("get services_v2")?)
}

/// Lists services with status = 'running'.
pub fn list_alive(conn: &Connection) -> Result<Vec<ServiceRow>> {
    let sql = format!(
        "SELECT {} FROM services_v2 WHERE status = 'running' ORDER BY id ASC",
        SELECT_COLUMNS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Lists services that the supervisor must keep watch over: any non-terminal
/// state (running, degraded, starting). Terminal states (failed, stopped) are
/// excluded — they require an explicit user action to come back online.
pub fn list_supervised(conn: &Connection) -> Result<Vec<ServiceRow>> {
    let sql = format!(
        "SELECT {} FROM services_v2 WHERE status IN ('running','degraded','starting') \
         ORDER BY id ASC",
        SELECT_COLUMNS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Lists every service regardless of status (admin view).
pub fn list_all(conn: &Connection) -> Result<Vec<ServiceRow>> {
    let sql = format!("SELECT {} FROM services_v2 ORDER BY id ASC", SELECT_COLUMNS);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], map_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Updates the lifecycle status of a service. Bumps `updated_at`.
pub fn update_status(conn: &Connection, id: i64, status: ServiceStatus) -> Result<()> {
    let n = conn.execute(
        "UPDATE services_v2 SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        params![status.as_db_tag(), id],
    )?;
    if n == 0 {
        return Err(anyhow!("update_status: service id={} not found", id));
    }
    Ok(())
}

/// Records a successful or failed health probe.
pub fn update_health(conn: &Connection, id: i64, ok: bool, err: Option<&str>) -> Result<()> {
    let sql = if ok {
        "UPDATE services_v2 SET health_last_ok = CURRENT_TIMESTAMP, health_last_err = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?1"
    } else {
        "UPDATE services_v2 SET health_last_err = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1"
    };
    let n = if ok {
        conn.execute(sql, params![id])?
    } else {
        conn.execute(sql, params![id, err.unwrap_or("unknown")])?
    };
    if n == 0 {
        return Err(anyhow!("update_health: service id={} not found", id));
    }
    Ok(())
}

/// Increments `restart_count`. Used by supervisor.
pub fn increment_restart(conn: &Connection, id: i64) -> Result<i64> {
    conn.execute(
        "UPDATE services_v2 SET restart_count = restart_count + 1, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
        params![id],
    )?;
    let new_count: i64 = conn.query_row(
        "SELECT restart_count FROM services_v2 WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(new_count)
}

/// Updates runtime metadata (pid, ports, endpoint) once the engine is up.
pub fn update_runtime(
    conn: &Connection,
    id: i64,
    pid: Option<i64>,
    runtime_port: Option<u16>,
    sidecar_quic_port: Option<u16>,
    endpoint_url: Option<&str>,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE services_v2 SET runtime_pid = ?2, runtime_port = ?3, sidecar_quic_port = ?4, \
         endpoint_url = ?5, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
        params![
            id,
            pid,
            runtime_port.map(|v| v as i64),
            sidecar_quic_port.map(|v| v as i64),
            endpoint_url,
        ],
    )?;
    if n == 0 {
        return Err(anyhow!("update_runtime: service id={} not found", id));
    }
    Ok(())
}

/// Deletes a service. Cascades to `model_registry_v2` via FK ON DELETE CASCADE.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM services_v2 WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services_repo::models::{self, NewModel};

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrations::run(&conn).expect("run migrations");
        conn
    }

    fn sample_new(engine: &str) -> NewService {
        NewService::minimal(engine, DeployMethod::Docker, Transport::HttpDirect)
    }

    #[test]
    fn insert_and_get() {
        let conn = open_test_db();
        let id = insert(&conn, &sample_new("vllm")).unwrap();
        let row = get(&conn, id).unwrap().expect("row exists");
        assert_eq!(row.engine_id, "vllm");
        assert_eq!(row.deploy_method, DeployMethod::Docker);
        assert_eq!(row.transport, Transport::HttpDirect);
        assert_eq!(row.status, ServiceStatus::Starting);
        assert_eq!(row.restart_count, 0);
    }

    #[test]
    fn update_status_persists() {
        let conn = open_test_db();
        let id = insert(&conn, &sample_new("ollama")).unwrap();
        update_status(&conn, id, ServiceStatus::Running).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(row.status, ServiceStatus::Running);
    }

    #[test]
    fn update_health_persists() {
        let conn = open_test_db();
        let id = insert(&conn, &sample_new("whisper")).unwrap();
        update_health(&conn, id, true, None).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert!(row.health_last_ok.is_some());
        assert!(row.health_last_err.is_none());

        update_health(&conn, id, false, Some("connection refused")).unwrap();
        let row = get(&conn, id).unwrap().unwrap();
        assert_eq!(row.health_last_err.as_deref(), Some("connection refused"));
    }

    #[test]
    fn list_alive_returns_only_running() {
        let conn = open_test_db();
        let a = insert(&conn, &sample_new("a")).unwrap();
        let b = insert(&conn, &sample_new("b")).unwrap();
        let c = insert(&conn, &sample_new("c")).unwrap();
        update_status(&conn, a, ServiceStatus::Running).unwrap();
        update_status(&conn, b, ServiceStatus::Failed).unwrap();
        update_status(&conn, c, ServiceStatus::Running).unwrap();

        let alive = list_alive(&conn).unwrap();
        let ids: Vec<i64> = alive.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![a, c]);
    }

    #[test]
    fn delete_cascades_to_models() {
        let conn = open_test_db();
        let sid = insert(&conn, &sample_new("xtts")).unwrap();
        models::insert(
            &conn,
            &NewModel {
                service_id: sid,
                model_name: "xtts-v2".to_string(),
                display_name: Some("XTTS v2".to_string()),
                capabilities: r#"["tts"]"#.to_string(),
                context_length: None,
                quantization: None,
                is_default: true,
            },
        )
        .unwrap();
        models::insert(
            &conn,
            &NewModel {
                service_id: sid,
                model_name: "xtts-pl".to_string(),
                display_name: None,
                capabilities: r#"["tts"]"#.to_string(),
                context_length: None,
                quantization: None,
                is_default: false,
            },
        )
        .unwrap();

        assert_eq!(models::list_for_service(&conn, sid).unwrap().len(), 2);
        delete(&conn, sid).unwrap();
        assert!(models::list_for_service(&conn, sid).unwrap().is_empty());
    }

    #[test]
    fn unique_constraint_service_id_model_name() {
        let conn = open_test_db();
        let sid = insert(&conn, &sample_new("vllm")).unwrap();
        models::insert(
            &conn,
            &NewModel {
                service_id: sid,
                model_name: "qwen-7b".to_string(),
                display_name: None,
                capabilities: "[]".to_string(),
                context_length: Some(8192),
                quantization: None,
                is_default: true,
            },
        )
        .unwrap();
        let err = models::insert(
            &conn,
            &NewModel {
                service_id: sid,
                model_name: "qwen-7b".to_string(),
                display_name: None,
                capabilities: "[]".to_string(),
                context_length: Some(8192),
                quantization: None,
                is_default: false,
            },
        );
        assert!(err.is_err(), "duplicate (service_id, model_name) must fail");
    }
}
