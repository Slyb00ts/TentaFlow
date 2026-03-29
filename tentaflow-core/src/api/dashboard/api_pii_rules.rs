// =============================================================================
// Plik: api/dashboard/api_pii_rules.rs
// Opis: CRUD regul filtrowania danych osobowych (PII).
// =============================================================================

use crate::db::{self, DbPool};
use crate::db::models::{NewPiiRule, UpdatePiiRule};
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreatePiiRuleRequest {
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
    pub priority: Option<i64>,
    pub description: Option<String>,
    pub test_examples: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdatePiiRuleRequest {
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
    pub description: Option<String>,
    pub test_examples: Option<String>,
}

/// GET /api/pii-rules - lista regul PII z paginacja
pub fn handle_list(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_pii_rules(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/pii-rules - utworz regule PII
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreatePiiRuleRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'name' nie moze byc puste"}"#.to_string()));
    }
    if let Err(e) = Regex::new(&req.pattern) {
        return Ok((400, format!(r#"{{"error":"Niepoprawne wyrazenie regularne: {}"}}"#, e)));
    }

    let params = NewPiiRule {
        name: &req.name,
        category: &req.category,
        pattern: &req.pattern,
        replacement: &req.replacement,
        priority: req.priority.unwrap_or(0),
        description: req.description.as_deref(),
        test_examples: req.test_examples.as_deref(),
    };

    let id = db::repository::create_pii_rule(pool, &params)?;
    let item = db::repository::get_pii_rule(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/pii-rules/:id - aktualizuj regule PII
pub fn handle_update(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_pii_rule(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Regula PII o id {} nie istnieje"}}"#, id)));
    }

    let req: UpdatePiiRuleRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'name' nie moze byc puste"}"#.to_string()));
    }
    if let Err(e) = Regex::new(&req.pattern) {
        return Ok((400, format!(r#"{{"error":"Niepoprawne wyrazenie regularne: {}"}}"#, e)));
    }

    let params = UpdatePiiRule {
        id,
        name: &req.name,
        category: &req.category,
        pattern: &req.pattern,
        replacement: &req.replacement,
        is_active: req.is_active.unwrap_or(true),
        priority: req.priority.unwrap_or(0),
        description: req.description.as_deref(),
        test_examples: req.test_examples.as_deref(),
    };

    db::repository::update_pii_rule(pool, &params)?;
    let item = db::repository::get_pii_rule(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/pii-rules/:id - usun regule PII
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_pii_rule(pool, id)?;
    if existing.is_none() {
        return Ok((404, format!(r#"{{"error":"Regula PII o id {} nie istnieje"}}"#, id)));
    }

    db::repository::delete_pii_rule(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
