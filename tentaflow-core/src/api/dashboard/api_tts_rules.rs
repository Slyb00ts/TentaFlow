// =============================================================================
// Plik: api/dashboard/api_tts_rules.rs
// Opis: CRUD regul czyszczenia tekstu dla TTS.
// =============================================================================

use crate::db::models::UpdateTtsCleaningRule;
use crate::db::{self, DbPool};
use anyhow::Result;
use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateTtsRuleRequest {
    pub rule_type: String,
    pub pattern: String,
    pub replacement: Option<String>,
    pub language: String,
    pub priority: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateTtsRuleRequest {
    pub rule_type: String,
    pub pattern: String,
    pub replacement: Option<String>,
    pub language: String,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

const ALLOWED_RULE_TYPES: &[&str] = &["abbreviation", "phonetic", "emoji_range", "regex_remove"];

/// GET /api/tts-rules - lista regul TTS z paginacja
pub fn handle_list(pool: &DbPool, offset: i64, limit: i64) -> Result<(u16, String)> {
    let items = db::repository::list_tts_cleaning_rules(pool, offset, limit)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/tts-rules - utworz regule TTS
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateTtsRuleRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if !ALLOWED_RULE_TYPES.contains(&req.rule_type.as_str()) {
        return Ok((
            400,
            format!(
                r#"{{"error":"Niedozwolona wartosc rule_type '{}'. Dozwolone: {}"}}"#,
                req.rule_type,
                ALLOWED_RULE_TYPES.join(", ")
            ),
        ));
    }
    if let Err(e) = Regex::new(&req.pattern) {
        return Ok((
            400,
            format!(r#"{{"error":"Niepoprawne wyrazenie regularne: {}"}}"#, e),
        ));
    }

    let id = db::repository::create_tts_cleaning_rule(
        pool,
        &req.rule_type,
        &req.pattern,
        req.replacement.as_deref(),
        &req.language,
        req.priority.unwrap_or(0),
    )?;
    crate::tts::clean_cache::refresh(pool);
    let item = db::repository::get_tts_cleaning_rule(pool, id)?;
    Ok((201, serde_json::to_string(&item)?))
}

/// PUT /api/tts-rules/:id - aktualizuj regule TTS
pub fn handle_update(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let existing = db::repository::get_tts_cleaning_rule(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Regula TTS o id {} nie istnieje"}}"#, id),
        ));
    }

    let req: UpdateTtsRuleRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if !ALLOWED_RULE_TYPES.contains(&req.rule_type.as_str()) {
        return Ok((
            400,
            format!(
                r#"{{"error":"Niedozwolona wartosc rule_type '{}'. Dozwolone: {}"}}"#,
                req.rule_type,
                ALLOWED_RULE_TYPES.join(", ")
            ),
        ));
    }
    if let Err(e) = Regex::new(&req.pattern) {
        return Ok((
            400,
            format!(r#"{{"error":"Niepoprawne wyrazenie regularne: {}"}}"#, e),
        ));
    }

    let params = UpdateTtsCleaningRule {
        id,
        rule_type: &req.rule_type,
        pattern: &req.pattern,
        replacement: req.replacement.as_deref(),
        language: &req.language,
        is_active: req.is_active.unwrap_or(true),
        priority: req.priority.unwrap_or(0),
    };

    db::repository::update_tts_cleaning_rule(pool, &params)?;
    crate::tts::clean_cache::refresh(pool);
    let item = db::repository::get_tts_cleaning_rule(pool, id)?;
    Ok((200, serde_json::to_string(&item)?))
}

/// DELETE /api/tts-rules/:id - usun regule TTS
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let existing = db::repository::get_tts_cleaning_rule(pool, id)?;
    if existing.is_none() {
        return Ok((
            404,
            format!(r#"{{"error":"Regula TTS o id {} nie istnieje"}}"#, id),
        ));
    }

    db::repository::delete_tts_cleaning_rule(pool, id)?;
    crate::tts::clean_cache::refresh(pool);
    Ok((200, r#"{"ok":true}"#.to_string()))
}
