// =============================================================================
// Plik: auth/sso.rs
// Opis: Uniwersalny klient OIDC — discovery, auth URL, wymiana code na token,
//       pobranie user info, pelny flow SSO callback z tworzeniem uzytkownika.
// =============================================================================

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::models::SsoProvider;
use crate::db::{self, DbPool};

/// Konfiguracja OIDC providera (pochodzi z DB — SsoProvider)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcConfig {
    pub provider_id: i64,
    pub provider_name: String,
    pub provider_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub discovery_url: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub auto_create_users: bool,
    pub default_group_id: Option<i64>,
}

/// Endpointy odkryte z .well-known/openid-configuration
#[derive(Debug, Clone, Deserialize)]
pub struct OidcDiscovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub userinfo_endpoint: String,
    #[serde(default)]
    pub jwks_uri: String,
    pub issuer: String,
}

/// Odpowiedz wymiany code na token
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub id_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub token_type: String,
}

/// Informacje o uzytkowniku z OIDC userinfo endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct OidcUserInfo {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
}

/// Wynik SSO callback — uzytkownik + token JWT
#[derive(Debug, Serialize)]
pub struct SsoCallbackResult {
    pub token: String,
    pub username: String,
    pub is_new_user: bool,
}

/// Buduje URL discovery z bazowego URL providera.
/// Dodaje /.well-known/openid-configuration jesli nie jest juz w URL.
fn build_discovery_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/.well-known/openid-configuration") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/.well-known/openid-configuration")
    }
}

/// Pobiera konfiguracje OIDC z .well-known/openid-configuration
pub async fn discover(discovery_url: &str) -> Result<OidcDiscovery> {
    let url = build_discovery_url(discovery_url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Tworzenie klienta HTTP dla OIDC discovery")?;

    let response = client
        .get(&url)
        .send()
        .await
        .context("Zapytanie OIDC discovery")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "OIDC discovery zwrocil status {}: {}",
            response.status(),
            url
        ));
    }

    let discovery: OidcDiscovery = response
        .json()
        .await
        .context("Parsowanie odpowiedzi OIDC discovery")?;

    Ok(discovery)
}

/// Buduje URL autoryzacji do przekierowania uzytkownika
pub fn build_auth_url(config: &OidcConfig, discovery: &OidcDiscovery, state: &str) -> String {
    let scopes = if config.scopes.is_empty() {
        "openid profile email".to_string()
    } else {
        config.scopes.join(" ")
    };

    let params = [
        ("response_type", "code"),
        ("client_id", &config.client_id),
        ("redirect_uri", &config.redirect_uri),
        ("scope", &scopes),
        ("state", state),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{}", discovery.authorization_endpoint, query)
}

/// Wymienia authorization code na tokeny (access_token, id_token, refresh_token)
pub async fn exchange_code(
    config: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("Tworzenie klienta HTTP do wymiany code")?;

    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &config.redirect_uri),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
    ];

    let response = client
        .post(&discovery.token_endpoint)
        .form(&params)
        .send()
        .await
        .context("Zapytanie wymiany code na token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Wymiana code na token zwrocila status {}: {}",
            status,
            body
        ));
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .context("Parsowanie odpowiedzi wymiany code na token")?;

    Ok(token_response)
}

/// Pobiera informacje o uzytkowniku z userinfo endpoint
pub async fn get_user_info(discovery: &OidcDiscovery, access_token: &str) -> Result<OidcUserInfo> {
    if discovery.userinfo_endpoint.is_empty() {
        return Err(anyhow::anyhow!(
            "Provider OIDC nie udostepnia userinfo endpoint"
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Tworzenie klienta HTTP do userinfo")?;

    let response = client
        .get(&discovery.userinfo_endpoint)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
        .context("Zapytanie userinfo")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Userinfo zwrocil status {}: {}",
            status,
            body
        ));
    }

    let user_info: OidcUserInfo = response
        .json()
        .await
        .context("Parsowanie odpowiedzi userinfo")?;

    Ok(user_info)
}

/// Konwertuje SsoProvider z DB na OidcConfig.
/// Wymaga odszyfrowania client_secret przez SecretsCipher.
pub fn provider_to_config(
    provider: &SsoProvider,
    client_secret_decrypted: &str,
    redirect_base_url: &str,
) -> OidcConfig {
    OidcConfig {
        provider_id: provider.id,
        provider_name: provider.name.clone(),
        provider_type: provider.provider_type.clone(),
        client_id: provider.client_id.clone(),
        client_secret: client_secret_decrypted.to_string(),
        discovery_url: provider.discovery_url.clone(),
        redirect_uri: format!(
            "{}/api/sso/callback",
            redirect_base_url.trim_end_matches('/')
        ),
        scopes: vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
        ],
        auto_create_users: provider.auto_create_users,
        default_group_id: provider.default_group_id,
    }
}

/// Pelny flow SSO callback:
/// 1. Wymiana code na token
/// 2. Pobranie user info
/// 3. Znalezienie lub utworzenie uzytkownika w bazie
/// 4. Wygenerowanie JWT
pub async fn handle_sso_callback(
    db: &DbPool,
    config: &OidcConfig,
    discovery: &OidcDiscovery,
    code: &str,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<SsoCallbackResult> {
    // Krok 1: Wymiana code na token
    let token_response = exchange_code(config, discovery, code).await?;

    // Krok 2: Pobranie user info
    let user_info = get_user_info(discovery, &token_response.access_token).await?;

    // Krok 3: Znalezienie lub utworzenie uzytkownika
    let provider_name = &config.provider_name;
    let subject = &user_info.sub;

    // Sprawdz czy uzytkownik z tym SSO subject juz istnieje
    let existing_user = db::repository::get_user_account_by_sso(db, provider_name, subject)?;

    let (user_id, username, is_new_user) = if let Some(user) = existing_user {
        // Uzytkownik juz istnieje — aktualizuj last_login
        let _ = db::repository::update_user_account_last_login(db, user.id);
        (user.id, user.username, false)
    } else if config.auto_create_users {
        // Automatyczne tworzenie uzytkownika
        let username = determine_username(&user_info);
        let display_name = user_info.name.clone().unwrap_or_else(|| username.clone());
        let email = user_info.email.clone().unwrap_or_default();

        let user_id = db::repository::create_user_account_sso(
            db,
            &username,
            &display_name,
            &email,
            provider_name,
            subject,
        )?;

        // Dodaj do domyslnej grupy jesli skonfigurowana
        if let Some(group_id) = config.default_group_id {
            let _ = db::repository::add_user_to_group(db, group_id, user_id);
        }

        // Audit log
        let _ = db::repository::log_audit(
            db,
            Some(user_id),
            None,
            "sso.user_created",
            Some(&username),
            Some(&format!("provider={}, sub={}", provider_name, subject)),
            None,
            None,
        );

        (user_id, username, true)
    } else {
        return Err(anyhow::anyhow!(
            "Uzytkownik SSO nie istnieje i automatyczne tworzenie kont jest wylaczone \
             (provider={}, sub={})",
            provider_name,
            subject
        ));
    };

    // Krok 4: Wygenerowanie JWT
    let jwt_secret = db::repository::get_setting_secure(db, "jwt_secret", settings_cipher)?
        .ok_or_else(|| anyhow::anyhow!("Brak jwt_secret w ustawieniach"))?;

    let expiry_hours: i64 = db::repository::get_setting(db, "jwt_expiry_hours")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);

    let token =
        crate::api::dashboard::auth::generate_jwt(user_id, &username, &jwt_secret, expiry_hours)?;

    // Audit log
    let _ = db::repository::log_audit(
        db,
        Some(user_id),
        None,
        "sso.login",
        Some(&username),
        Some(&format!("provider={}", provider_name)),
        None,
        None,
    );

    Ok(SsoCallbackResult {
        token,
        username,
        is_new_user,
    })
}

/// Wyznacza nazwe uzytkownika z danych OIDC.
/// Priorytet: preferred_username > email (przed @) > sub
fn determine_username(user_info: &OidcUserInfo) -> String {
    if let Some(ref username) = user_info.preferred_username {
        if !username.is_empty() {
            return sanitize_username(username);
        }
    }
    if let Some(ref email) = user_info.email {
        if let Some(local_part) = email.split('@').next() {
            if !local_part.is_empty() {
                return sanitize_username(local_part);
            }
        }
    }
    sanitize_username(&user_info.sub)
}

/// Oczyszcza nazwe uzytkownika — dopuszcza tylko znaki alfanumeryczne, myslnik i podkreslenie
fn sanitize_username(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect::<String>()
        .to_lowercase()
}
