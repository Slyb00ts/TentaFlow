// =============================================================================
// Plik: src/pairing.rs
// Opis: Kryterium (b) — Ed25519 + PIN pairing handshake. Cel: <5s do shared
//       secret. Bootstrap implementacja:
//         1. Klient i serwer mają Ed25519 keypairs.
//         2. Admin wpisuje 6-cyfrowy PIN po obu stronach.
//         3. PIN -> HKDF -> shared key dla pierwszego secured frame.
//         4. Wymiana podpisanych challenge-response (signature pokrywa PIN+nonce).
//         5. HKDF z material -> session key + nonce_seed.
//
//       Cala kryptografia jest zwykly Rust (no iroh dependency) — dziala tak
//       samo dla quinn. Jesli #22 = adopt iroh, glue nad iroh::Endpoint.
// =============================================================================

use anyhow::{anyhow, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::PIN_LENGTH;

/// Wynik pairing — shared session secret + obie public keys do trusted_keys.
pub struct PairingResult {
    pub session_secret: [u8; 32],
    pub local_pubkey: VerifyingKey,
    pub remote_pubkey: VerifyingKey,
}

/// Symuluje pairing handshake — bez sieci, tylko crypto round-trip:
///   client -> server: { client_pub, signature(PIN || nonce_a) }
///   server -> client: { server_pub, signature(PIN || nonce_a || nonce_b) }
///   wspolny session_key = HKDF(PIN, nonce_a || nonce_b, "tentaflow-pair-v1")
pub fn simulate_handshake(
    client_key: &SigningKey,
    server_key: &SigningKey,
    pin: &str,
    nonce_a: &[u8; 16],
    nonce_b: &[u8; 16],
) -> Result<PairingResult> {
    if pin.len() != PIN_LENGTH || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!("PIN must be {} ASCII digits", PIN_LENGTH));
    }

    // Krok 1: klient podpisuje PIN || nonce_a
    let mut msg_a = Vec::with_capacity(PIN_LENGTH + 16);
    msg_a.extend_from_slice(pin.as_bytes());
    msg_a.extend_from_slice(nonce_a);
    let sig_a: Signature = client_key.sign(&msg_a);

    // Server verify
    let client_pub = client_key.verifying_key();
    client_pub
        .verify(&msg_a, &sig_a)
        .map_err(|e| anyhow!("client sig verify failed: {}", e))?;

    // Krok 2: server podpisuje PIN || nonce_a || nonce_b
    let mut msg_b = Vec::with_capacity(PIN_LENGTH + 32);
    msg_b.extend_from_slice(pin.as_bytes());
    msg_b.extend_from_slice(nonce_a);
    msg_b.extend_from_slice(nonce_b);
    let sig_b: Signature = server_key.sign(&msg_b);

    // Client verify
    let server_pub = server_key.verifying_key();
    server_pub
        .verify(&msg_b, &sig_b)
        .map_err(|e| anyhow!("server sig verify failed: {}", e))?;

    // HKDF-derive session secret z PIN + nonces.
    let mut salt = Vec::with_capacity(32);
    salt.extend_from_slice(nonce_a);
    salt.extend_from_slice(nonce_b);
    let hk = Hkdf::<Sha256>::new(Some(&salt), pin.as_bytes());
    let mut session_secret = [0u8; 32];
    hk.expand(b"tentaflow-pair-v1", &mut session_secret)
        .map_err(|e| anyhow!("HKDF expand failed: {}", e))?;

    Ok(PairingResult {
        session_secret,
        local_pubkey: client_pub,
        remote_pubkey: server_pub,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;
    use std::time::Instant;

    fn random_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    #[test]
    fn handshake_succeeds_with_valid_pin() {
        let client = random_key();
        let server = random_key();
        let nonce_a = [11u8; 16];
        let nonce_b = [22u8; 16];

        let result = simulate_handshake(&client, &server, "123456", &nonce_a, &nonce_b);
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_ne!(r.session_secret, [0u8; 32]);
    }

    #[test]
    fn handshake_rejects_invalid_pin_format() {
        let client = random_key();
        let server = random_key();
        let nonce_a = [1u8; 16];
        let nonce_b = [2u8; 16];

        // Niepoprawna dlugosc.
        assert!(simulate_handshake(&client, &server, "12345", &nonce_a, &nonce_b).is_err());
        // Niepoprawne znaki.
        assert!(simulate_handshake(&client, &server, "12345a", &nonce_a, &nonce_b).is_err());
    }

    #[test]
    fn same_pin_and_nonces_yield_same_secret() {
        let c1 = random_key();
        let s1 = random_key();
        let c2 = SigningKey::from_bytes(&c1.to_bytes());
        let s2 = SigningKey::from_bytes(&s1.to_bytes());
        let nonce_a = [33u8; 16];
        let nonce_b = [44u8; 16];

        let r1 = simulate_handshake(&c1, &s1, "654321", &nonce_a, &nonce_b).unwrap();
        let r2 = simulate_handshake(&c2, &s2, "654321", &nonce_a, &nonce_b).unwrap();
        assert_eq!(r1.session_secret, r2.session_secret);
    }

    #[test]
    fn different_pin_yields_different_secret() {
        let client = random_key();
        let server = random_key();
        let nonce_a = [1u8; 16];
        let nonce_b = [2u8; 16];

        let r1 = simulate_handshake(&client, &server, "111111", &nonce_a, &nonce_b).unwrap();
        let r2 = simulate_handshake(&client, &server, "222222", &nonce_a, &nonce_b).unwrap();
        assert_ne!(r1.session_secret, r2.session_secret);
    }

    #[test]
    fn handshake_completes_under_5_seconds() {
        let client = random_key();
        let server = random_key();
        let nonce_a = [1u8; 16];
        let nonce_b = [2u8; 16];

        let start = Instant::now();
        let _ = simulate_handshake(&client, &server, "999999", &nonce_a, &nonce_b).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 5,
            "handshake took {:?} >= 5s budget",
            elapsed
        );
        // W praktyce <1ms (no network), ale assertion sprawdza ze nie ma
        // patologicznego case (np. infinite loop). Real <5s validation
        // wymaga real network test (deferowany).
    }
}
