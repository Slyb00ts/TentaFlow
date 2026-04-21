// =============================================================================
// Plik: api/dashboard/api_registries.rs
// Opis: REST API dla rejestrow Docker - CRUD, test polaczenia.
// =============================================================================

use crate::crypto::SecretsCipher;
use crate::db::{self, DbPool};
use std::sync::Arc;

#[derive(serde::Deserialize)]
struct RegistryRequest {
    name: String,
    #[serde(default)]
    registry_type: String,
    url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    skip_tls_verify: bool,
}

/// Walidacja pol nazwa i URL rejestru
fn validate_registry_request(req: &RegistryRequest) -> Option<(u16, String)> {
    if req.name.is_empty() || req.name.len() > 200 {
        return Some((
            400,
            r#"{"error":"Nazwa musi miec od 1 do 200 znakow"}"#.to_string(),
        ));
    }
    if req.url.is_empty() || req.url.len() > 500 {
        return Some((
            400,
            r#"{"error":"URL musi miec od 1 do 500 znakow"}"#.to_string(),
        ));
    }
    None
}

/// GET /api/registries - lista rejestrow (hasla zawsze "***")
pub fn handle_list(pool: &DbPool) -> anyhow::Result<(u16, String)> {
    let registries = db::repository::list_registries(pool)?;
    let masked: Vec<serde_json::Value> = registries
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "registry_type": r.registry_type,
                "url": r.url,
                "username": r.username,
                "password": "***",
                "is_active": r.is_active,
                "skip_tls_verify": r.skip_tls_verify,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
            })
        })
        .collect();
    Ok((200, serde_json::to_string(&masked)?))
}

/// POST /api/registries - dodaj rejestr (szyfruj haslo)
pub fn handle_create(
    pool: &DbPool,
    cipher: &Arc<SecretsCipher>,
    body: &[u8],
) -> anyhow::Result<(u16, String)> {
    let req: RegistryRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return Ok((400, r#"{"error":"Niepoprawny format danych"}"#.to_string())),
    };
    if let Some(err) = validate_registry_request(&req) {
        return Ok(err);
    }
    let registry_type = if req.registry_type.is_empty() {
        "custom"
    } else {
        &req.registry_type
    };
    let encrypted_password = if req.password.is_empty() {
        String::new()
    } else {
        cipher.encrypt(&req.password)?
    };
    let id = db::repository::create_registry(
        pool,
        &req.name,
        registry_type,
        &req.url,
        &req.username,
        &encrypted_password,
        req.skip_tls_verify,
    )?;
    tracing::info!("Audit: utworzono rejestr '{}' (id={})", req.name, id);
    Ok((201, format!(r#"{{"id":{}}}"#, id)))
}

/// PUT /api/registries/:id - aktualizuj rejestr
pub fn handle_update(
    pool: &DbPool,
    cipher: &Arc<SecretsCipher>,
    id: i64,
    body: &[u8],
) -> anyhow::Result<(u16, String)> {
    let req: RegistryRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return Ok((400, r#"{"error":"Niepoprawny format danych"}"#.to_string())),
    };
    if let Some(err) = validate_registry_request(&req) {
        return Ok(err);
    }
    let registry_type = if req.registry_type.is_empty() {
        "custom"
    } else {
        &req.registry_type
    };

    // Puste haslo = bez zmian (zachowaj stare)
    let encrypted_password = if req.password.is_empty() {
        match db::repository::get_registry(pool, id)? {
            Some(existing) => existing.password_encrypted,
            None => return Ok((404, r#"{"error":"Rejestr nie znaleziony"}"#.to_string())),
        }
    } else {
        cipher.encrypt(&req.password)?
    };

    db::repository::update_registry(
        pool,
        id,
        &req.name,
        registry_type,
        &req.url,
        &req.username,
        &encrypted_password,
        req.skip_tls_verify,
    )?;
    tracing::info!("Audit: zaktualizowano rejestr '{}' (id={})", req.name, id);
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// DELETE /api/registries/:id - usun rejestr
pub fn handle_delete(pool: &DbPool, id: i64) -> anyhow::Result<(u16, String)> {
    let affected = db::repository::delete_registry(pool, id)?;
    if affected == 0 {
        return Ok((404, r#"{"error":"Rejestr nie znaleziony"}"#.to_string()));
    }
    tracing::info!("Audit: usunieto rejestr id={}", id);
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// POST /api/registries/:id/test - test polaczenia z rejestrem
pub async fn handle_test(pool: &DbPool, cipher: &Arc<SecretsCipher>, id: i64) -> (u16, String) {
    let registry = match db::repository::get_registry(pool, id) {
        Ok(Some(r)) => r,
        Ok(None) => return (404, r#"{"error":"Rejestr nie znaleziony"}"#.to_string()),
        Err(e) => {
            return (
                500,
                format!(
                    r#"{{"error":"{}"}}"#,
                    super::escape_json_string(&e.to_string())
                ),
            )
        }
    };

    let password = cipher.decrypt_if_encrypted(&registry.password_encrypted);

    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(registry.skip_tls_verify)
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                500,
                format!(
                    r#"{{"error":"Blad tworzenia klienta HTTP: {}"}}"#,
                    super::escape_json_string(&e.to_string())
                ),
            )
        }
    };

    let url = format!("{}/v2/", registry.url.trim_end_matches('/'));
    let mut request = client.get(&url);
    if !registry.username.is_empty() {
        request = request.basic_auth(&registry.username, Some(&password));
    }

    match request.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status == 200 {
                (
                    200,
                    format!(
                        r#"{{"connected":true,"auth_ok":true,"registry_status":{}}}"#,
                        status
                    ),
                )
            } else if status == 401 {
                (
                    200,
                    format!(
                        r#"{{"connected":true,"auth_ok":false,"registry_status":{}}}"#,
                        status
                    ),
                )
            } else {
                (
                    502,
                    format!(
                        r#"{{"connected":false,"registry_status":{},"error":"Nieoczekiwany status"}}"#,
                        status
                    ),
                )
            }
        }
        Err(e) => (
            502,
            format!(
                r#"{{"connected":false,"error":"{}"}}"#,
                super::escape_json_string(&e.to_string())
            ),
        ),
    }
}
