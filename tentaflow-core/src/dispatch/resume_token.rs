// =============================================================================
// Plik: dispatch/resume_token.rs
// Opis: Resume token system dla subscription resume po reconnect (Task #38 +
//       design doc Subscription streaming z resume tokens). Server wystawia
//       HMAC-SHA256 token przy IS_STREAM_END jesli subscription byla aktywna;
//       klient po reconnect moze wyslac SubscribeResume z tokenem zeby
//       drainowac brakujace chunki z SQLite recorder buffer.
//
//       Token format (raw bytes, base64 url-safe na wire):
//         [16] subscription_id (u128 LE)
//         [8]  last_sequence (u64 LE)
//         [8]  expires_at_epoch (u64 LE)
//         [32] HMAC-SHA256 nad pierwszymi 32 bajtami
//       = 64 bajty total, base64 = ~88 znakow.
//
//       Sekret HMAC z `jwt_secret` w db settings (reuse), zeby nie wymagac
//       dodatkowej rotacji kluczy. Token wygasa po 5 minutach (resume musi
//       byc szybkie — dluzsza disconnect = client zaczyna od nowa).
// =============================================================================

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Maksymalny czas zycia resume tokenu (sekundy). Klient ktory disconnects
/// dluzej niz to powinien rebuild stream from scratch.
pub const TOKEN_TTL_SECS: u64 = 300;

/// Rozmiar payloadu (bez podpisu) w bajtach.
const PAYLOAD_LEN: usize = 16 + 8 + 8;
/// Rozmiar pelnego tokenu (payload + HMAC-SHA256 = 32 bajty).
const TOKEN_LEN: usize = PAYLOAD_LEN + 32;

#[derive(Debug)]
pub enum ResumeError {
    InvalidLength,
    Expired,
    SignatureMismatch,
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidLength => write!(f, "invalid resume token length"),
            ResumeError::Expired => write!(f, "resume token expired"),
            ResumeError::SignatureMismatch => write!(f, "resume token signature mismatch"),
        }
    }
}
impl std::error::Error for ResumeError {}

/// Decoded resume token. Stworzone przez `verify`, wystawione przez `issue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeToken {
    pub subscription_id: u128,
    pub last_sequence: u64,
    pub expires_at_epoch: u64,
}

/// Wystawia podpisany resume token. Sekret = bytes z jwt_secret (lub innego
/// stable per-server secret).
pub fn issue(subscription_id: u128, last_sequence: u64, secret: &[u8]) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_at = now + TOKEN_TTL_SECS;

    let mut buf = Vec::with_capacity(TOKEN_LEN);
    buf.extend_from_slice(&subscription_id.to_le_bytes());
    buf.extend_from_slice(&last_sequence.to_le_bytes());
    buf.extend_from_slice(&expires_at.to_le_bytes());

    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&buf);
    let sig = mac.finalize().into_bytes();
    buf.extend_from_slice(&sig);
    buf
}

/// Weryfikuje token — sprawdza HMAC i czas wygasniecia.
pub fn verify(token_bytes: &[u8], secret: &[u8]) -> Result<ResumeToken, ResumeError> {
    if token_bytes.len() != TOKEN_LEN {
        return Err(ResumeError::InvalidLength);
    }

    let (payload, sig) = token_bytes.split_at(PAYLOAD_LEN);
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(payload);
    if mac.verify_slice(sig).is_err() {
        return Err(ResumeError::SignatureMismatch);
    }

    let mut sub_bytes = [0u8; 16];
    sub_bytes.copy_from_slice(&payload[0..16]);
    let mut seq_bytes = [0u8; 8];
    seq_bytes.copy_from_slice(&payload[16..24]);
    let mut exp_bytes = [0u8; 8];
    exp_bytes.copy_from_slice(&payload[24..32]);

    let expires_at = u64::from_le_bytes(exp_bytes);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now >= expires_at {
        return Err(ResumeError::Expired);
    }

    Ok(ResumeToken {
        subscription_id: u128::from_le_bytes(sub_bytes),
        last_sequence: u64::from_le_bytes(seq_bytes),
        expires_at_epoch: expires_at,
    })
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &[u8] = b"test-secret-do-not-use-in-prod";

    #[test]
    fn issue_and_verify_round_trip() {
        let token = issue(0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0u128, 42, TEST_SECRET);
        assert_eq!(token.len(), TOKEN_LEN);
        let decoded = verify(&token, TEST_SECRET).unwrap();
        assert_eq!(decoded.subscription_id, 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0u128);
        assert_eq!(decoded.last_sequence, 42);
    }

    #[test]
    fn wrong_secret_fails_verify() {
        let token = issue(1, 1, TEST_SECRET);
        let result = verify(&token, b"different-secret");
        assert!(matches!(result, Err(ResumeError::SignatureMismatch)));
    }

    #[test]
    fn truncated_token_fails_verify() {
        let token = issue(1, 1, TEST_SECRET);
        let result = verify(&token[..32], TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::InvalidLength)));
    }

    #[test]
    fn tampered_sequence_fails_verify() {
        let mut token = issue(1, 1, TEST_SECRET);
        // Modyfikujemy last_sequence (bajty 16..24) bez aktualizacji HMAC.
        token[16] = token[16].wrapping_add(1);
        let result = verify(&token, TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::SignatureMismatch)));
    }

    #[test]
    fn manual_expired_token_rejected() {
        // Stworzmy token z manual expires_at w przeszlosci.
        let mut buf = Vec::with_capacity(TOKEN_LEN);
        buf.extend_from_slice(&1u128.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes());
        // expires_at = 1000 (rok 1970, na pewno expired)
        buf.extend_from_slice(&1000u64.to_le_bytes());
        let mut mac = HmacSha256::new_from_slice(TEST_SECRET).unwrap();
        mac.update(&buf);
        let sig = mac.finalize().into_bytes();
        buf.extend_from_slice(&sig);

        let result = verify(&buf, TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::Expired)));
    }

    #[test]
    fn token_carries_subscription_id_and_sequence() {
        let token = issue(0x12345, 999, TEST_SECRET);
        let decoded = verify(&token, TEST_SECRET).unwrap();
        assert_eq!(decoded.subscription_id, 0x12345);
        assert_eq!(decoded.last_sequence, 999);
        // expires_at_epoch should be ~now + 300
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(decoded.expires_at_epoch >= now + TOKEN_TTL_SECS - 5);
        assert!(decoded.expires_at_epoch <= now + TOKEN_TTL_SECS + 5);
    }
}
