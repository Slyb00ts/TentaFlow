// =============================================================================
// Plik: addon/signature.rs
// Opis: Weryfikacja podpisow Ed25519 nad bundlami `[[ui_component]]`.
//       Algorytm: SHA-256(bundle_bytes) → 32-byte digest → Ed25519 verify
//       z `publisher.ed25519_public_key`. Klucz wydawcy musi byc w trust
//       store (`trusted_publishers`, migracja v26) — w przeciwnym razie
//       install jest odrzucany (default-deny). Brak fallbacku / soft mode:
//       kazdy nie-OK przypadek = blad twardy.
// =============================================================================

use std::path::Path;

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH};
use sha2::{Digest, Sha256};

use crate::db::DbPool;

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("publisher key not in trust store: {0}")]
    UntrustedPublisher(String),

    #[error("invalid public key format (expected 32-byte Ed25519 base64): {0}")]
    InvalidPublicKey(String),

    #[error("invalid signature format (expected Ed25519 base64): {0}")]
    InvalidSignatureFormat(String),

    #[error("signature verify failed")]
    SignatureVerifyFailed,

    #[error("bundle file unreadable: {0}")]
    BundleIoError(#[from] std::io::Error),

    #[error("bundle empty")]
    BundleEmpty,

    #[error("trust store query failed: {0}")]
    TrustStoreError(String),
}

/// Verifies a single `[[ui_component]]` bundle against publisher key + sig.
///
/// Steps (any failure stops the chain and returns a typed error):
/// 1. trust store lookup — `publisher_pk_b64` must be a row in `trusted_publishers`
/// 2. decode pk: standard base64 → 32 bytes
/// 3. decode signature (strip optional `ed25519:` prefix) → 64 bytes
/// 4. read bundle, compute SHA-256(bytes), verify Ed25519 over the digest
pub fn verify_ui_component_bundle(
    bundle_path: &Path,
    publisher_pk_b64: &str,
    signature_b64: &str,
    pool: &DbPool,
) -> Result<(), SignatureError> {
    if !is_publisher_trusted(pool, publisher_pk_b64)? {
        return Err(SignatureError::UntrustedPublisher(
            publisher_pk_b64.to_string(),
        ));
    }

    let pk_bytes = base64::engine::general_purpose::STANDARD
        .decode(publisher_pk_b64.as_bytes())
        .map_err(|e| SignatureError::InvalidPublicKey(e.to_string()))?;
    if pk_bytes.len() != PUBLIC_KEY_LENGTH {
        return Err(SignatureError::InvalidPublicKey(format!(
            "expected {} bytes, got {}",
            PUBLIC_KEY_LENGTH,
            pk_bytes.len()
        )));
    }
    let mut pk_arr = [0u8; PUBLIC_KEY_LENGTH];
    pk_arr.copy_from_slice(&pk_bytes);
    let pk = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| SignatureError::InvalidPublicKey(e.to_string()))?;

    let sig_b64_clean = signature_b64
        .strip_prefix("ed25519:")
        .unwrap_or(signature_b64);
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64_clean.as_bytes())
        .map_err(|e| SignatureError::InvalidSignatureFormat(e.to_string()))?;
    if sig_bytes.len() != SIGNATURE_LENGTH {
        return Err(SignatureError::InvalidSignatureFormat(format!(
            "expected {} bytes, got {}",
            SIGNATURE_LENGTH,
            sig_bytes.len()
        )));
    }
    let mut sig_arr = [0u8; SIGNATURE_LENGTH];
    sig_arr.copy_from_slice(&sig_bytes);
    let sig = Signature::from_bytes(&sig_arr);

    let bundle_bytes = std::fs::read(bundle_path)?;
    if bundle_bytes.is_empty() {
        return Err(SignatureError::BundleEmpty);
    }
    let mut hasher = Sha256::new();
    hasher.update(&bundle_bytes);
    let digest = hasher.finalize();

    pk.verify(digest.as_slice(), &sig)
        .map_err(|_| SignatureError::SignatureVerifyFailed)?;

    Ok(())
}

/// Trust store query (light wrapper over repository — kept here so the
/// signature module owns its single source of "is this key trusted?").
pub fn is_publisher_trusted(pool: &DbPool, key_b64: &str) -> Result<bool, SignatureError> {
    let conn = pool
        .lock()
        .map_err(|e| SignatureError::TrustStoreError(format!("lock poisoned: {e}")))?;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM trusted_publishers WHERE key_b64 = ?1",
            rusqlite::params![key_b64],
            |row| row.get(0),
        )
        .map_err(|e| SignatureError::TrustStoreError(e.to_string()))?;
    Ok(count > 0)
}

/// Short, log-safe form of a publisher key (first 8 chars). Used in audit
/// details so the full key never leaks into log files but operators can
/// still correlate denials with `trusted_publishers` rows.
pub fn truncate_pk_for_audit(pk_b64: &str) -> String {
    pk_b64.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

    // Deterministic key derivation for tests — XOR a counter into a fixed
    // seed so each call yields a distinct keypair without pulling in an
    // RNG version that may not match the vendored ed25519-dalek's
    // `rand_core` interface. Pure-bytes path avoids any RNG trait mismatch.
    fn keypair(tag: u8) -> SigningKey {
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(0x9d).wrapping_add(tag);
        }
        SigningKey::from_bytes(&seed)
    }

    fn make_pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        Arc::new(Mutex::new(conn))
    }

    fn trust(pool: &DbPool, pk_b64: &str) {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO trusted_publishers (key_b64, label, added_at) VALUES (?1, 'test', '2026-01-01T00:00:00Z')",
            rusqlite::params![pk_b64],
        )
        .expect("insert trust");
    }

    fn pk_b64(sk: &SigningKey) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
    }

    fn sign_bundle(sk: &SigningKey, bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        let digest = h.finalize();
        let sig = sk.sign(digest.as_slice());
        base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
    }

    fn write_bundle(bytes: &[u8]) -> NamedTempFile {
        use std::io::Write;
        let mut f = NamedTempFile::new().expect("tmp");
        f.write_all(bytes).expect("write");
        f.flush().expect("flush");
        f
    }

    #[test]
    fn verify_valid_signature() {
        let pool = make_pool();
        let sk = keypair(1);
        let pk = pk_b64(&sk);
        trust(&pool, &pk);
        let bundle = b"console.log('hello addon');";
        let sig = sign_bundle(&sk, bundle);
        let f = write_bundle(bundle);
        verify_ui_component_bundle(f.path(), &pk, &sig, &pool).expect("verify ok");
    }

    #[test]
    fn verify_valid_signature_with_prefix() {
        let pool = make_pool();
        let sk = keypair(2);
        let pk = pk_b64(&sk);
        trust(&pool, &pk);
        let bundle = b"x";
        let sig = format!("ed25519:{}", sign_bundle(&sk, bundle));
        let f = write_bundle(bundle);
        verify_ui_component_bundle(f.path(), &pk, &sig, &pool).expect("verify ok with prefix");
    }

    #[test]
    fn verify_rejects_wrong_signature() {
        let pool = make_pool();
        let sk_a = keypair(10);
        let sk_b = keypair(20);
        let pk_a = pk_b64(&sk_a);
        trust(&pool, &pk_a);
        let bundle = b"payload";
        let sig_b = sign_bundle(&sk_b, bundle); // signed with B, verify against A
        let f = write_bundle(bundle);
        match verify_ui_component_bundle(f.path(), &pk_a, &sig_b, &pool) {
            Err(SignatureError::SignatureVerifyFailed) => {}
            other => panic!("expected SignatureVerifyFailed, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_tampered_bundle() {
        let pool = make_pool();
        let sk = keypair(2);
        let pk = pk_b64(&sk);
        trust(&pool, &pk);
        let original = b"original-content";
        let sig = sign_bundle(&sk, original);
        let f = write_bundle(b"TAMPERED-content");
        match verify_ui_component_bundle(f.path(), &pk, &sig, &pool) {
            Err(SignatureError::SignatureVerifyFailed) => {}
            other => panic!("expected SignatureVerifyFailed, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_untrusted_publisher() {
        let pool = make_pool();
        let sk = keypair(2);
        let pk = pk_b64(&sk);
        // intentionally NOT trusted
        let bundle = b"data";
        let sig = sign_bundle(&sk, bundle);
        let f = write_bundle(bundle);
        match verify_ui_component_bundle(f.path(), &pk, &sig, &pool) {
            Err(SignatureError::UntrustedPublisher(k)) => assert_eq!(k, pk),
            other => panic!("expected UntrustedPublisher, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_invalid_pk_format() {
        let pool = make_pool();
        let pk = "not-base64!!!";
        trust(&pool, pk);
        let bundle = b"x";
        let f = write_bundle(bundle);
        match verify_ui_component_bundle(f.path(), pk, "AA==", &pool) {
            Err(SignatureError::InvalidPublicKey(_)) => {}
            other => panic!("expected InvalidPublicKey, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_invalid_sig_format() {
        let pool = make_pool();
        let sk = keypair(2);
        let pk = pk_b64(&sk);
        trust(&pool, &pk);
        let bundle = b"x";
        let f = write_bundle(bundle);
        match verify_ui_component_bundle(f.path(), &pk, "@@@not-base64@@@", &pool) {
            Err(SignatureError::InvalidSignatureFormat(_)) => {}
            other => panic!("expected InvalidSignatureFormat, got {other:?}"),
        }
    }

    #[test]
    fn verify_rejects_empty_bundle() {
        let pool = make_pool();
        let sk = keypair(2);
        let pk = pk_b64(&sk);
        trust(&pool, &pk);
        let sig = sign_bundle(&sk, &[]);
        let f = write_bundle(&[]);
        match verify_ui_component_bundle(f.path(), &pk, &sig, &pool) {
            Err(SignatureError::BundleEmpty) => {}
            other => panic!("expected BundleEmpty, got {other:?}"),
        }
    }
}
