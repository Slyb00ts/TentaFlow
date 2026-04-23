// =============================================================================
// Plik: api/dashboard/api_prompts.rs
// Opis: CRUD promptow - lista, tworzenie, edycja, usuwanie.
// =============================================================================

use crate::db::models::{NewPrompt, UpdatePrompt};
use crate::db::{self, DbPool};
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreatePromptRequest {
    pub prompt_id: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub prompt_type: String,
    pub default_model: Option<String>,
    pub variables: Option<String>,
    pub cache_priority: Option<i64>,
    pub language: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdatePromptRequest {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub prompt_type: String,
    pub default_model: Option<String>,
    pub variables: Option<String>,
    pub cache_priority: Option<i64>,
    pub is_active: Option<bool>,
    pub language: Option<String>,
}

const ALLOWED_PROMPT_TYPES: &[&str] = &["system", "suffix", "template", "user"];

/// GET /api/prompts - lista promptow z paginacja
pub fn handle_list(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_prompts(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// GET /api/prompts/:id - szczegoly promptu
pub fn handle_get(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    match db::repository::get_prompt(pool, id)? {
        Some(item) => Ok((200, serde_json::to_string(&item)?)),
        None => Ok((
            404,
            format!(r#"{{"error":"Prompt o id {} nie istnieje"}}"#, id),
        )),
    }
}

/// POST /api/prompts - utworz nowy prompt
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreatePromptRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((
            400,
            r#"{"error":"Pole 'name' nie może być puste"}"#.to_string(),
        ));
    }
    if !ALLOWED_PROMPT_TYPES.contains(&req.prompt_type.as_str()) {
        return Ok((
            400,
            format!(
                r#"{{"error":"Niedozwolona wartość prompt_type '{}'. Dozwolone: {}"}}"#,
                req.prompt_type,
                ALLOWED_PROMPT_TYPES.join(", ")
            ),
        ));
    }

    let language = req.language.as_deref().unwrap_or("pl");
    let params = NewPrompt {
        prompt_id: &req.prompt_id,
        name: &req.name,
        description: req.description.as_deref(),
        content: &req.content,
        prompt_type: &req.prompt_type,
        default_model: req.default_model.as_deref(),
        variables: req.variables.as_deref(),
        cache_priority: req.cache_priority.unwrap_or(0),
        language,
    };

    let id = db::repository::create_prompt(pool, &params)?;
    let item = db::repository::get_prompt(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/prompts/:id - aktualizuj prompt
pub fn handle_update(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_prompt(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Prompt o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdatePromptRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((
            400,
            r#"{"error":"Pole 'name' nie może być puste"}"#.to_string(),
        ));
    }
    if !ALLOWED_PROMPT_TYPES.contains(&req.prompt_type.as_str()) {
        return Ok((
            400,
            format!(
                r#"{{"error":"Niedozwolona wartość prompt_type '{}'. Dozwolone: {}"}}"#,
                req.prompt_type,
                ALLOWED_PROMPT_TYPES.join(", ")
            ),
        ));
    }

    let language = req
        .language
        .as_deref()
        .unwrap_or(existing.as_ref().map(|p| p.language.as_str()).unwrap_or("pl"));
    let params = UpdatePrompt {
        id,
        name: &req.name,
        description: req.description.as_deref(),
        content: &req.content,
        prompt_type: &req.prompt_type,
        default_model: req.default_model.as_deref(),
        variables: req.variables.as_deref(),
        cache_priority: req.cache_priority.unwrap_or(0),
        is_active: req.is_active.unwrap_or(true),
        language,
    };

    db::repository::update_prompt(pool, &params)?;
    let item = db::repository::get_prompt(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/prompts/:id - usun prompt
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_prompt(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Prompt o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_prompt(pool, id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}
