// =============================================================================
// Plik: api/dashboard/auth.rs
// Opis: Hashowanie hasel argon2, hashowanie kluczy API SHA256, JWT.
// =============================================================================

use anyhow::Result;
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use hmac::{Hmac, KeyInit, Mac};
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
    pub password_change_only: bool,
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

/// Identifier of a pepper, stored with every key issued under it: the first
/// 8 bytes of SHA-256 over a domain tag and the pepper, in hex. It names the
/// pepper without revealing it, so a key is verified against the pepper it was
/// issued under and no other.
pub fn pepper_id(pepper: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(b"tentaflow api-key pepper id\0");
    hasher.update(pepper);
    hex_lower(&hasher.finalize()[..8])
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// API key verifier = hex(HMAC-SHA256(pepper, token)), over the whole token as
/// issued. The pepper is an org-wide secret (see `get_or_create_api_key_pepper`),
/// stored encrypted and replicated only as a sync shared secret (re-encrypted
/// per node); a plain DB dump without the master key cannot forge keys.
/// INVARIANT: the pepper is identical across all nodes within an org so a
/// replicated key verifies everywhere — a joiner adopts the org pepper from the
/// baseline before issuing or verifying keys. The verifier stays this bare MAC
/// so a node that predates pepper ids computes the same value for the same
/// token and a key issued on a newer node verifies there too.
pub fn api_key_verifier(token: &str, pepper: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(pepper).expect("HMAC accepts keys of any length");
    mac.update(token.as_bytes());
    hex_lower(&mac.finalize().into_bytes())
}

/// Length of the pepper id inside a token, in hex characters.
const PEPPER_ID_HEX_LEN: usize = 16;

/// `sk-<pepper id>-<random hex>`: the token names the pepper it was issued
/// under, so verification is one MAC and one lookup, however many peppers the
/// node keeps. Tokens issued before ids existed are `sk-<random hex>`.
pub fn issue_api_key_token(random_hex: &str, pepper: &[u8]) -> String {
    format!("sk-{}-{}", pepper_id(pepper), random_hex)
}

/// The pepper id a token names, if it carries one. A legacy token is `sk-`
/// plus hex and has no `-` after its first 16 characters.
fn named_pepper_id(token: &str) -> Option<&str> {
    let rest = token.strip_prefix("sk-")?;
    let id = rest.get(..PEPPER_ID_HEX_LEN)?;
    (rest.as_bytes().get(PEPPER_ID_HEX_LEN) == Some(&b'-')
        && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(id)
}

/// Looks an API key up under the pepper it names (one MAC and one lookup): the
/// current one or one the fleet retired, so a key keeps working after the fleet
/// converged on another pepper. A key without an id (issued before ids existed)
/// is tried under the current pepper, then under the retired ones.
pub fn verify_api_key_any_pepper(
    pool: &crate::db::DbPool,
    cipher: &crate::crypto::SettingsCipher,
    token: &str,
    current_pepper: &[u8],
) -> anyhow::Result<Option<crate::db::models::DbApiKey>> {
    let named = named_pepper_id(token);
    // A token that names the current pepper needs no list of retired ones.
    if named.is_some_and(|id| id == pepper_id(current_pepper)) {
        return crate::db::repository::verify_api_key(
            pool,
            &api_key_verifier(token, current_pepper),
        );
    }
    let retired = crate::db::repository::retired_api_key_peppers(pool, cipher)?;
    let peppers: Vec<&[u8]> = std::iter::once(current_pepper)
        .chain(retired.iter().map(Vec::as_slice))
        .collect();
    if let Some(id) = named {
        return match peppers.iter().find(|pepper| pepper_id(pepper) == id) {
            Some(pepper) => {
                crate::db::repository::verify_api_key(pool, &api_key_verifier(token, pepper))
            }
            None => Ok(None),
        };
    }
    for pepper in &peppers {
        if let Some(row) =
            crate::db::repository::verify_api_key(pool, &api_key_verifier(token, pepper))?
        {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// Generuje token JWT dla uzytkownika.
/// VULN-004: Token NIE zawiera flagi is_admin — sprawdzane w DB przy kazdym requeście.
pub fn generate_jwt(
    user_id: &str,
    username: &str,
    secret: &str,
    expiry_hours: i64,
    password_change_only: bool,
) -> Result<String> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(expiry_hours))
        .ok_or_else(|| anyhow::anyhow!("Blad obliczania czasu wygasniecia tokenu"))?
        .timestamp() as usize;

    let claims = Claims {
        sub: username.to_string(),
        user_id: user_id.to_string(),
        password_change_only,
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
