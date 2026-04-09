// =============================================================================
// Plik: api/dashboard/api_settings.rs
// Opis: Odczyt i zapis ustawien routera.
// =============================================================================

use crate::db::{self, DbPool};
use super::auth::Claims;
use anyhow::Result;
use serde::Deserialize;

/// Klucze chronione przed zmiana przez API
const PROTECTED_SETTINGS: &[&str] = &["jwt_secret", "encryption_master_key"];

/// Fragmenty nazw kluczy ktorych wartosci sa maskowane w odpowiedzi API
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &["secret", "key", "password", "token", "master"];

#[derive(Deserialize)]
pub struct UpdateSettingRequest {
    pub key: String,
    pub value: String,
}

/// GET /api/settings - lista wszystkich ustawien (admin only, maskowanie sekretow)
pub fn handle_list(pool: &DbPool, claims: &Claims) -> Result<(u16, String)> {
    // VULN-016: Wymagaj uprawnien administratora
    let is_admin = db::repository::get_user_account_by_id(pool, claims.user_id)
        .ok()
        .flatten()
        .map(|u| u.is_admin)
        .unwrap_or(false);
    if !is_admin {
        return Ok((403, serde_json::json!({"error": "Brak uprawnien administratora"}).to_string()));
    }

    let settings = db::repository::list_settings(pool)?;

    // VULN-016: Maskuj wartosci wrażliwych kluczy
    let masked: Vec<serde_json::Value> = settings.iter().map(|s| {
        let key_lower = s.key.to_lowercase();
        let is_sensitive = SENSITIVE_KEY_FRAGMENTS.iter().any(|frag| key_lower.contains(frag));
        serde_json::json!({
            "key": s.key,
            "value": if is_sensitive { "***".to_string() } else { s.value.clone() },
            "updated_at": s.updated_at,
        })
    }).collect();

    Ok((200, serde_json::to_string(&masked)?))
}

/// PUT /api/settings - aktualizuj ustawienie
pub fn handle_update(pool: &DbPool, body: &[u8], settings_cipher: &crate::crypto::SettingsCipher) -> Result<(u16, String)> {
    let req: UpdateSettingRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if PROTECTED_SETTINGS.contains(&req.key.as_str()) {
        return Ok((403, serde_json::json!({"error": "Zmiana tego ustawienia jest zabroniona"}).to_string()));
    }

    db::repository::set_setting_secure(pool, &req.key, &req.value, settings_cipher)?;

    // VULN-033: Maskuj wartosc w odpowiedzi jesli klucz jest wrazliwy (analogicznie do handle_list)
    let key_lower = req.key.to_lowercase();
    let is_sensitive = SENSITIVE_KEY_FRAGMENTS.iter().any(|frag| key_lower.contains(frag));
    let display_value = if is_sensitive {
        "***".to_string()
    } else {
        db::repository::get_setting(pool, &req.key)?.unwrap_or_default()
    };

    let response = serde_json::json!({
        "key": req.key,
        "value": display_value
    });

    Ok((200, serde_json::to_string(&response)?))
}
