// =============================================================================
// Plik: api/dashboard/api_services.rs
// Opis: CRUD serwisow AI - lista, tworzenie, edycja, usuwanie, statystyki.
// =============================================================================

use crate::db::{self, DbPool};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CreateServiceRequest {
    pub name: String,
    pub service_type: String,
    pub strategy: String,
    pub model_category: Option<String>,
    pub config_json: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateServiceRequest {
    pub name: String,
    pub service_type: String,
    pub strategy: String,
    pub model_category: Option<String>,
    pub status: String,
    pub config_json: Option<String>,
}

#[derive(Serialize)]
pub struct ServiceWithBackends {
    #[serde(flatten)]
    pub service: db::models::DbService,
    pub backends: Vec<db::models::DbServiceBackend>,
}

#[derive(Serialize)]
pub struct ServiceStats {
    pub service_id: i64,
    pub total_requests: u64,
    pub avg_latency_ms: f64,
    pub error_rate: f64,
}

/// GET /api/services - lista serwisow z backendami (jeden JOIN zamiast N+1)
pub fn handle_list(pool: &DbPool) -> Result<(u16, String)> {
    let pairs = db::repository::list_services_with_backends(pool)?;

    let result: Vec<ServiceWithBackends> = pairs
        .into_iter()
        .map(|(service, backends)| ServiceWithBackends { service, backends })
        .collect();

    Ok((200, serde_json::to_string(&result)?))
}

/// POST /api/services - utworz nowy serwis
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateServiceRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let config = req.config_json.as_deref().unwrap_or("{}");

    let id = db::repository::create_service(
        pool,
        &req.name,
        &req.service_type,
        &req.strategy,
        req.model_category.as_deref(),
        config,
    )?;

    let service = db::repository::get_service(pool, id)?;
    Ok((201, serde_json::to_string(&service)?))
}

/// PUT /api/services/:id - aktualizuj serwis
pub fn handle_update(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_service(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Serwis o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateServiceRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let config = req.config_json.as_deref().unwrap_or("{}");

    db::repository::update_service(
        pool,
        id,
        &req.name,
        &req.service_type,
        &req.strategy,
        req.model_category.as_deref(),
        &req.status,
        config,
    )?;

    let service = db::repository::get_service(pool, id)?;
    Ok((200, serde_json::to_string(&service)?))
}

/// DELETE /api/services/:id - usun serwis
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_service(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Serwis o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_service(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// GET /api/services/:id/stats - statystyki serwisu (placeholder)
pub fn handle_stats(_pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let stats = ServiceStats {
        service_id: id,
        total_requests: 0,
        avg_latency_ms: 0.0,
        error_rate: 0.0,
    };

    Ok((200, serde_json::to_string(&stats)?))
}

// --- Backendy serwisow ---

#[derive(Deserialize)]
pub struct CreateBackendRequest {
    pub service_id: i64,
    pub connection_type: String,
    pub config_json: Option<String>,
    pub max_concurrent: Option<i64>,
    pub timeout_ms: Option<i64>,
    pub weight: Option<i64>,
    pub model_name_override: Option<String>,
    pub health_check_path: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateBackendRequest {
    pub connection_type: String,
    pub config_json: Option<String>,
    pub max_concurrent: Option<i64>,
    pub timeout_ms: Option<i64>,
    pub weight: Option<i64>,
    pub model_name_override: Option<String>,
    pub health_check_path: Option<String>,
    pub is_active: Option<bool>,
}

/// GET /api/services/:id/backends - lista backendow serwisu
pub fn handle_list_backends(pool: &DbPool, service_id: i64) -> Result<(u16, String)> {
    let backends = db::repository::list_backends_for_service(pool, service_id)?;
    Ok((200, serde_json::to_string(&backends)?))
}

/// POST /api/services/:id/backends - utworz nowy backend
pub fn handle_create_backend(pool: &DbPool, service_id: i64, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateBackendRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let config = req.config_json.as_deref().unwrap_or("{}");
    let backend = db::models::NewBackend {
        service_id,
        connection_type: &req.connection_type,
        config_json: config,
        max_concurrent: req.max_concurrent.unwrap_or(50),
        timeout_ms: req.timeout_ms.unwrap_or(120000),
        weight: req.weight.unwrap_or(1),
        model_name_override: req.model_name_override.as_deref(),
        health_check_path: req.health_check_path.as_deref(),
    };

    let id = db::repository::create_backend(pool, &backend)?;
    let item = db::repository::get_backend(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/backends/:id - aktualizuj backend
pub fn handle_update_backend(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    if db::repository::get_backend(pool, id)?.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Backend o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateBackendRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let config = req.config_json.as_deref().unwrap_or("{}");
    db::repository::update_backend(
        pool,
        id,
        &req.connection_type,
        config,
        req.max_concurrent.unwrap_or(50),
        req.timeout_ms.unwrap_or(120000),
        req.weight.unwrap_or(1),
        req.model_name_override.as_deref(),
        req.health_check_path.as_deref(),
        req.is_active.unwrap_or(true),
    )?;

    let item = db::repository::get_backend(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/backends/:id - usun backend
pub fn handle_delete_backend(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    if db::repository::get_backend(pool, id)?.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Backend o id {} nie istnieje"}}"#, id),
        ));
    }
    db::repository::delete_backend(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
