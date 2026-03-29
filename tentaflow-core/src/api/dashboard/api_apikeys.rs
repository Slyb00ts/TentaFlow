// =============================================================================
// Plik: api/dashboard/api_apikeys.rs
// Opis: Zarzadzanie kluczami API - lista, tworzenie, usuwanie.
// =============================================================================

use crate::db::{self, DbPool};
use super::auth;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub rate_limit_rps: Option<i64>,
}

#[derive(Serialize)]
pub struct CreateApiKeyResponse {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub key_prefix: String,
    pub rate_limit_rps: i64,
}

#[derive(Serialize)]
pub struct ApiKeyListItem {
    pub id: i64,
    pub key_prefix: String,
    pub name: String,
    pub rate_limit_rps: i64,
    pub is_active: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// GET /api/apikeys - lista kluczy API (bez pelnych hashy)
pub fn handle_list(pool: &DbPool) -> Result<(u16, String)> {
    let keys = db::repository::list_api_keys(pool)?;

    let items: Vec<ApiKeyListItem> = keys
        .into_iter()
        .map(|k| ApiKeyListItem {
            id: k.id,
            key_prefix: k.key_prefix,
            name: k.name,
            rate_limit_rps: k.rate_limit_rps,
            is_active: k.is_active,
            created_at: k.created_at,
            last_used_at: k.last_used_at,
        })
        .collect();

    Ok((200, serde_json::to_string(&items)?))
}

/// POST /api/apikeys - wygeneruj nowy klucz API
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateApiKeyRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return Ok((400, r#"{"error":"Niepoprawny format danych"}"#.to_string())),
    };

    if req.name.is_empty() || req.name.len() > 200 {
        return Ok((400, r#"{"error":"Nazwa musi miec od 1 do 200 znakow"}"#.to_string()));
    }

    let rate_limit = req.rate_limit_rps.unwrap_or(60);
    if !(1..=10000).contains(&rate_limit) {
        return Ok((400, r#"{"error":"rate_limit_rps musi byc w zakresie 1-10000"}"#.to_string()));
    }

    // Generuj losowy klucz API
    let raw_key = format!("sk-{}", uuid::Uuid::new_v4().simple());
    let key_hash = auth::hash_api_key(&raw_key);
    let key_prefix = format!("sk-...{}", &raw_key[raw_key.len() - 6..]);

    let id = db::repository::create_api_key(pool, &key_hash, &key_prefix, &req.name, rate_limit)?;

    tracing::info!("Audit: utworzono klucz API '{}' (id={})", req.name, id);

    let response = CreateApiKeyResponse {
        id,
        key: raw_key,
        name: req.name,
        key_prefix,
        rate_limit_rps: rate_limit,
    };

    Ok((201, serde_json::to_string(&response)?))
}

/// DELETE /api/apikeys/:id - usun klucz API
pub fn handle_delete(pool: &DbPool, id: i64) -> Result<(u16, String)> {
    let affected = db::repository::delete_api_key(pool, id)?;
    if affected == 0 {
        return Ok((404, r#"{"error":"Klucz API nie znaleziony"}"#.to_string()));
    }
    tracing::info!("Audit: usunieto klucz API id={}", id);
    Ok((200, r#"{"ok":true}"#.to_string()))
}
