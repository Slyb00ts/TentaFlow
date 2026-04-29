// ============ File: api/dashboard/api_services.rs — REST view over services + model_registry ============
//
// Thin GUI surface for the unified services pipeline. The REST shape is
// intentionally minimal: a flat list of supervised services plus their
// attached model rows. Heavier mutations (deploy, delete) flow through the
// binary RPC handlers; this module is read + DELETE only.

use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use crate::db::DbPool;
use crate::services::deploy as deploy_pipeline;
use crate::services::ports::PortAllocator;
use crate::services_repo::services::{DeployMethod, ServiceStatus};
use crate::services_repo::{models as models_repo, services as services_repo};

#[derive(Serialize)]
pub struct ServiceModelItem {
    pub model_name: String,
    pub display_name: Option<String>,
    pub capabilities: serde_json::Value,
}

#[derive(Serialize)]
pub struct ServiceListItem {
    pub id: i64,
    pub engine_id: String,
    pub category: String,
    pub display_name: String,
    pub deploy_method: &'static str,
    pub transport: &'static str,
    pub status: &'static str,
    pub pinned: bool,
    pub paused: bool,
    pub endpoint_url: Option<String>,
    pub runtime_pid: Option<i64>,
    pub runtime_port: Option<u16>,
    pub sidecar_quic_port: Option<u16>,
    pub restart_count: i64,
    pub models: Vec<ServiceModelItem>,
}

fn model_to_item(row: crate::services_repo::models::ModelRow) -> ServiceModelItem {
    let caps = serde_json::from_str::<serde_json::Value>(&row.capabilities)
        .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    ServiceModelItem {
        model_name: row.model_name,
        display_name: row.display_name,
        capabilities: caps,
    }
}

fn build_item(
    db: &DbPool,
    svc: crate::services_repo::services::ServiceRow,
) -> Result<ServiceListItem> {
    let conn = db
        .lock()
        .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
    let models = models_repo::list_for_service(&conn, svc.id)?
        .into_iter()
        .map(model_to_item)
        .collect();
    Ok(ServiceListItem {
        id: svc.id,
        engine_id: svc.engine_id,
        category: svc.category,
        display_name: svc.display_name,
        deploy_method: svc.deploy_method.as_db_tag(),
        transport: svc.transport.as_db_tag(),
        status: svc.status.as_db_tag(),
        pinned: svc.pinned,
        paused: svc.paused,
        endpoint_url: svc.endpoint_url,
        runtime_pid: svc.runtime_pid,
        runtime_port: svc.runtime_port,
        sidecar_quic_port: svc.sidecar_quic_port,
        restart_count: svc.restart_count,
        models,
    })
}

/// GET /api/services — supervised services (running / starting / degraded).
pub fn handle_list(db: &DbPool) -> Result<(u16, String)> {
    let conn = db
        .lock()
        .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
    let services = services_repo::list_supervised(&conn)?;
    drop(conn);

    let mut out: Vec<ServiceListItem> = Vec::with_capacity(services.len());
    for svc in services {
        out.push(build_item(db, svc)?);
    }
    Ok((200, serde_json::to_string(&out)?))
}

/// DELETE /api/services/:id — stops the runtime and removes the row, cascading
/// to `model_registry` via FK ON DELETE CASCADE.
pub async fn handle_delete(
    db: &DbPool,
    ports: Option<Arc<PortAllocator>>,
    id: i64,
) -> Result<(u16, String)> {
    let svc = {
        let conn = db
            .lock()
            .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
        services_repo::get(&conn, id)?
    };
    let Some(svc) = svc else {
        return Ok((404, format!(r#"{{"error":"Service id {} not found"}}"#, id)));
    };

    if let Some(ports) = ports {
        if let Err(e) = deploy_pipeline::stop(&svc, ports).await {
            tracing::warn!("services::deploy::stop({}): {}", id, e);
        }
    }

    let conn = db
        .lock()
        .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
    services_repo::delete(&conn, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

#[allow(dead_code)] // referenced by tests / future binary RPC.
pub fn status_label(status: ServiceStatus) -> &'static str {
    status.as_db_tag()
}

#[allow(dead_code)]
pub fn method_label(method: DeployMethod) -> &'static str {
    method.as_db_tag()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::transport::Transport;
    use crate::services_repo::models::NewModel;
    use crate::services_repo::services::{DeployMethod, NewService, ServiceStatus};

    fn open_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(std::sync::Mutex::new(conn))
    }

    fn seed_running_service(db: &DbPool, engine: &str) -> i64 {
        let conn = db.lock().unwrap();
        let mut new = NewService::minimal(engine, DeployMethod::Docker, Transport::HttpDirect);
        new.status = ServiceStatus::Running;
        let id = services_repo::insert(&conn, &new).unwrap();
        services_repo::update_status(&conn, id, ServiceStatus::Running).unwrap();
        models_repo::insert(
            &conn,
            &NewModel {
                service_id: id,
                model_name: format!("{}-default", engine),
                display_name: Some("Default".into()),
                capabilities: r#"["chat"]"#.into(),
                context_length: Some(4096),
                quantization: None,
                is_default: true,
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn handle_list_returns_alive_with_models() {
        let db = open_db();
        let _id = seed_running_service(&db, "vllm");

        let (status, body) = handle_list(&db).unwrap();
        assert_eq!(status, 200);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["engine_id"], "vllm");
        assert_eq!(parsed[0]["status"], "running");
        assert_eq!(parsed[0]["models"].as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["models"][0]["model_name"], "vllm-default");
    }

    #[tokio::test]
    async fn handle_delete_removes_service_and_models() {
        let db = open_db();
        let id = seed_running_service(&db, "vllm");

        let (status, _) = handle_delete(&db, None, id).await.unwrap();
        assert_eq!(status, 200);

        let conn = db.lock().unwrap();
        let row = services_repo::get(&conn, id).unwrap();
        assert!(row.is_none(), "service row was deleted");
        let models = models_repo::list_for_service(&conn, id).unwrap();
        assert!(models.is_empty(), "FK CASCADE removed model rows");
    }

    #[tokio::test]
    async fn handle_delete_404_for_unknown_id() {
        let db = open_db();
        let (status, _) = handle_delete(&db, None, 9999).await.unwrap();
        assert_eq!(status, 404);
    }
}
