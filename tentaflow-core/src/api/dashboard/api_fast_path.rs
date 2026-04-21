// =============================================================================
// Plik: api/dashboard/api_fast_path.rs
// Opis: CRUD wzorcow szybkiej sciezki (fast path patterns).
// =============================================================================

use crate::db::models::UpdateFastPathPattern;
use crate::db::{self, DbPool};
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateFastPathPatternRequest {
    pub module: String,
    pub pattern_type: String,
    pub pattern: String,
    pub match_type: String,
    pub result_json: Option<String>,
    pub priority: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateFastPathPatternRequest {
    pub module: String,
    pub pattern_type: String,
    pub pattern: String,
    pub match_type: String,
    pub result_json: Option<String>,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

/// Waliduje regex jesli match_type == "regex"
fn validate_pattern(pattern: &str, match_type: &str) -> std::result::Result<(), String> {
    if match_type == "regex" {
        Regex::new(pattern).map_err(|e| format!("{}", e))?;
    }
    Ok(())
}

/// GET /api/fast-path-patterns - lista wzorcow fast path z paginacja
pub fn handle_list(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_fast_path_patterns(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/fast-path-patterns - utworz wzorzec fast path
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateFastPathPatternRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.module.trim().is_empty() {
        return Ok((
            400,
            r#"{"error":"Pole 'module' nie może być puste"}"#.to_string(),
        ));
    }
    if let Err(e) = validate_pattern(&req.pattern, &req.match_type) {
        return Ok((
            400,
            format!(r#"{{"error":"Niepoprawne wyrażenie regularne: {}"}}"#, e),
        ));
    }

    let result_json = req.result_json.as_deref().unwrap_or("{}");

    let id = db::repository::create_fast_path_pattern(
        pool,
        &req.module,
        &req.pattern_type,
        &req.pattern,
        &req.match_type,
        result_json,
        req.priority.unwrap_or(0),
    )?;
    let item = db::repository::get_fast_path_pattern(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/fast-path-patterns/:id - aktualizuj wzorzec fast path
pub fn handle_update(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_fast_path_pattern(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(
                r#"{{"error":"Wzorzec fast path o id {} nie istnieje"}}"#,
                id
            ),
        ));
    }

    let req: UpdateFastPathPatternRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.module.trim().is_empty() {
        return Ok((
            400,
            r#"{"error":"Pole 'module' nie może być puste"}"#.to_string(),
        ));
    }
    if let Err(e) = validate_pattern(&req.pattern, &req.match_type) {
        return Ok((
            400,
            format!(r#"{{"error":"Niepoprawne wyrażenie regularne: {}"}}"#, e),
        ));
    }

    let result_json = req.result_json.as_deref().unwrap_or("{}");

    let params = UpdateFastPathPattern {
        id,
        module: &req.module,
        pattern_type: &req.pattern_type,
        pattern: &req.pattern,
        match_type: &req.match_type,
        result_json,
        is_active: req.is_active.unwrap_or(true),
        priority: req.priority.unwrap_or(0),
    };

    db::repository::update_fast_path_pattern(pool, &params)?;
    let item = db::repository::get_fast_path_pattern(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/fast-path-patterns/:id - usun wzorzec fast path
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_fast_path_pattern(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(
                r#"{{"error":"Wzorzec fast path o id {} nie istnieje"}}"#,
                id
            ),
        ));
    }

    db::repository::delete_fast_path_pattern(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
