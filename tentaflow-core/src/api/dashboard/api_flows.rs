// =============================================================================
// Plik: api/dashboard/api_flows.rs
// Opis: CRUD flow, flow_model_bindings, flow_node_templates i flow_executions.
// =============================================================================

use crate::db::models::FlowNodeTemplateParams;
use crate::db::{self, DbPool};
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateFlowBindingRequest {
    pub flow_id: i64,
    pub model_pattern: String,
    pub priority: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateFlowBindingRequest {
    pub flow_id: i64,
    pub model_pattern: String,
    pub priority: Option<i64>,
}

#[derive(Deserialize)]
pub struct CreateNodeTemplateRequest {
    pub node_type: String,
    pub category: String,
    pub label: String,
    pub description: Option<String>,
    pub default_config: Option<String>,
    pub icon: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateNodeTemplateRequest {
    pub node_type: String,
    pub category: String,
    pub label: String,
    pub description: Option<String>,
    pub default_config: Option<String>,
    pub icon: Option<String>,
}

// Flows CRUD + Versions — zmigrowane do binary WS (FAZA 3). Domain logic
// (parsowanie + walidacja + optimistic locking + snapshot wersji) zyje w
// `dispatch/handlers.rs` i `db/repository.rs`. REST wrappery usuniete.

// --- Flow Model Bindings ---

/// GET /api/flow-bindings - lista powiazan flow-model
pub fn handle_list_bindings(pool: &DbPool) -> Result<(u16, String)> {
    let items = db::repository::list_flow_model_bindings(pool)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/flow-bindings - utworz powiazanie
pub fn handle_create_binding(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateFlowBindingRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let id = db::repository::create_flow_model_binding(
        pool,
        req.flow_id,
        &req.model_pattern,
        req.priority.unwrap_or(0),
    )?;
    let item = db::repository::get_flow_model_binding(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/flow-bindings/:id - aktualizuj powiazanie
pub fn handle_update_binding(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_model_binding(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Powiazanie o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateFlowBindingRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    db::repository::update_flow_model_binding(
        pool,
        id,
        req.flow_id,
        &req.model_pattern,
        req.priority.unwrap_or(0),
    )?;
    let item = db::repository::get_flow_model_binding(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/flow-bindings/:id - usun powiazanie
pub fn handle_delete_binding(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_model_binding(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Powiazanie o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_flow_model_binding(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// --- Flow Node Templates ---
// GET /api/flow-node-templates → zmigrowany do binary (FlowNodeTemplatesListRequest).
// POST/PUT/DELETE zostaja dla admin CRUD bez klienta frontowego.

/// POST /api/flow-node-templates - utworz szablon wezla
pub fn handle_create_node_template(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateNodeTemplateRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let params = FlowNodeTemplateParams {
        node_type: &req.node_type,
        category: &req.category,
        label: &req.label,
        description: req.description.as_deref(),
        default_config: req.default_config.as_deref().unwrap_or("{}"),
        icon: req.icon.as_deref(),
    };

    let id = db::repository::create_flow_node_template(pool, &params)?;
    let item = db::repository::get_flow_node_template(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/flow-node-templates/:id - aktualizuj szablon wezla
pub fn handle_update_node_template(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_node_template(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Szablon wezla o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateNodeTemplateRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let params = FlowNodeTemplateParams {
        node_type: &req.node_type,
        category: &req.category,
        label: &req.label,
        description: req.description.as_deref(),
        default_config: req.default_config.as_deref().unwrap_or("{}"),
        icon: req.icon.as_deref(),
    };

    db::repository::update_flow_node_template(pool, id, &params)?;
    let item = db::repository::get_flow_node_template(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/flow-node-templates/:id - usun szablon wezla
pub fn handle_delete_node_template(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_node_template(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Szablon wezla o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_flow_node_template(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// --- Flow Executions ---

/// GET /api/flow-executions - lista wykonan flow z paginacja
pub fn handle_list_executions(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_flow_executions(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// GET /api/flow-executions/:id - szczegoly wykonania flow
pub fn handle_get_execution(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    match db::repository::get_flow_execution(pool, id)? {
        Some(item) => Ok((200, serde_json::to_string(&item)?)),
        None => Ok((
            404,
            format!(r#"{{"error":"Wykonanie flow o id {} nie istnieje"}}"#, id),
        )),
    }
}

/// DELETE /api/flow-executions/:id - usun wykonanie flow
pub fn handle_delete_execution(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_execution(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Wykonanie flow o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_flow_execution(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
