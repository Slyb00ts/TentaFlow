// =============================================================================
// Plik: api/dashboard/api_flows.rs
// Opis: CRUD flow, flow_model_bindings, flow_node_templates i flow_executions.
// =============================================================================

use crate::db::{self, DbPool};
use crate::db::models::{FlowParams, FlowNodeTemplateParams};
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateFlowRequest {
    pub name: String,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub service_type: Option<String>,
    pub flow_json: Option<String>,
    pub status: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateFlowRequest {
    pub name: String,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub service_type: Option<String>,
    pub flow_json: String,
    pub status: Option<String>,
}

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

const ALLOWED_FLOW_SERVICE_TYPES: &[&str] = &["llm", "embedding", "stt", "tts", "rag", "memory"];

// --- Flows ---

/// GET /api/flows - lista flow z paginacja
pub fn handle_list_flows(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_flows(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// GET /api/flows/:id - szczegoly flow
pub fn handle_get_flow(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    match db::repository::get_flow(pool, id)? {
        Some(item) => Ok((200, serde_json::to_string(&item)?)),
        None => Ok((404, format!(r#"{{"error":"Flow o id {} nie istnieje"}}"#, id))),
    }
}

const DEFAULT_FLOW_JSON: &str = r#"{"nodes":[],"edges":[]}"#;

/// POST /api/flows - utworz flow
pub fn handle_create_flow(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateFlowRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'name' nie moze byc puste"}"#.to_string()));
    }
    if let Some(ref st) = req.service_type {
        if !ALLOWED_FLOW_SERVICE_TYPES.contains(&st.as_str()) {
            return Ok((400, format!(
                r#"{{"error":"Niedozwolona wartosc service_type '{}'. Dozwolone: {}"}}"#,
                st, ALLOWED_FLOW_SERVICE_TYPES.join(", ")
            )));
        }
    }
    let flow_json = req.flow_json.as_deref().unwrap_or(DEFAULT_FLOW_JSON);
    if let Err(e) = serde_json::from_str::<serde_json::Value>(flow_json) {
        return Ok((400, format!(r#"{{"error":"Niepoprawny flow_json: {}"}}"#, e)));
    }

    let params = FlowParams {
        name: &req.name,
        description: req.description.as_deref(),
        is_default: req.is_default.unwrap_or(false),
        service_type: req.service_type.as_deref(),
        flow_json,
        status: req.status.as_deref().unwrap_or("draft"),
    };

    let id = db::repository::create_flow(pool, &params)?;
    let item = db::repository::get_flow(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/flows/:id - aktualizuj flow (z optimistic locking)
pub fn handle_update_flow(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = match db::repository::get_flow(pool, id)? {
        Some(f) => f,
        None => return Ok((404, format!(r#"{{"error":"Flow o id {} nie istnieje"}}"#, id))),
    };

    let req: UpdateFlowRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'name' nie moze byc puste"}"#.to_string()));
    }
    if let Some(ref st) = req.service_type {
        if !ALLOWED_FLOW_SERVICE_TYPES.contains(&st.as_str()) {
            return Ok((400, format!(
                r#"{{"error":"Niedozwolona wartosc service_type '{}'. Dozwolone: {}"}}"#,
                st, ALLOWED_FLOW_SERVICE_TYPES.join(", ")
            )));
        }
    }
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&req.flow_json) {
        return Ok((400, format!(r#"{{"error":"Niepoprawny flow_json: {}"}}"#, e)));
    }

    let params = FlowParams {
        name: &req.name,
        description: req.description.as_deref(),
        is_default: req.is_default.unwrap_or(false),
        service_type: req.service_type.as_deref(),
        flow_json: &req.flow_json,
        status: req.status.as_deref().unwrap_or("draft"),
    };

    match db::repository::update_flow(pool, id, existing.version, &params) {
        Ok(()) => {}
        Err(e) if e.to_string().contains("CONFLICT") => {
            return Ok((409, r#"{"error":"Konflikt wersji - flow zostal zmodyfikowany przez innego uzytkownika"}"#.to_string()));
        }
        Err(e) => return Err(e),
    }
    let item = db::repository::get_flow(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/flows/:id - usun flow
pub fn handle_delete_flow(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_flow(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Flow o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_flow(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// --- Flow Model Bindings ---

/// GET /api/flow-bindings - lista powiazan flow-model
pub fn handle_list_bindings(pool: &DbPool) -> Result<(u16, String)> {
    let items = db::repository::list_flow_model_bindings(pool)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/flow-bindings - utworz powiazanie
pub fn handle_create_binding(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateFlowBindingRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

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
        return Ok((404, format!(r#"{{"error":"Powiazanie o id {} nie istnieje"}}"#, id)));
    }

    let req: UpdateFlowBindingRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

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
        return Ok((404, format!(r#"{{"error":"Powiazanie o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_flow_model_binding(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// --- Flow Node Templates ---

/// GET /api/flow-node-templates - lista szablonow wezlow
pub fn handle_list_node_templates(pool: &DbPool) -> Result<(u16, String)> {
    let items = db::repository::list_flow_node_templates(pool)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/flow-node-templates - utworz szablon wezla
pub fn handle_create_node_template(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateNodeTemplateRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

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
        return Ok((404, format!(r#"{{"error":"Szablon wezla o id {} nie istnieje"}}"#, id)));
    }

    let req: UpdateNodeTemplateRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

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
        return Ok((404, format!(r#"{{"error":"Szablon wezla o id {} nie istnieje"}}"#, id)));
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
        None => Ok((404, format!(r#"{{"error":"Wykonanie flow o id {} nie istnieje"}}"#, id))),
    }
}

/// DELETE /api/flow-executions/:id - usun wykonanie flow
pub fn handle_delete_execution(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_flow_execution(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Wykonanie flow o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_flow_execution(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
