// =============================================================================
// Plik: api/dashboard/api_models.rs
// Opis: CRUD rejestru modeli AI oraz aliasow modeli. REST handlery dla
//       `/api/models` i `/api/models/:id` (wciaz REST). Aliasy + unified
//       zmigrowane do binary protocol w FAZA 2 — domain logic ponizej
//       (`collect_unified`, `create_alias`, `update_alias`, `delete_alias`,
//       `list_aliases`) jest wspoldzielona przez handlery binarne.
// =============================================================================

use std::sync::Arc;

use crate::db::models::{DbModelAlias, NewModelEntry, UpdateModelEntry};
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::service_registry::UnifiedModelInfo;
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateModelEntryRequest {
    pub model_name: String,
    pub display_name: Option<String>,
    pub service_type: String,
    pub connection_type: String,
    pub service_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub is_public: Option<bool>,
    pub config_json: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateModelEntryRequest {
    pub display_name: Option<String>,
    pub service_type: String,
    pub connection_type: String,
    pub service_id: Option<i64>,
    pub flow_id: Option<i64>,
    pub is_public: Option<bool>,
    pub is_active: Option<bool>,
    pub config_json: Option<String>,
}

/// GET /api/models - alive models served by services (running or degraded).
/// Phase 5 flip: legacy `model_registry` is no longer queried here. Pagination
/// kept for compatibility — applied client-side to the JOIN result.
pub fn handle_list_entries(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Item {
        id: i64,
        service_id: i64,
        model_name: String,
        display_name: Option<String>,
        capabilities: serde_json::Value,
        context_length: Option<i64>,
        quantization: Option<String>,
        is_default: bool,
        engine_id: String,
        status: String,
        transport: String,
        deploy_method: String,
        endpoint_url: Option<String>,
    }

    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("pool lock poisoned: {}", e))?;
    let rows = crate::services_repo::models::list_alive(&conn)?;
    drop(conn);

    let start = offset.max(0) as usize;
    let take = if limit <= 0 {
        rows.len()
    } else {
        limit as usize
    };
    let items: Vec<Item> = rows
        .into_iter()
        .skip(start)
        .take(take)
        .map(|m| {
            let caps = serde_json::from_str::<serde_json::Value>(&m.capabilities)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
            Item {
                id: m.id,
                service_id: m.service_id,
                model_name: m.model_name,
                display_name: m.display_name,
                capabilities: caps,
                context_length: m.context_length,
                quantization: m.quantization,
                is_default: m.is_default,
                engine_id: m.engine_id,
                status: m.status,
                transport: m.transport,
                deploy_method: m.deploy_method,
                endpoint_url: m.endpoint_url,
            }
        })
        .collect();
    Ok((200, serde_json::to_string(&items)?))
}

/// GET /api/models/:id - szczegoly wpisu modelu
pub fn handle_get_entry(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    match db::repository::get_model_entry(pool, id)? {
        Some(item) => Ok((200, serde_json::to_string(&item)?)),
        None => Ok((
            404,
            format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id),
        )),
    }
}

/// POST /api/models - utworz wpis modelu
pub fn handle_create_entry(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateModelEntryRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.model_name.trim().is_empty() {
        return Ok((
            400,
            r#"{"error":"Pole 'model_name' nie może być puste"}"#.to_string(),
        ));
    }

    let config = req.config_json.as_deref().unwrap_or("{}");

    let params = NewModelEntry {
        model_name: &req.model_name,
        display_name: req.display_name.as_deref(),
        service_type: &req.service_type,
        connection_type: &req.connection_type,
        service_id: req.service_id,
        flow_id: req.flow_id,
        is_public: req.is_public.unwrap_or(false),
        config_json: config,
    };

    let id = db::repository::create_model_entry(pool, &params)?;
    let item = db::repository::get_model_entry(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/models/:id - aktualizuj wpis modelu
pub fn handle_update_entry(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_model_entry(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateModelEntryRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let config = req.config_json.as_deref().unwrap_or("{}");

    let params = UpdateModelEntry {
        id,
        display_name: req.display_name.as_deref(),
        service_type: &req.service_type,
        connection_type: &req.connection_type,
        service_id: req.service_id,
        flow_id: req.flow_id,
        is_public: req.is_public.unwrap_or(false),
        is_active: req.is_active.unwrap_or(true),
        config_json: config,
    };

    db::repository::update_model_entry(pool, &params)?;
    let item = db::repository::get_model_entry(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/models/:id - usun wpis modelu
pub fn handle_delete_entry(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_model_entry(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_model_entry(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// =============================================================================
// Domain logic aliasow (FAZA 2: uzywane przez binary handlery w
// `dispatch::handlers`). Zwraca DbModelAlias zamiast JSON — caller formatuje.
// =============================================================================

/// Lista wszystkich aliasow z DB.
pub fn list_aliases(pool: &DbPool) -> Result<Vec<DbModelAlias>> {
    db::repository::list_model_aliases(pool)
}

/// Utworz alias. Walidacja: alias i target_model musza byc nie-puste.
pub fn create_alias(
    pool: &DbPool,
    alias: &str,
    target_model: &str,
    strategy: Option<&str>,
    fallback_targets: Option<&str>,
) -> Result<i64> {
    if alias.trim().is_empty() || target_model.trim().is_empty() {
        anyhow::bail!("Alias i model docelowy nie moga byc puste");
    }
    db::repository::create_model_alias(pool, alias, target_model, fallback_targets, strategy)
}

/// Aktualizacja aliasu po id. Zwraca `Ok(false)` gdy rekord nie istnieje.
pub fn update_alias(
    pool: &DbPool,
    id: i64,
    alias: &str,
    target_model: &str,
    is_active: bool,
    strategy: Option<&str>,
    fallback_targets: Option<&str>,
) -> Result<bool> {
    if db::repository::get_model_alias(pool, id)?.is_none() {
        return Ok(false);
    }
    if alias.trim().is_empty() || target_model.trim().is_empty() {
        anyhow::bail!("Alias i model docelowy nie moga byc puste");
    }
    db::repository::update_model_alias(
        pool,
        id,
        alias,
        target_model,
        is_active,
        fallback_targets,
        strategy,
    )?;
    Ok(true)
}

/// Usuniecie aliasu po id. Zwraca `Ok(false)` gdy rekord nie istnieje.
pub fn delete_alias(pool: &DbPool, id: i64) -> Result<bool> {
    if db::repository::get_model_alias(pool, id)?.is_none() {
        return Ok(false);
    }
    db::repository::delete_model_alias(pool, id)?;
    Ok(true)
}

/// Unikalne modele z mesh service registry. Pusty `Vec` gdy mesh niedostepny.
pub fn collect_unified(quic_mesh: &Option<Arc<IrohMeshManager>>) -> Vec<UnifiedModelInfo> {
    match quic_mesh {
        Some(qm) => qm.service_registry().unique_models(),
        None => Vec::new(),
    }
}

/// Synchronizacja router cache + broadcast alias sync do meshu po mutacji.
/// Wolane z handlerow binarnych po udanym create/update/delete.
pub fn broadcast_alias_mutation(
    pool: &DbPool,
    router: &Arc<crate::routing::router::Router>,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) {
    router.reload_alias_cache();
    if let Some(qm) = quic_mesh {
        if let Ok(aliases) = db::repository::list_model_aliases(pool) {
            if let Ok(json) = serde_json::to_vec(&aliases) {
                let qm = Arc::clone(qm);
                tokio::spawn(async move {
                    qm.broadcast_alias_sync(json).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests_endpoint {
    use super::*;
    use crate::services::transport::Transport;
    use crate::services_repo::models::{insert as model_insert, NewModel};
    use crate::services_repo::services::{
        insert as service_insert, update_status, DeployMethod, NewService, ServiceStatus,
    };

    fn open_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        std::sync::Arc::new(std::sync::Mutex::new(conn))
    }

    fn seed(db: &DbPool, engine: &str, status: ServiceStatus) -> i64 {
        let conn = db.lock().unwrap();
        let mut new = NewService::minimal(engine, DeployMethod::Docker, Transport::HttpDirect);
        new.status = status;
        let id = service_insert(&conn, &new).unwrap();
        update_status(&conn, id, status).unwrap();
        model_insert(
            &conn,
            &NewModel {
                service_id: id,
                model_name: format!("{}-m", engine),
                display_name: None,
                capabilities: r#"["chat"]"#.into(),
                context_length: None,
                quantization: None,
                is_default: true,
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn endpoint_returns_only_alive() {
        let db = open_db();
        seed(&db, "running-engine", ServiceStatus::Running);
        seed(&db, "starting-engine", ServiceStatus::Starting);
        seed(&db, "failed-engine", ServiceStatus::Failed);
        seed(&db, "degraded-engine", ServiceStatus::Degraded);

        let (status, body) = handle_list_entries(&db, 0, 100).unwrap();
        assert_eq!(status, 200);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        let engines: Vec<&str> = parsed
            .iter()
            .map(|v| v["engine_id"].as_str().unwrap())
            .collect();
        assert!(engines.contains(&"running-engine"));
        assert!(engines.contains(&"degraded-engine"));
        assert!(!engines.contains(&"starting-engine"));
        assert!(!engines.contains(&"failed-engine"));
    }
}
