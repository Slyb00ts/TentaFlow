// =============================================================================
// Plik: addon/oauth.rs
// Opis: OAuth 2.0 (+ PKCE) engine dla addonow — build authorize URL,
//       exchange code→tokeny, refresh, revoke, fetch userinfo. Providerzy
//       tokensz/userinfo: microsoft (Graph), google (userinfo), github (/user).
//       Wspiera dowolnego providera OIDC przez `authorize_url/token_url/revoke_url`
//       z manifestu addonu.
// =============================================================================

use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::db::repository::{DbAddonOAuthConfig, DbAddonOAuthProviderDecl};

/// Pelen komplet tokenow zwroconych przez OAuth provider.
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_in_secs: Option<u64>,
    pub scope: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum OAuthError {
    #[error("konfiguracja OAuth niekompletna: {0}")]
    BadConfig(String),
    #[error("blad HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider zwrocil blad: {0}")]
    Provider(String),
    #[error("nieparsowalne userinfo: {0}")]
    UserInfo(String),
}

/// Generuje losowy `state` (32 bajty base64url, anti-CSRF).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS RNG fill_bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Generuje PKCE `code_verifier` (43-128 znakow base64url bez padding).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 64];
    getrandom::fill(&mut bytes).expect("OS RNG fill_bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `code_challenge` = base64url(sha256(code_verifier)).
pub fn code_challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Wspolny klient HTTP dla OAuth calls — twarde timeouty zapobiegaja hang na
/// wolnym/nieresponsywnym providerze (10s total, 5s connect).
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client builder")
}

/// Buduje pelny authorize URL z parametrami (state + opcjonalnie PKCE).
pub fn build_authorize_url(
    cfg: &DbAddonOAuthConfig,
    decl: &DbAddonOAuthProviderDecl,
    state: &str,
    code_challenge: Option<&str>,
) -> String {
    let scopes = decl.scopes.replace(',', " ");
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&scope={}",
        decl.authorize_url,
        urlencoding::encode(&cfg.client_id),
        urlencoding::encode(&cfg.redirect_uri),
        urlencoding::encode(state),
        urlencoding::encode(&scopes),
    );
    if decl.pkce {
        if let Some(ch) = code_challenge {
            url.push_str(&format!(
                "&code_challenge={}&code_challenge_method=S256",
                urlencoding::encode(ch)
            ));
        }
    }
    url
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_token_type")]
    token_type: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// Wymienia authorization code na tokeny. Zwraca access+refresh+metadata.
pub async fn exchange_code(
    cfg: &DbAddonOAuthConfig,
    decl: &DbAddonOAuthProviderDecl,
    client_secret: &str,
    code: &str,
    code_verifier: Option<&str>,
) -> std::result::Result<OAuthTokens, OAuthError> {
    if cfg.client_id.is_empty() {
        return Err(OAuthError::BadConfig("pusty client_id".into()));
    }
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", cfg.redirect_uri.clone()),
        ("client_id", cfg.client_id.clone()),
        ("client_secret", client_secret.to_string()),
    ];
    if let Some(v) = code_verifier {
        form.push(("code_verifier", v.to_string()));
    }
    let client = http_client();
    let resp = client.post(&decl.token_url).form(&form).send().await?;
    let body: TokenResponse = resp.json().await?;
    if let Some(e) = body.error {
        return Err(OAuthError::Provider(format!(
            "{}: {}",
            e,
            body.error_description.unwrap_or_default()
        )));
    }
    Ok(OAuthTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        token_type: body.token_type,
        expires_in_secs: body.expires_in,
        scope: body.scope,
    })
}

/// Odswieza access_token przez refresh_token.
pub async fn refresh_token(
    cfg: &DbAddonOAuthConfig,
    decl: &DbAddonOAuthProviderDecl,
    client_secret: &str,
    refresh_token: &str,
) -> std::result::Result<OAuthTokens, OAuthError> {
    let form: Vec<(&str, String)> = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", cfg.client_id.clone()),
        ("client_secret", client_secret.to_string()),
    ];
    let client = http_client();
    let resp = client.post(&decl.token_url).form(&form).send().await?;
    let body: TokenResponse = resp.json().await?;
    if let Some(e) = body.error {
        return Err(OAuthError::Provider(format!(
            "{}: {}",
            e,
            body.error_description.unwrap_or_default()
        )));
    }
    Ok(OAuthTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        token_type: body.token_type,
        expires_in_secs: body.expires_in,
        scope: body.scope,
    })
}

/// Wola revoke_url providera (jesli zadeklarowany). Bezpieczny no-op gdy brak URL.
pub async fn revoke_token(
    cfg: &DbAddonOAuthConfig,
    decl: &DbAddonOAuthProviderDecl,
    client_secret: &str,
    token: &str,
) -> std::result::Result<(), OAuthError> {
    let Some(url) = decl.revoke_url.as_deref() else {
        return Ok(());
    };
    let form: Vec<(&str, String)> = vec![
        ("token", token.to_string()),
        ("client_id", cfg.client_id.clone()),
        ("client_secret", client_secret.to_string()),
    ];
    let client = http_client();
    let resp = client.post(url).form(&form).send().await?;
    if !resp.status().is_success() {
        return Err(OAuthError::Provider(format!(
            "revoke HTTP {}",
            resp.status()
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
struct MicrosoftMe {
    id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "userPrincipalName")]
    upn: Option<String>,
    mail: Option<String>,
}

#[derive(Deserialize)]
struct GoogleUserInfo {
    sub: Option<String>,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    id: Option<u64>,
    login: Option<String>,
    name: Option<String>,
}

/// Pobiera (external_account_id, display_name) dla znanych providerow:
/// microsoft (Graph `/v1.0/me`), google (`/oauth2/v3/userinfo`), github (`/user`).
/// Dla nieznanych providerow zwraca pusty display_name i fallback "external".
pub async fn fetch_userinfo(
    provider_id: &str,
    access_token: &str,
) -> std::result::Result<(String, String), OAuthError> {
    let client = http_client();
    match provider_id {
        "microsoft" => {
            let resp = client
                .get("https://graph.microsoft.com/v1.0/me")
                .bearer_auth(access_token)
                .send()
                .await?;
            let me: MicrosoftMe = resp.json().await?;
            let id = me.id.unwrap_or_default();
            let name = me
                .display_name
                .or(me.upn)
                .or(me.mail)
                .unwrap_or_else(|| id.clone());
            Ok((id, name))
        }
        "google" => {
            let resp = client
                .get("https://openidconnect.googleapis.com/v1/userinfo")
                .bearer_auth(access_token)
                .send()
                .await?;
            let u: GoogleUserInfo = resp.json().await?;
            let id = u.sub.unwrap_or_default();
            let name = u.name.or(u.email).unwrap_or_else(|| id.clone());
            Ok((id, name))
        }
        "github" => {
            let resp = client
                .get("https://api.github.com/user")
                .bearer_auth(access_token)
                .header("User-Agent", "tentaflow")
                .send()
                .await?;
            let u: GithubUser = resp.json().await?;
            let id = u.id.map(|v| v.to_string()).unwrap_or_default();
            let name = u.name.or(u.login).unwrap_or_else(|| id.clone());
            Ok((id, name))
        }
        _ => Ok((String::new(), String::new())),
    }
}

// =============================================================================
// Blocking variants for use from sync contexts (WASM host functions).
// Use reqwest::blocking::Client to avoid bridging into a tokio runtime from
// inside the WASM guest call. Timeouts match the async client.
// =============================================================================

fn blocking_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest blocking client builder")
}

/// Blocking refresh - same semantics as [`refresh_token`], for sync callers.
pub fn refresh_token_blocking(
    cfg: &DbAddonOAuthConfig,
    decl: &DbAddonOAuthProviderDecl,
    client_secret: &str,
    refresh_token: &str,
) -> std::result::Result<OAuthTokens, OAuthError> {
    let form: Vec<(&str, String)> = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", cfg.client_id.clone()),
        ("client_secret", client_secret.to_string()),
    ];
    let client = blocking_http_client();
    let resp = client.post(&decl.token_url).form(&form).send()?;
    let body: TokenResponse = resp.json()?;
    if let Some(e) = body.error {
        return Err(OAuthError::Provider(format!(
            "{}: {}",
            e,
            body.error_description.unwrap_or_default()
        )));
    }
    Ok(OAuthTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        token_type: body.token_type,
        expires_in_secs: body.expires_in,
        scope: body.scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_challenge_matches_spec() {
        // RFC 7636 przyklad: verifier "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        // => challenge "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let c = code_challenge_from_verifier(v);
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let cfg = DbAddonOAuthConfig {
            addon_id: "a".into(),
            provider_id: "microsoft".into(),
            client_id: "cid".into(),
            client_secret_encrypted: None,
            redirect_uri: "https://x/cb".into(),
            enabled: true,
            updated_at: "".into(),
            oauth_mode: "individual".into(),
        };
        let decl = DbAddonOAuthProviderDecl {
            addon_id: "a".into(),
            provider_id: "microsoft".into(),
            display_name: "".into(),
            authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "".into(),
            revoke_url: None,
            scopes: "User.Read offline_access".into(),
            mode: "individual".into(),
            pkce: true,
        };
        let url = build_authorize_url(&cfg, &decl, "STATE123", Some("CH"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("state=STATE123"));
        assert!(url.contains("code_challenge=CH"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn test_build_authorize_url_contains_client_id_state_scopes_pkce() {
        // Arrange
        let cfg = DbAddonOAuthConfig {
            addon_id: "teams".into(),
            provider_id: "microsoft".into(),
            client_id: "CID-ABC".into(),
            client_secret_encrypted: None,
            redirect_uri: "https://host/cb".into(),
            enabled: true,
            updated_at: "".into(),
            oauth_mode: "individual".into(),
        };
        let decl = DbAddonOAuthProviderDecl {
            addon_id: "teams".into(),
            provider_id: "microsoft".into(),
            display_name: "MS".into(),
            authorize_url: "https://login.microsoftonline.com/authorize".into(),
            token_url: "".into(),
            revoke_url: None,
            scopes: "User.Read,offline_access".into(),
            mode: "individual".into(),
            pkce: true,
        };

        // Act
        let url = build_authorize_url(&cfg, &decl, "s-RAND", Some("CH-XYZ"));

        // Assert — wszystkie wymagane parametry obecne
        assert!(url.contains("client_id=CID-ABC"), "client_id: {}", url);
        assert!(url.contains("state=s-RAND"), "state: {}", url);
        // Scopes rozdzielone po enkodowaniu (przecinek → spacja → %20)
        assert!(
            url.contains("scope=User.Read%20offline_access"),
            "scope: {}",
            url
        );
        assert!(
            url.contains("code_challenge=CH-XYZ"),
            "code_challenge: {}",
            url
        );
        assert!(
            url.contains("code_challenge_method=S256"),
            "method: {}",
            url
        );
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn test_generate_state_is_random_and_has_minimum_entropy() {
        // Act
        let s1 = generate_state();
        let s2 = generate_state();

        // Assert
        assert_ne!(s1, s2, "kazde wywolanie musi dawac inny state");
        assert!(s1.len() >= 24, "state ma min. 24 znaki (jest {})", s1.len());
        assert!(s2.len() >= 24);
    }

    #[test]
    fn test_pkce_verifier_challenge_pair_matches_s256() {
        // Arrange
        let verifier = generate_code_verifier();

        // Act
        let challenge = code_challenge_from_verifier(&verifier);

        // Assert — rucznie policzone sha256+base64url (bez padding) musi sie zgadzac
        use base64::Engine;
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(
            challenge, expected,
            "challenge musi byc base64url(sha256(verifier))"
        );
        // Challenge nie zawiera paddingu
        assert!(!challenge.contains('='), "no padding w S256 challenge");
    }
}
