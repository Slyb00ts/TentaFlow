// =============================================================================
// Plik: api/dashboard/auth.rs
// Opis: Hashowanie hasel argon2, hashowanie kluczy API SHA256, JWT.
// =============================================================================

use anyhow::Result;
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use hmac::{Hmac, Mac};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Dane zawarte w tokenie JWT
/// VULN-004: is_admin USUNIETY z JWT — zawsze sprawdzaj w DB (Zero Trust).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Nazwa uzytkownika
    pub sub: String,
    /// Identyfikator uzytkownika w bazie (UUID `user_accounts.id`)
    pub user_id: String,
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

/// API key verifier = hex(HMAC-SHA256(pepper, token)). The pepper is an org-wide
/// secret (see `get_or_create_api_key_pepper`), stored encrypted and replicated
/// only as a sync shared secret (re-encrypted per node); a plain DB dump without
/// the master key cannot forge keys. INVARIANT: the pepper is identical across
/// all nodes within an org so a replicated key verifies everywhere — a joiner
/// adopts the org pepper from the baseline before issuing or verifying keys.
pub fn api_key_verifier(token: &str, pepper: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(pepper).expect("HMAC accepts keys of any length");
    mac.update(token.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Generuje token JWT dla uzytkownika.
/// VULN-004: Token NIE zawiera flagi is_admin — sprawdzane w DB przy kazdym requeście.
pub fn generate_jwt(
    user_id: &str,
    username: &str,
    secret: &str,
    expiry_hours: i64,
) -> Result<String> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(expiry_hours))
        .ok_or_else(|| anyhow::anyhow!("Blad obliczania czasu wygasniecia tokenu"))?
        .timestamp() as usize;

    let claims = Claims {
        sub: username.to_string(),
        user_id: user_id.to_string(),
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
