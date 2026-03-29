// =============================================================================
// Plik: api/dashboard/auth.rs
// Opis: Hashowanie hasel argon2, hashowanie kluczy API SHA256, JWT.
// =============================================================================

use anyhow::Result;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng};
use argon2::Argon2;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Dane zawarte w tokenie JWT
/// VULN-004: is_admin USUNIETY z JWT — zawsze sprawdzaj w DB (Zero Trust).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Nazwa uzytkownika
    pub sub: String,
    /// Identyfikator uzytkownika w bazie
    pub user_id: i64,
    /// Czas wygasniecia (unix timestamp)
    pub exp: usize,
}

/// Tworzy argon2 hash z hasla uzytkownika (z losowym saltem)
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Blad hashowania hasla: {}", e))?;
    Ok(hash.to_string())
}

/// Weryfikuje haslo uzytkownika z zapisanym hashem argon2
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed_hash) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

/// SHA256 hex hash dla kluczy API (szybki, deterministyczny)
pub fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generuje token JWT dla uzytkownika.
/// VULN-004: Token NIE zawiera flagi is_admin — sprawdzane w DB przy kazdym requeście.
pub fn generate_jwt(user_id: i64, username: &str, secret: &str, expiry_hours: i64) -> Result<String> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(expiry_hours))
        .ok_or_else(|| anyhow::anyhow!("Blad obliczania czasu wygasniecia tokenu"))?
        .timestamp() as usize;

    let claims = Claims {
        sub: username.to_string(),
        user_id,
        exp: expiration,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?;

    Ok(token)
}

/// Waliduje token JWT i zwraca Claims
pub fn validate_jwt(token: &str, secret: &str) -> Result<Claims> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;

    Ok(token_data.claims)
}
