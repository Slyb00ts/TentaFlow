// =============================================================================
// Plik: dispatch/resume_token.rs
// Opis: Resume token system dla subscription resume po reconnect.
//       Server wystawia HMAC-SHA256 token przy IS_STREAM_END jesli subscription
//       byla aktywna; klient po reconnect moze wyslac SubscribeResume z tokenem
//       zeby drainowac brakujace chunki z SQLite recorder buffer.
//
//       Token format v2 (raw bytes, base64 url-safe na wire):
//         [16] subscription_id (u128 LE)
//         [8]  last_sequence (u64 LE)
//         [8]  expires_at_epoch (u64 LE)
//         [16] originating_user_id ([u8; 16]) — P0 FIX: prevents cross-user replay
//         [32] HMAC-SHA256 nad pierwszymi 48 bajtami
//       = 80 bajtow total, base64 = ~108 znakow.
//
//       SECURITY (P0 fix 2026-04-18):
//       Token JEST zwiazany z user_id ktory rozpoczynal stream. Verify wymaga
//       expected_user_id parametru i odrzuca jesli mismatch — to zapobiega
//       cross-user stream theft jesli token wycieknie (XSS, log, MITM bez TLS).
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

/// Rozmiar payloadu (bez podpisu) w bajtach. v2 (P0 fix): + 16 bajtow user_id.
const PAYLOAD_LEN: usize = 16 + 8 + 8 + 16;
/// Rozmiar pelnego tokenu (payload + HMAC-SHA256 = 32 bajty).
const TOKEN_LEN: usize = PAYLOAD_LEN + 32;

#[derive(Debug)]
pub enum ResumeError {
    InvalidLength,
    Expired,
    SignatureMismatch,
    /// P0 fix: caller's session user_id != token's originating_user_id.
    UserMismatch,
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidLength => write!(f, "invalid resume token length"),
            ResumeError::Expired => write!(f, "resume token expired"),
            ResumeError::SignatureMismatch => write!(f, "resume token signature mismatch"),
            ResumeError::UserMismatch => write!(f, "resume token belongs to different user"),
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
    /// P0 fix: user_id ktory rozpoczynal subscription. Verify sprawdza ze
    /// caller ma matching user_id w swojej sesji.
    pub originating_user_id: [u8; 16],
}

/// Wystawia podpisany resume token. Sekret = bytes z jwt_secret (lub innego
/// stable per-server secret). P0 fix: token jest zwiazany z user_id —
/// musi byc wywolane z user_id ktory rozpoczynal subscription.
pub fn issue(
    subscription_id: u128,
    last_sequence: u64,
    originating_user_id: [u8; 16],
    secret: &[u8],
) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_at = now + TOKEN_TTL_SECS;

    let mut buf = Vec::with_capacity(TOKEN_LEN);
    buf.extend_from_slice(&subscription_id.to_le_bytes());
    buf.extend_from_slice(&last_sequence.to_le_bytes());
    buf.extend_from_slice(&expires_at.to_le_bytes());
    buf.extend_from_slice(&originating_user_id);

    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&buf);
    let sig = mac.finalize().into_bytes();
    buf.extend_from_slice(&sig);
    buf
}

/// Weryfikuje token — sprawdza HMAC, czas wygasniecia I user_id binding (P0).
/// `expected_user_id` musi pochodzic z aktualnej sesji caller'a (ws_binary
/// dispatch ctx). Jesli token byl wystawiony dla innego usera = `UserMismatch`.
pub fn verify(
    token_bytes: &[u8],
    expected_user_id: &[u8; 16],
    secret: &[u8],
) -> Result<ResumeToken, ResumeError> {
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
    let mut user_bytes = [0u8; 16];
    user_bytes.copy_from_slice(&payload[32..48]);

    // P0 FIX: user binding check (constant-time compare zeby nie wyciekac
    // ktore bajty matchuja przez timing).
    if !constant_time_eq(&user_bytes, expected_user_id) {
        return Err(ResumeError::UserMismatch);
    }

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
        originating_user_id: user_bytes,
    })
}

/// Constant-time bajty equality — zapobiega timing oracle atakom przy weryfikacji
/// user_id. `subtle` crate byloby idiomatyczne ale dla 16 bajtow ten 4-liner
/// wystarcza i nie wymaga nowej zaleznosci.
fn constant_time_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &[u8] = b"test-secret-do-not-use-in-prod";
    const ALICE: [u8; 16] = [0xAA; 16];
    const BOB: [u8; 16] = [0xBB; 16];

    #[test]
    fn issue_and_verify_round_trip() {
        let token = issue(0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0u128, 42, ALICE, TEST_SECRET);
        assert_eq!(token.len(), TOKEN_LEN);
        let decoded = verify(&token, &ALICE, TEST_SECRET).unwrap();
        assert_eq!(decoded.subscription_id, 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0u128);
        assert_eq!(decoded.last_sequence, 42);
        assert_eq!(decoded.originating_user_id, ALICE);
    }

    #[test]
    fn wrong_secret_fails_verify() {
        let token = issue(1, 1, ALICE, TEST_SECRET);
        let result = verify(&token, &ALICE, b"different-secret");
        assert!(matches!(result, Err(ResumeError::SignatureMismatch)));
    }

    #[test]
    fn truncated_token_fails_verify() {
        let token = issue(1, 1, ALICE, TEST_SECRET);
        let result = verify(&token[..32], &ALICE, TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::InvalidLength)));
    }

    #[test]
    fn tampered_sequence_fails_verify() {
        let mut token = issue(1, 1, ALICE, TEST_SECRET);
        // Modyfikujemy last_sequence (bajty 16..24) bez aktualizacji HMAC.
        token[16] = token[16].wrapping_add(1);
        let result = verify(&token, &ALICE, TEST_SECRET);
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
        buf.extend_from_slice(&ALICE);
        let mut mac = HmacSha256::new_from_slice(TEST_SECRET).unwrap();
        mac.update(&buf);
        let sig = mac.finalize().into_bytes();
        buf.extend_from_slice(&sig);

        // Token bedzie odrzucony jeszcze PRZED expiry check przez user_mismatch
        // (tu signature OK + user OK = drugi check expiry).
        let result = verify(&buf, &ALICE, TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::Expired)));
    }

    #[test]
    fn token_carries_subscription_id_and_sequence() {
        let token = issue(0x12345, 999, ALICE, TEST_SECRET);
        let decoded = verify(&token, &ALICE, TEST_SECRET).unwrap();
        assert_eq!(decoded.subscription_id, 0x12345);
        assert_eq!(decoded.last_sequence, 999);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(decoded.expires_at_epoch >= now + TOKEN_TTL_SECS - 5);
        assert!(decoded.expires_at_epoch <= now + TOKEN_TTL_SECS + 5);
    }

    #[test]
    fn p0_cross_user_replay_rejected() {
        // Alice's token, Bob tries to use it.
        let alice_token = issue(0x42, 7, ALICE, TEST_SECRET);
        let result = verify(&alice_token, &BOB, TEST_SECRET);
        assert!(
            matches!(result, Err(ResumeError::UserMismatch)),
            "P0 fix: cross-user token must be rejected, got {:?}",
            result
        );
    }

    #[test]
    fn p0_same_user_succeeds() {
        let token = issue(0x42, 7, ALICE, TEST_SECRET);
        let result = verify(&token, &ALICE, TEST_SECRET);
        assert!(result.is_ok(), "P0: same user must succeed, got {:?}", result);
    }

    #[test]
    fn p0_tampered_user_id_fails_signature() {
        let mut token = issue(0x42, 7, ALICE, TEST_SECRET);
        // Zmien user_id w payloadzie (bajty 32..48) bez aktualizacji HMAC.
        token[32] = 0xCC;
        // Verify z BOB (zeby pass user check) odrzuci na signature.
        let mut bob_after_tamper = [0u8; 16];
        bob_after_tamper[0] = 0xCC;
        bob_after_tamper[1..].copy_from_slice(&ALICE[1..]);
        let result = verify(&token, &bob_after_tamper, TEST_SECRET);
        assert!(matches!(result, Err(ResumeError::SignatureMismatch)));
    }

    #[test]
    fn constant_time_eq_works() {
        let a = [0u8; 16];
        let b = [0u8; 16];
        assert!(constant_time_eq(&a, &b));
        let mut c = [0u8; 16];
        c[0] = 1;
        assert!(!constant_time_eq(&a, &c));
        let mut d = [0u8; 16];
        d[15] = 1;
        assert!(!constant_time_eq(&a, &d));
    }
}
