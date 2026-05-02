// =============================================================================
// Plik: api/dashboard/api_addon_system.rs
// Opis: REST handlery flowu OAuth (SSO + per-addon). Pozostałe operacje na
//       userach/grupach/addonach idą binary protocolem przez dispatch/handlers.
// =============================================================================

use super::auth::Claims;
use crate::db::{self, DbPool};
use anyhow::Result;

// =============================================================================
// Helpery
// =============================================================================

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

fn parse_query_opt_string(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let val = parts.next()?;
        if key == name && !val.is_empty() {
            Some(val.to_string())
        } else {
            None
        }
    })
}

/// Pomocnik: pobiera wszystkie wartosci konfiguracji addonu z tabeli addon_config.
fn get_addon_config_map(
    pool: &DbPool,
    addon_id: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM addon_config WHERE addon_id = ?1")
        .map_err(|e| anyhow::anyhow!("Blad przygotowania zapytania: {}", e))?;
    let map: std::collections::HashMap<String, String> = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(map)
}

// =============================================================================
// SSO Providers API — flow OAuth (login redirect + callback). Zarzadzanie
// providerami (list/create/delete) odbywa sie przez binary protocol.
// =============================================================================

/// GET /api/sso/login/:provider_id — generuje auth URL i zwraca redirect
pub async fn handle_sso_login(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    provider_id: i64,
    redirect_base_url: &str,
) -> Result<(u16, String)> {
    let provider = db::repository::get_sso_provider(pool, provider_id)?
        .ok_or_else(|| anyhow::anyhow!("SSO provider nie znaleziony"))?;

    if !provider.enabled {
        return Ok((400, json_error("SSO provider jest wyłączony")));
    }

    // Odszyfruj client_secret
    let client_secret = cipher
        .decrypt(&provider.client_secret_encrypted)
        .map_err(|e| anyhow::anyhow!("Blad odszyfrowywania client_secret: {}", e))?;

    // Pobierz redirect base URL z ustawien DB (fallback na przekazany z Host header)
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| redirect_base_url.to_string());

    let config = crate::auth::sso::provider_to_config(&provider, &client_secret, &base_url);

    // Discovery
    let discovery = crate::auth::sso::discover(&config.discovery_url)
        .await
        .map_err(|e| anyhow::anyhow!("Blad OIDC discovery: {}", e))?;

    // Generuj state (anti-CSRF) — provider_id + losowy UUID + timestamp
    let state = format!("{}:{}", provider_id, uuid::Uuid::new_v4());

    // Zapisz state z timestampem w ustawieniach (walidacja TTL przy callback)
    let state_value = format!("{}:{}", provider_id, chrono::Utc::now().timestamp());
    let _ = db::repository::set_setting(pool, &format!("sso_state:{}", state), &state_value);

    let auth_url = crate::auth::sso::build_auth_url(&config, &discovery, &state);

    Ok((
        200,
        serde_json::json!({
            "auth_url": auth_url,
            "state": state,
        })
        .to_string(),
    ))
}

/// GET /api/sso/callback?code=...&state=... — callback po zalogowaniu SSO
pub async fn handle_sso_callback(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    query: &str,
    redirect_base_url: &str,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<(u16, String)> {
    let code = parse_query_opt_string(query, "code")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'code' w callback"))?;
    let state = parse_query_opt_string(query, "state")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'state' w callback"))?;

    // Zweryfikuj state (anti-CSRF) z walidacja TTL
    let state_key = format!("sso_state:{}", state);
    let state_value = db::repository::get_setting(pool, &state_key)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny lub wygasniety state SSO"))?;

    // Natychmiast usun zuzyty state (jednorazowe uzycie — zapobiega replay attack)
    let _ = db::repository::delete_setting(pool, &state_key);

    // Parsuj provider_id i timestamp z state_value (format: "provider_id:timestamp")
    let parts: Vec<&str> = state_value.splitn(2, ':').collect();
    let provider_id: i64 = parts
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny provider_id w state"))?;

    // Sprawdz TTL state (max 10 minut)
    if let Some(ts_str) = parts.get(1) {
        if let Ok(ts) = ts_str.parse::<i64>() {
            let now = chrono::Utc::now().timestamp();
            let max_age_seconds = 600; // 10 minut
            if now - ts > max_age_seconds {
                return Err(anyhow::anyhow!(
                    "State SSO wygasniety (starszy niz 10 minut)"
                ));
            }
        }
    }

    let provider = db::repository::get_sso_provider(pool, provider_id)?
        .ok_or_else(|| anyhow::anyhow!("SSO provider nie znaleziony"))?;

    // Odszyfruj client_secret
    let client_secret = cipher
        .decrypt(&provider.client_secret_encrypted)
        .map_err(|e| anyhow::anyhow!("Blad odszyfrowywania client_secret: {}", e))?;

    // Pobierz redirect base URL z ustawien DB (fallback na przekazany z Host header)
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| redirect_base_url.to_string());

    let config = crate::auth::sso::provider_to_config(&provider, &client_secret, &base_url);

    // Discovery
    let discovery = crate::auth::sso::discover(&config.discovery_url)
        .await
        .map_err(|e| anyhow::anyhow!("Blad OIDC discovery: {}", e))?;

    // Pelny flow: exchange code -> get user info -> find/create user -> JWT
    let result =
        crate::auth::sso::handle_sso_callback(pool, &config, &discovery, &code, settings_cipher)
            .await?;

    // Redirect do dashboardu z tokenem JWT w query param
    let redirect_url = format!(
        "{}/?token={}",
        base_url.trim_end_matches('/'),
        urlencoding::encode(&result.token)
    );
    Ok((
        200,
        serde_json::json!({
            "redirect_url": redirect_url,
            "token": result.token,
            "username": result.username,
            "is_new_user": result.is_new_user,
        })
        .to_string(),
    ))
}

// =============================================================================
// Addon OAuth — osobny flow OAuth per addon (np. Teams -> Graph API)
// =============================================================================

/// GET /api/addons/:addon_id/oauth/login — generuje auth URL dla addonu
/// Wymaga JWT (musimy wiedziec ktory uzytkownik sie loguje).
/// Buduje auth URL z client_id z addon config, scopami z manifestu,
/// redirect_uri z oauth_redirect_base_url + /api/addons/{addon_id}/oauth/callback.
pub async fn handle_addon_oauth_login(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
) -> Result<(u16, String)> {
    // Sprawdz czy addon istnieje
    let addon = db::repository::get_addon(pool, addon_id)?
        .ok_or_else(|| anyhow::anyhow!("Addon '{}' nie znaleziony", addon_id))?;

    if !addon.is_enabled {
        return Ok((400, json_error("Addon jest wyłączony")));
    }

    // Pobierz redirect base URL z ustawien DB
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://localhost:8090".to_string());

    // Pobierz konfiguracje addonu — client_id, tenant_id, scopes
    let config = get_addon_config_map(pool, addon_id)?;
    let client_id = config
        .get("client_id")
        .or_else(|| config.get("azure_client_id"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_id w konfiguracji addonu '{}'", addon_id))?;

    let tenant_id = config
        .get("tenant_id")
        .or_else(|| config.get("azure_tenant_id"))
        .cloned()
        .unwrap_or_else(|| "common".to_string());

    // Domyslne scopy per addon — Teams potrzebuje dodatkowych uprawnien Graph API
    let scopes = match addon_id {
        "teams" => "offline_access Chat.ReadWrite Calendars.Read Files.Read OnlineMeetings.ReadWrite User.Read",
        _ => "offline_access User.Read",
    };

    let redirect_uri = format!(
        "{}/api/addons/{}/oauth/callback",
        base_url.trim_end_matches('/'),
        addon_id
    );

    // Generuj state (anti-CSRF) — addon_id + user_id + losowy UUID + timestamp
    let state = format!("{}:{}:{}", addon_id, claims.user_id, uuid::Uuid::new_v4());
    let state_value = format!(
        "{}:{}:{}",
        addon_id,
        claims.user_id,
        chrono::Utc::now().timestamp()
    );
    let _ =
        db::repository::set_setting(pool, &format!("addon_oauth_state:{}", state), &state_value);

    // Buduj auth URL (Microsoft Azure AD / Entra ID)
    let auth_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        urlencoding::encode(&tenant_id),
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scopes),
        urlencoding::encode(&state),
    );

    Ok((
        200,
        serde_json::json!({
            "auth_url": auth_url,
            "state": state,
        })
        .to_string(),
    ))
}

/// GET /api/addons/:addon_id/oauth/callback?code=xxx&state=yyy
/// Callback OAuth per addon — wymienia code na tokeny, zapisuje do addon secrets per user.
/// Nie wymaga JWT — user wraca z Microsoft redirect.
pub async fn handle_addon_oauth_callback(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    path: &str,
    query: &str,
) -> Result<(u16, String)> {
    // Wyciagnij addon_id ze sciezki: /api/addons/{addon_id}/oauth/callback
    let addon_id = path
        .strip_prefix("/api/addons/")
        .and_then(|rest| rest.strip_suffix("/oauth/callback"))
        .ok_or_else(|| anyhow::anyhow!("Niepoprawna sciezka callback"))?;

    let code = parse_query_opt_string(query, "code")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'code' w callback"))?;
    let state = parse_query_opt_string(query, "state")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'state' w callback"))?;

    // Obsluga bledow od Microsoft
    if let Some(error) = parse_query_opt_string(query, "error") {
        let error_desc = parse_query_opt_string(query, "error_description").unwrap_or_default();
        return Err(anyhow::anyhow!("Blad OAuth: {} — {}", error, error_desc));
    }

    // Zweryfikuj state (anti-CSRF)
    let state_key = format!("addon_oauth_state:{}", state);
    let state_value = db::repository::get_setting(pool, &state_key)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny lub wygasniety state OAuth"))?;

    // Natychmiast usun zuzyty state (jednorazowe uzycie)
    let _ = db::repository::delete_setting(pool, &state_key);

    // Parsuj addon_id, user_id i timestamp z state_value
    let parts: Vec<&str> = state_value.splitn(3, ':').collect();
    let stored_addon_id = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny addon_id w state"))?;
    let user_id: i64 = parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny user_id w state"))?;

    if *stored_addon_id != addon_id {
        return Err(anyhow::anyhow!("Niezgodnosc addon_id w state"));
    }

    // Sprawdz TTL state (max 10 minut)
    if let Some(ts_str) = parts.get(2) {
        if let Ok(ts) = ts_str.parse::<i64>() {
            let now = chrono::Utc::now().timestamp();
            if now - ts > 600 {
                return Err(anyhow::anyhow!(
                    "State OAuth wygasniety (starszy niz 10 minut)"
                ));
            }
        }
    }

    // Pobierz konfiguracje addonu — client_id, client_secret, tenant_id
    let config = get_addon_config_map(pool, addon_id)?;
    let client_id = config
        .get("client_id")
        .or_else(|| config.get("azure_client_id"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_id w konfiguracji addonu"))?;

    let client_secret_encrypted = config
        .get("client_secret")
        .or_else(|| config.get("azure_client_secret"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_secret w konfiguracji addonu"))?;

    let client_secret = cipher
        .decrypt(&client_secret_encrypted)
        .unwrap_or_else(|_| client_secret_encrypted.clone());

    let tenant_id = config
        .get("tenant_id")
        .or_else(|| config.get("azure_tenant_id"))
        .cloned()
        .unwrap_or_else(|| "common".to_string());

    // Pobierz redirect base URL z DB
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://localhost:8090".to_string());

    let redirect_uri = format!(
        "{}/api/addons/{}/oauth/callback",
        base_url.trim_end_matches('/'),
        addon_id
    );

    // Wymien code na tokeny (server-to-server)
    let token_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant_id
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("Blad tworzenia klienta HTTP: {}", e))?;

    let params = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ];

    let response = client
        .post(&token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Blad wymiany code na token: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Wymiana code na token zwrocila status {}: {}",
            status,
            body
        ));
    }

    let token_data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Blad parsowania odpowiedzi tokenowej: {}", e))?;

    let access_token = token_data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let refresh_token = token_data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if access_token.is_empty() {
        return Err(anyhow::anyhow!("Brak access_token w odpowiedzi tokenowej"));
    }

    // Zaszyfruj i zapisz tokeny do addon secrets per user
    let encrypted_access = cipher
        .encrypt(&access_token)
        .unwrap_or_else(|_| access_token.clone());
    let encrypted_refresh = cipher
        .encrypt(&refresh_token)
        .unwrap_or_else(|_| refresh_token.clone());

    // Zapisz tokeny do addon secrets per user
    db::repository::set_addon_secret(
        pool,
        addon_id,
        Some(user_id),
        "oauth_token",
        &encrypted_access,
    )?;
    if !refresh_token.is_empty() {
        db::repository::set_addon_secret(
            pool,
            addon_id,
            Some(user_id),
            "refresh_token",
            &encrypted_refresh,
        )?;
    }

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(user_id),
        None,
        "addon.oauth.authorized",
        Some(addon_id),
        None,
        None,
        None,
    );

    // Redirect do dashboardu z komunikatem sukcesu
    let redirect_url = format!(
        "{}/#/addons?oauth_success={}&addon={}",
        base_url.trim_end_matches('/'),
        addon_id,
        addon_id
    );

    Ok((
        200,
        serde_json::json!({
            "redirect_url": redirect_url,
            "ok": true,
        })
        .to_string(),
    ))
}
