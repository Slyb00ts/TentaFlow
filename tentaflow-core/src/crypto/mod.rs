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

// =============================================================================
// SettingsCipher — szyfrowanie sekretow w tabeli settings
// Master key z pliku na dysku (~/.tentaflow/master.key)
// =============================================================================

const MASTER_KEY_PATH: &str = ".tentaflow/master.key";

/// Lista kluczy settings ktore MUSZA byc szyfrowane
const ENCRYPTED_SETTING_KEYS: &[&str] = &[
    "jwt_secret",
    "encryption_master_key",
    "node_private_key",
    "node_x25519_private_key",
    "ngc_api_key",
];

/// Okresla sciezke do master key — priorytet: custom_dir, home_dir, data_dir
pub fn master_key_path(custom_dir: Option<&std::path::Path>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(dir) = custom_dir {
        return Ok(dir.join("master.key"));
    }
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(MASTER_KEY_PATH));
    }
    if let Some(data) = dirs::data_dir() {
        return Ok(data.join("tentaflow").join("master.key"));
    }
    anyhow::bail!("Nie mozna okreslic katalogu na master key (brak home_dir i data_dir)")
}

/// Laduje master key z pliku lub generuje nowy
/// custom_dir: opcjonalny katalog (mobile/desktop podaja swoj data_dir)
pub fn load_or_create_master_key_in(custom_dir: Option<&std::path::Path>) -> anyhow::Result<[u8; 32]> {
    let key_path = master_key_path(custom_dir)?;

    if key_path.exists() {
        let key_hex = std::fs::read_to_string(&key_path)?;
        let trimmed = key_hex.trim();
        if trimmed.len() != 64 {
            anyhow::bail!(
                "Master key ma nieprawidlowa dlugosc: {} znakow hex (oczekiwano 64)",
                trimmed.len()
            );
        }
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&trimmed[i * 2..i * 2 + 2], 16)
                .map_err(|e| anyhow::anyhow!("Nieprawidlowy hex w master key: {}", e))?;
        }
        Ok(key)
    } else {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);

        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let hex_str = key.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        std::fs::write(&key_path, &hex_str)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        #[cfg(windows)]
        {
            restrict_file_acl_windows(&key_path);
        }

        tracing::info!("Wygenerowano nowy master key: {}", key_path.display());
        Ok(key)
    }
}

/// Wrapper do backward compat — uzywa domyslnego katalogu
pub fn load_or_create_master_key() -> anyhow::Result<[u8; 32]> {
    load_or_create_master_key_in(None)
}

/// Windows: ogranicza ACL pliku do tylko biezacego uzytkownika
#[cfg(windows)]
fn restrict_file_acl_windows(path: &std::path::Path) {
    use std::process::Command;
    let path_str = path.to_string_lossy();
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "CURRENT_USER".to_string());
    let _ = Command::new("icacls")
        .args([&*path_str, "/inheritance:r", "/grant:r", &format!("{}:F", username)])
        .output();
}

pub struct SettingsCipher {
    cipher: Aes256Gcm,
}

impl SettingsCipher {
    /// Tworzy cipher z master key (32 bajty z pliku na dysku).
    /// Derywuje oddzielny klucz AES-256 przez HKDF-SHA256.
    pub fn new(master_key: &[u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(b"tentaflow-settings-v1"), master_key);
        let mut derived = Zeroizing::new([0u8; 32]);
        hk.expand(b"settings-encryption", derived.as_mut())
            .expect("HKDF expand");

        Self {
            cipher: Aes256Gcm::new_from_slice(derived.as_ref()).expect("AES key"),
        }
    }

    /// Czy ten klucz settings powinien byc szyfrowany
    pub fn should_encrypt(key: &str) -> bool {
        ENCRYPTED_SETTING_KEYS.contains(&key)
            || key.contains("_key")
            || key.contains("_secret")
            || key.contains("_token")
            || key.contains("_password")
            || key.contains("api_key")
    }

    /// Szyfruj wartosc. Zwraca "enc:base64(nonce||ciphertext)"
    pub fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        let mut result = String::with_capacity(4 + ((12 + ciphertext.len()) * 4 / 3 + 4));
        result.push_str(ENCRYPTED_PREFIX);
        B64.encode_string(&combined, &mut result);
        Ok(result)
    }

    /// Deszyfruj wartosc. Akceptuje "enc:..." lub plaintext (backward compat dla migracji)
    pub fn decrypt(&self, stored: &str) -> anyhow::Result<String> {
        if !stored.starts_with(ENCRYPTED_PREFIX) {
            return Ok(stored.to_string());
        }

        let encoded = &stored[4..];
        let combined = B64
            .decode(encoded)
            .map_err(|e| anyhow::anyhow!("Nieprawidlowy base64: {}", e))?;

        if combined.len() < 28 {
            anyhow::bail!("Zaszyfrowana wartosc za krotka (min 28 bajtow)");
        }

        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext_bytes = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Blad deszyfrowania: {}", e))?;

        Ok(String::from_utf8(plaintext_bytes)?)
    }
}

/// Migruje istniejace plaintext sekrety do zaszyfrowanej formy
pub fn migrate_plaintext_secrets(
    pool: &crate::db::DbPool,
    cipher: &SettingsCipher,
) -> anyhow::Result<u32> {
    let mut migrated = 0;
    for key in ENCRYPTED_SETTING_KEYS {
        if let Some(val) = crate::db::repository::get_setting(pool, key)? {
            if !val.starts_with(ENCRYPTED_PREFIX) {
                let encrypted = cipher.encrypt(&val)?;
                crate::db::repository::set_setting(pool, key, &encrypted)?;
                migrated += 1;
                tracing::info!("Zaszyfrowano setting: {}", key);
            }
        }
    }
    Ok(migrated)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_cipher_encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let cipher = SettingsCipher::new(&key);

        let plaintext = "super-tajny-klucz-jwt-12345";
        let encrypted = cipher.encrypt(plaintext).unwrap();

        assert!(encrypted.starts_with("enc:"));
        assert_ne!(encrypted, plaintext);

        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn settings_cipher_plaintext_passthrough() {
        let cipher = SettingsCipher::new(&[0u8; 32]);
        let plaintext = "wartosc-bez-prefiksu";
        let result = cipher.decrypt(plaintext).unwrap();
        assert_eq!(result, plaintext);
    }

    #[test]
    fn settings_cipher_should_encrypt_known_keys() {
        assert!(SettingsCipher::should_encrypt("jwt_secret"));
        assert!(SettingsCipher::should_encrypt("encryption_master_key"));
        assert!(SettingsCipher::should_encrypt("node_private_key"));
        assert!(SettingsCipher::should_encrypt("node_x25519_private_key"));
        assert!(SettingsCipher::should_encrypt("ngc_api_key"));
    }

    #[test]
    fn settings_cipher_should_encrypt_pattern_matching() {
        assert!(SettingsCipher::should_encrypt("custom_api_key"));
        assert!(SettingsCipher::should_encrypt("some_secret"));
        assert!(SettingsCipher::should_encrypt("auth_token"));
        assert!(SettingsCipher::should_encrypt("db_password"));
        assert!(!SettingsCipher::should_encrypt("jwt_expiry_hours"));
        assert!(!SettingsCipher::should_encrypt("flow_engine_enabled"));
    }

    #[test]
    fn settings_cipher_different_keys_produce_different_ciphertexts() {
        let cipher1 = SettingsCipher::new(&[1u8; 32]);
        let cipher2 = SettingsCipher::new(&[2u8; 32]);

        let encrypted1 = cipher1.encrypt("test").unwrap();
        let encrypted2 = cipher2.encrypt("test").unwrap();

        assert_ne!(encrypted1, encrypted2);

        // cipher1 nie moze odszyfrowac danych z cipher2
        assert!(cipher1.decrypt(&encrypted2).is_err());
    }

    #[test]
    fn settings_cipher_each_encryption_unique() {
        let cipher = SettingsCipher::new(&[0u8; 32]);
        let enc1 = cipher.encrypt("test").unwrap();
        let enc2 = cipher.encrypt("test").unwrap();
        assert_ne!(enc1, enc2); // Rozne nonce = rozny ciphertext
    }

    #[test]
    fn migrate_plaintext_secrets_works() {
        use std::sync::{Arc, Mutex};

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        ).unwrap();

        let pool: crate::db::DbPool = Arc::new(Mutex::new(conn));

        // Wstaw plaintext sekrety
        crate::db::repository::set_setting(&pool, "jwt_secret", "abcdef123456").unwrap();
        crate::db::repository::set_setting(&pool, "node_private_key", "deadbeef").unwrap();
        crate::db::repository::set_setting(&pool, "jwt_expiry_hours", "24").unwrap();

        let cipher = SettingsCipher::new(&[99u8; 32]);
        let migrated = migrate_plaintext_secrets(&pool, &cipher).unwrap();
        assert_eq!(migrated, 2); // jwt_secret + node_private_key

        // Sprawdz ze sa zaszyfrowane
        let jwt_raw = crate::db::repository::get_setting(&pool, "jwt_secret").unwrap().unwrap();
        assert!(jwt_raw.starts_with("enc:"));

        // Sprawdz odczyt przez get_setting_secure
        let jwt_decrypted = crate::db::repository::get_setting_secure(&pool, "jwt_secret", &cipher)
            .unwrap()
            .unwrap();
        assert_eq!(jwt_decrypted, "abcdef123456");

        // Ponowna migracja nie powinna nic robic
        let migrated2 = migrate_plaintext_secrets(&pool, &cipher).unwrap();
        assert_eq!(migrated2, 0);

        // jwt_expiry_hours nie powinno byc zmienione
        let hours = crate::db::repository::get_setting(&pool, "jwt_expiry_hours").unwrap().unwrap();
        assert_eq!(hours, "24");
    }
}
