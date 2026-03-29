// =============================================================================
// Plik: crypto/mod.rs
// Opis: Modul szyfrowania sekretow AES-256-GCM z HKDF key derivation.
//       Zapewnia szyfrowanie/deszyfrowanie wartosci z automatyczna detekcja
//       zaszyfrowanych danych na podstawie prefixu "enc:".
// Przyklad: SecretsCipher::new(hex_key)?.encrypt("tajne_haslo")
// =============================================================================

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

/// Stala sol dla HKDF - zapobiega atakom z precomputed tables
const HKDF_SALT: &[u8] = b"tentaflow-v1";

/// Prefix zaszyfrowanych wartosci - jednoznaczna identyfikacja bez heurystyki
const ENCRYPTED_PREFIX: &str = "enc:";

/// Dekoduje 64-znakowy hex string na tablice 32 bajtow
fn decode_hex_32(hex_str: &str) -> anyhow::Result<[u8; 32]> {
    if hex_str.len() != 64 {
        anyhow::bail!("Oczekiwano 64 znaki hex (32 bajty), otrzymano {}", hex_str.len());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_str[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow::anyhow!("Nieprawidlowy hex: {}", e))?;
    }
    Ok(out)
}

/// Generuje losowy 32-bajtowy klucz jako hex string (VULN-025: OsRng zamiast thread_rng)
pub fn generate_master_key() -> String {
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let mut hex = String::with_capacity(64);
    for b in &key {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", b);
    }
    key.zeroize();
    hex
}

pub struct SecretsCipher {
    cipher: Aes256Gcm,
}

impl SecretsCipher {
    /// Tworzy nowy cipher z master key (hex string 64 znaki = 32 bajty)
    pub fn new(master_key_hex: &str) -> anyhow::Result<Self> {
        let master_key = Zeroizing::new(
            decode_hex_32(master_key_hex)
                .map_err(|e| anyhow::anyhow!("Nieprawidlowy master key hex: {}", e))?
        );

        // HKDF-SHA256 key derivation z sol
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), master_key.as_ref());
        let mut derived_key = Zeroizing::new([0u8; 32]);
        hk.expand(b"tentaflow-secrets-v1", derived_key.as_mut())
            .map_err(|e| anyhow::anyhow!("HKDF expand error: {}", e))?;

        let cipher = Aes256Gcm::new_from_slice(derived_key.as_ref())
            .map_err(|e| anyhow::anyhow!("AES init error: {}", e))?;

        Ok(Self { cipher })
    }

    /// Szyfruje plaintext -> "enc:" + base64(nonce_12B || ciphertext || tag_16B)
    /// VULN-025: OsRng zamiast thread_rng dla materialu kryptograficznego
    pub fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        // nonce (12B) || ciphertext+tag
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        let mut result = String::with_capacity(4 + ((12 + ciphertext.len()) * 4 / 3 + 4));
        result.push_str(ENCRYPTED_PREFIX);
        B64.encode_string(&combined, &mut result);
        Ok(result)
    }

    /// Deszyfruje "enc:" + base64(nonce_12B || ciphertext || tag_16B) -> plaintext
    pub fn decrypt(&self, encrypted_value: &str) -> anyhow::Result<String> {
        let b64_part = encrypted_value.strip_prefix(ENCRYPTED_PREFIX)
            .unwrap_or(encrypted_value);

        let decoded = B64
            .decode(b64_part)
            .map_err(|e| anyhow::anyhow!("Nieprawidlowy base64: {}", e))?;

        if decoded.len() < 28 {
            anyhow::bail!("Zaszyfrowane dane za krotkie (min 28 bajtow)");
        }

        let (nonce_bytes, ciphertext) = decoded.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext_bytes = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Blad deszyfrowania: {}", e))?;

        match String::from_utf8(plaintext_bytes) {
            Ok(s) => Ok(s),
            Err(e) => {
                let mut bytes = e.into_bytes();
                bytes.zeroize();
                anyhow::bail!("Odszyfrowane dane nie sa UTF-8")
            }
        }
    }

    /// Sprawdza czy wartosc jest zaszyfrowana na podstawie prefixu "enc:"
    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with(ENCRYPTED_PREFIX)
    }

    /// Odszyfruj jesli zaszyfrowane, w przeciwnym razie zwroc oryginalna wartosc
    pub fn decrypt_if_encrypted<'a>(&self, value: &'a str) -> std::borrow::Cow<'a, str> {
        if value.is_empty() || value == "***" {
            return std::borrow::Cow::Borrowed(value);
        }
        if !Self::is_encrypted(value) {
            return std::borrow::Cow::Borrowed(value);
        }
        match self.decrypt(value) {
            Ok(plaintext) => std::borrow::Cow::Owned(plaintext),
            Err(e) => {
                tracing::warn!("Deszyfrowanie nie powiodlo sie dla wartosci z prefixem enc: - {}", e);
                std::borrow::Cow::Borrowed(value)
            }
        }
    }
}

/// Hashuje haslo uzytkownika algorytmem argon2 (PHC string format)
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    use rand::rngs::OsRng;

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Blad hashowania hasla: {}", e))?;
    Ok(hash.to_string())
}

/// Weryfikuje haslo uzytkownika z zapisanym hashem argon2
pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::{Argon2, PasswordVerifier, PasswordHash};

    let Ok(parsed_hash) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}
