// =============================================================================
// Plik: api/dashboard/api_models.rs
// Opis: CRUD rejestru modeli AI oraz aliasow modeli.
// =============================================================================

use std::sync::Arc;

use crate::db::{self, DbPool};
use crate::db::models::{NewModelEntry, UpdateModelEntry};
use crate::mesh::quic_mesh::QuicMeshManager;
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

#[derive(Deserialize)]
pub struct CreateModelAliasRequest {
    pub alias: String,
    pub target_model: String,
    pub fallback_targets: Option<String>,
    pub strategy: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateModelAliasRequest {
    pub alias: String,
    pub target_model: String,
    pub is_active: Option<bool>,
    pub fallback_targets: Option<String>,
    pub strategy: Option<String>,
}

/// GET /api/models - lista wpisow rejestru modeli z paginacja
pub fn handle_list_entries(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_model_entries(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// GET /api/models/:id - szczegoly wpisu modelu
pub fn handle_get_entry(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    match db::repository::get_model_entry(pool, id)? {
        Some(item) => Ok((200, serde_json::to_string(&item)?)),
        None => Ok((404, format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id))),
    }
}

/// POST /api/models - utworz wpis modelu
pub fn handle_create_entry(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateModelEntryRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.model_name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'model_name' nie moze byc puste"}"#.to_string()));
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
        return Ok((404, format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id)));
    }

    let req: UpdateModelEntryRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

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
        return Ok((404, format!(r#"{{"error":"Model o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_model_entry(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// GET /api/model-aliases - lista aliasow modeli
pub fn handle_list_aliases(pool: &DbPool) -> Result<(u16, String)> {
    let items = db::repository::list_model_aliases(pool)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/model-aliases - utworz alias modelu
pub fn handle_create_alias(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateModelAliasRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.alias.trim().is_empty() || req.target_model.trim().is_empty() {
        return Ok((400, r#"{"error":"Alias i model docelowy nie moga byc puste"}"#.to_string()));
    }

    let id = db::repository::create_model_alias(pool, &req.alias, &req.target_model, req.fallback_targets.as_deref(), req.strategy.as_deref())?;
    let item = db::repository::get_model_alias(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/model-aliases/:id - aktualizuj alias modelu
pub fn handle_update_alias(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_model_alias(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Alias modelu o id {} nie istnieje"}}"#, id)));
    }

    let req: UpdateModelAliasRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.alias.trim().is_empty() || req.target_model.trim().is_empty() {
        return Ok((400, r#"{"error":"Alias i model docelowy nie moga byc puste"}"#.to_string()));
    }

    db::repository::update_model_alias(pool, id, &req.alias, &req.target_model, req.is_active.unwrap_or(true), req.fallback_targets.as_deref(), req.strategy.as_deref())?;
    let item = db::repository::get_model_alias(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/model-aliases/:id - usun alias modelu
pub fn handle_delete_alias(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_model_alias(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Alias modelu o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_model_alias(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// GET /api/models/unified — unikalne modele ze wszystkich nodow mesh
pub fn handle_unified_models(quic_mesh: &Option<Arc<QuicMeshManager>>) -> Result<(u16, String)> {
    match quic_mesh {
        Some(ref qm) => {
            let models = qm.service_registry().unique_models();
            Ok((200, serde_json::to_string(&models)?))
        }
        None => Ok((200, "[]".to_string())),
    }
}
