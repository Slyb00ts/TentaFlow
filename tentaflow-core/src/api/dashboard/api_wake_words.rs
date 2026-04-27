// =============================================================================
// Plik: api/dashboard/api_wake_words.rs
// Opis: CRUD wake-words teams-bota. Tabela `teams_bot_wake_words`.
//       Endpointy:
//         GET    /api/wake-words           — lista wszystkich
//         POST   /api/wake-words           — dodaj { word }
//         PATCH  /api/wake-words/:id       — toggle { enabled }
//         DELETE /api/wake-words/:id       — usun
// =============================================================================

use anyhow::Result;
use serde::Deserialize;

use crate::db::{self, DbPool};

#[derive(Deserialize)]
pub struct CreateRequest {
    pub word: String,
}

#[derive(Deserialize)]
pub struct ToggleRequest {
    pub enabled: bool,
}

/// GET /api/wake-words
pub fn handle_list(pool: &DbPool) -> Result<(u16, String)> {
    let items = db::repository::list_wake_words(pool)?;
    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/wake-words
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;
    let word = req.word.trim();
    if word.is_empty() {
        return Ok((400, r#"{"error":"Slowo nie moze byc puste"}"#.to_string()));
    }
    if word.len() > 64 {
        return Ok((400, r#"{"error":"Slowo za dlugie (max 64 znakow)"}"#.to_string()));
    }
    if word.contains(',') {
        return Ok((400, r#"{"error":"Przecinek niedozwolony — to separator CSV"}"#.to_string()));
    }
    let id = db::repository::add_wake_word(pool, word)?;
    Ok((201, format!(r#"{{"id":{},"word":"{}","enabled":true}}"#, id, word)))
}

/// PATCH /api/wake-words/:id  body: {"enabled": bool}
pub fn handle_toggle(pool: &DbPool, id: i64, body: &[u8]) -> Result<(u16, String)> {
    let req: ToggleRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;
    db::repository::set_wake_word_enabled(pool, id, req.enabled)?;
    Ok((200, format!(r#"{{"id":{},"enabled":{}}}"#, id, req.enabled)))
}

/// DELETE /api/wake-words/:id
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    db::repository::delete_wake_word(pool, id)?;
    Ok((200, r#"{"deleted":true}"#.to_string()))
}
