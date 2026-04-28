// =============================================================================
// Plik: api/dashboard/api_me_preferences.rs
// Opis: Preferencje zalogowanego uzytkownika — obecnie preferowany jezyk
//       wykorzystywany przez TTS gdy klient nie poda pola `language`.
// =============================================================================

use super::auth::Claims;
use crate::db::{repository, DbPool};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct PreferencesResponse {
    language: Option<String>,
}

#[derive(Deserialize)]
struct PreferencesUpdate {
    language: Option<String>,
}

/// GET /api/me/preferences — zwraca biezace preferencje uzytkownika.
pub fn handle_get(pool: &DbPool, claims: &Claims) -> Result<(u16, String)> {
    let language = repository::get_user_preferred_language(pool, claims.user_id)?;
    let body = serde_json::to_string(&PreferencesResponse { language })?;
    Ok((200, body))
}

/// PUT /api/me/preferences — ustawia preferowany jezyk. `null` lub brak pola
/// czysci preferencje. Zwraca 400 gdy kod jezyka nie jest obslugiwany.
pub fn handle_put(pool: &DbPool, claims: &Claims, body: &[u8]) -> Result<(u16, String)> {
    let req: PreferencesUpdate = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => {
            return Ok((
                400,
                serde_json::json!({"error": "invalid JSON body"}).to_string(),
            ));
        }
    };

    if repository::set_user_preferred_language(pool, claims.user_id, req.language.as_deref())
        .is_err()
    {
        return Ok((
            400,
            serde_json::json!({"error": "unsupported language code"}).to_string(),
        ));
    }

    let language = repository::get_user_preferred_language(pool, claims.user_id)?;
    let body = serde_json::to_string(&PreferencesResponse { language })?;
    Ok((200, body))
}
