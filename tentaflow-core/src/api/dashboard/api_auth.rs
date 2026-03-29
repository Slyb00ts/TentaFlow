// =============================================================================
// Plik: api/dashboard/api_auth.rs
// Opis: Endpointy logowania i informacji o zalogowanym uzytkowniku.
// =============================================================================

use crate::db::{self, DbPool};
use super::auth::{self, Claims};
use anyhow::Result;
use parking_lot::Mutex;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{info, warn};

// =============================================================================
// VULN-002: Rate limiter na endpoint logowania
// =============================================================================

/// Prosty in-memory rate limiter z dwoma progami: per username i per IP.
/// VULN-035: Dual rate limiting — username (10/min) i IP (30/min globalnie).
pub struct LoginRateLimiter {
    attempts: Mutex<HashMap<String, Vec<Instant>>>,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Sprawdza czy podany klucz moze wykonac probe logowania z podanym limitem.
    /// VULN-020: Zwraca true jesli dozwolone.
    /// VULN-032: Czyszczenie pustych wpisow i awaryjne czyszczenie mapy przy > 10000 kluczy (ochrona przed OOM).
    pub fn check_and_record(&self, key: &str, max_attempts: usize) -> bool {
        let mut map = self.attempts.lock();
        let now = Instant::now();

        // VULN-032: Awaryjne czyszczenie — lepsze niz OOM
        if map.len() > 10000 {
            map.clear();
        }

        let rate_key = key.to_string();
        let attempts = map.entry(rate_key.clone()).or_default();

        // Usun stare wpisy (starsze niz 60s)
        attempts.retain(|t| now.duration_since(*t).as_secs() < 60);

        // VULN-032: Jesli po czyszczeniu Vec jest pusty — usun klucz z mapy
        if attempts.is_empty() {
            map.remove(&rate_key);
            // Pierwszy request po czyszczeniu — wstaw ponownie i zezwol
            map.entry(key.to_string()).or_default().push(now);
            return true;
        }

        if attempts.len() >= max_attempts {
            return false; // Zablokowany — za duzo prob
        }

        attempts.push(now);
        true
    }
}

lazy_static::lazy_static! {
    /// Globalny rate limiter dla endpointu logowania
    static ref LOGIN_RATE_LIMITER: LoginRateLimiter = LoginRateLimiter::new();
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub username: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub must_change_password: bool,
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub user_id: i64,
    pub username: String,
}

/// POST /api/auth/login - logowanie, zwraca token JWT.
/// VULN-002: Rate limiting — max 10 prob na 60 sekund per username.
/// VULN-035: Dual rate limiting — dodatkowy limit 30/min per IP.
pub fn handle_login(pool: &DbPool, body: &[u8], remote_addr: &str) -> Result<(u16, String)> {
    let req: LoginRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return Ok((400, json_error("Niepoprawny format danych logowania"))),
    };

    // VULN-035: Sprawdz rate limit per IP (30/min) — przed weryfikacja hasla
    if !LOGIN_RATE_LIMITER.check_and_record(&format!("ip:{}", remote_addr), 30) {
        warn!("Rate limit logowania (IP): remote_addr={}", remote_addr);
        return Ok((429, json_error("Zbyt wiele prob logowania z tego adresu. Sprobuj ponownie za minute.")));
    }

    // VULN-002: Sprawdz rate limit per username (10/min)
    if !LOGIN_RATE_LIMITER.check_and_record(&req.username, 10) {
        // VULN-013: Logowanie zdarzen bezpieczenstwa — rate limit
        warn!("Rate limit logowania: username={}", req.username);
        return Ok((429, json_error("Zbyt wiele prob logowania. Sprobuj ponownie za minute.")));
    }

    // Weryfikacja logowania w tabeli user_accounts
    // VULN-003: Sprawdz must_change_password z user_accounts
    let (user_id, username, needs_password_change) =
        if let Some(ua) = db::repository::verify_user_account_password(pool, &req.username, &req.password)? {
            (ua.id, ua.username, ua.must_change_password)
        } else {
            // VULN-013: Logowanie zdarzen bezpieczenstwa — nieudane logowanie
            warn!("Nieudana proba logowania: username={}", req.username);
            return Ok((401, json_error("Niepoprawna nazwa uzytkownika lub haslo")));
        };

    // VULN-013: Logowanie zdarzen bezpieczenstwa — udane logowanie
    info!("Uzytkownik zalogowany: username={}, user_id={}", username, user_id);

    // Pobierz jwt_secret z ustawien; jesli brak — wygeneruj i zapisz
    let jwt_secret = match db::repository::get_setting(pool, "jwt_secret")? {
        Some(s) if !s.is_empty() => s,
        _ => {
            // VULN-009: Kryptograficznie bezpieczny secret z OsRng
            let mut key = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            let generated = hex::encode(key);
            db::repository::set_setting(pool, "jwt_secret", &generated)?;
            generated
        }
    };
    let expiry_hours: i64 = db::repository::get_setting(pool, "jwt_expiry_hours")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);

    // VULN-004: Token JWT NIE zawiera is_admin — sprawdzane w DB przy kazdym requeście
    let token = auth::generate_jwt(user_id, &username, &jwt_secret, expiry_hours)?;

    let response = LoginResponse {
        token,
        username,
        must_change_password: needs_password_change,
    };

    Ok((200, serde_json::to_string(&response)?))
}

/// GET /api/auth/me - informacje o zalogowanym uzytkowniku z Claims
pub fn handle_me(claims: &Claims) -> Result<(u16, String)> {
    let response = MeResponse {
        user_id: claims.user_id,
        username: claims.sub.clone(),
    };

    Ok((200, serde_json::to_string(&response)?))
}

/// POST /api/auth/change-password - zmiana hasla
pub fn handle_change_password(pool: &DbPool, claims: &Claims, body: &[u8]) -> Result<(u16, String)> {
    let req: ChangePasswordRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let user = db::repository::get_user_by_username(pool, &claims.sub)?
        .ok_or_else(|| anyhow::anyhow!("Uzytkownik nie istnieje"))?;

    if !auth::verify_password(&req.current_password, &user.password_hash) {
        return Ok((401, json_error("Niepoprawne aktualne haslo")));
    }

    // VULN-011: Ujednolicenie minimalnej dlugosci hasla — 8 znakow
    if req.new_password.len() < 8 {
        return Ok((400, json_error("Nowe haslo musi miec minimum 8 znakow")));
    }

    let new_hash = auth::hash_password(&req.new_password)?;
    db::repository::update_user_password(pool, user.id, &new_hash)?;
    db::repository::clear_must_change_password(pool, user.id)?;

    // VULN-013: Logowanie zdarzen bezpieczenstwa — zmiana hasla
    info!("Zmiana hasla: user_id={}", claims.user_id);

    Ok((200, r#"{"message":"Haslo zmienione pomyslnie"}"#.to_string()))
}

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}
