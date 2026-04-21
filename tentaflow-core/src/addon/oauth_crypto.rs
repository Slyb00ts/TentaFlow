// =============================================================================
// Plik: addon/oauth_crypto.rs
// Opis: Szyfrowanie AES-256-GCM dla sekretow OAuth addonow (client_secret,
//       access_token, refresh_token). Master-key ladowany z modulu
//       `oauth_master_key` (env / plik multiplatformowy). Format blob:
//       nonce(12B) || ciphertext || tag(16B) — nonce losowy per operacja.
// Przyklad:
//   let key = ensure_master_key(&db)?;
//   let blob = encrypt(&key, b"secret")?;
//   let plain = decrypt(&key, &blob)?;
// =============================================================================

use crate::addon::oauth_master_key;
use crate::db::{repository, DbPool};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};

/// Legacy: nazwa klucza w `settings` (tylko do migracji; nowe instalacje nie zapisuja).
pub const MASTER_KEY_SETTING: &str = "addon_oauth_master_key";

/// Zwraca master-key OAuth. Pierwszy call dokonuje jednorazowej migracji z legacy
/// settings (jesli istnieje) — re-encryptuje bloby i kasuje stary wpis.
pub fn ensure_master_key(db: &DbPool) -> Result<[u8; 32]> {
    // Migracja legacy ⇒ nowy master-key (idempotentna — gdy legacy brak, no-op).
    migrate_legacy_if_present(db)?;
    oauth_master_key::load_or_init()
}

/// Szyfruje `plaintext` AES-256-GCM. Zwraca blob: nonce(12) || ciphertext || tag(16).
pub fn encrypt(master_key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(master_key).context("blad inicjalizacji AES-256-GCM")?;
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).expect("OS RNG fill_bytes");
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("aes-gcm encrypt: {}", e))?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Jednorazowa migracja: jesli w settings jest stary master-key, deszyfruje nim
/// wszystkie bloby OAuth, szyfruje nowym (z `oauth_master_key`), zapisuje z powrotem
/// i kasuje legacy setting. Idempotentna — nastepne wywolania sa no-op.
fn migrate_legacy_if_present(db: &DbPool) -> Result<()> {
    use base64::Engine;
    let Some(b64) = repository::get_setting(db, MASTER_KEY_SETTING)? else {
        return Ok(());
    };
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .context("legacy master-key: base64 decode")?;
    if raw.len() != 32 {
        // Zepsuty — skasuj i polegaj na nowym zrodle.
        repository::delete_setting(db, MASTER_KEY_SETTING)?;
        return Ok(());
    }
    let mut old_key = [0u8; 32];
    old_key.copy_from_slice(&raw);

    let new_key = oauth_master_key::load_or_init()?;
    if old_key == new_key {
        // Nic do re-encryptowania — po prostu usun legacy setting.
        repository::delete_setting(db, MASTER_KEY_SETTING)?;
        return Ok(());
    }

    // Re-encrypt client_secret w addon_oauth_config.
    let secrets = repository::list_all_oauth_config_secrets(db)?;
    let mut re_encrypted: u64 = 0;
    for (id, blob) in secrets {
        match decrypt(&old_key, &blob) {
            Ok(plain) => {
                let new_blob = encrypt(&new_key, &plain)?;
                repository::update_oauth_config_secret_blob(db, id, &new_blob)?;
                re_encrypted += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "Migracja master-key: pominieto wpis addon_oauth_config id={} (deszyfracja: {})",
                    id,
                    e
                );
            }
        }
    }
    // Re-encrypt access/refresh tokens w user_oauth_accounts.
    let tokens = repository::list_all_user_oauth_token_blobs(db)?;
    for (id, access, refresh) in tokens {
        let new_access = decrypt(&old_key, &access)
            .ok()
            .map(|plain| encrypt(&new_key, &plain))
            .transpose()?;
        let new_refresh = if let Some(rt) = refresh {
            decrypt(&old_key, &rt)
                .ok()
                .map(|plain| encrypt(&new_key, &plain))
                .transpose()?
        } else {
            None
        };
        if let Some(a) = new_access {
            repository::update_user_oauth_token_blobs(db, id, &a, new_refresh.as_deref())?;
            re_encrypted += 1;
        }
    }
    repository::delete_setting(db, MASTER_KEY_SETTING)?;
    tracing::info!(
        "Migracja master-key OAuth zakonczona: {} rekordow re-encrypted",
        re_encrypted
    );
    Ok(())
}

/// Deszyfruje blob utworzony przez `encrypt`. Zwraca plaintext lub blad gdy auth tag nie pasuje.
pub fn decrypt(master_key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 + 16 {
        anyhow::bail!("blob za krotki ({} bajtow)", blob.len());
    }
    let cipher = Aes256Gcm::new_from_slice(master_key).context("blad inicjalizacji AES-256-GCM")?;
    let nonce = Nonce::from_slice(&blob[..12]);
    let plaintext = cipher
        .decrypt(nonce, &blob[12..])
        .map_err(|e| anyhow::anyhow!("aes-gcm decrypt (bad key/tag): {}", e))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [7u8; 32];
        let blob = encrypt(&key, b"hello world").unwrap();
        let plain = decrypt(&key, &blob).unwrap();
        assert_eq!(plain, b"hello world");
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key_a = [1u8; 32];
        let key_b = [2u8; 32];
        let blob = encrypt(&key_a, b"sekret").unwrap();
        assert!(decrypt(&key_b, &blob).is_err());
    }

    #[test]
    fn decrypt_truncated_blob_fails() {
        assert!(decrypt(&[0u8; 32], &[0u8; 5]).is_err());
    }

    #[test]
    fn test_decrypt_invalid_tag_fails() {
        // Arrange — flipnij bit w ciphertext/tag, auth powinien zawiesc
        let key = [9u8; 32];
        let mut blob = encrypt(&key, b"ciasne dane").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;

        // Act + Assert
        assert!(
            decrypt(&key, &blob).is_err(),
            "zmodyfikowany tag musi zostac odrzucony"
        );
    }

    #[test]
    fn test_nonces_differ_between_encryptions() {
        // Arrange
        let key = [3u8; 32];
        let plaintext = b"idempotent input";

        // Act — dwa szyfrowania tego samego tekstu
        let blob_a = encrypt(&key, plaintext).unwrap();
        let blob_b = encrypt(&key, plaintext).unwrap();

        // Assert — nonce'y (pierwsze 12B) musza sie roznic
        assert_ne!(
            &blob_a[..12],
            &blob_b[..12],
            "nonce powinien byc losowy per wywolanie"
        );
        // I caly blob tez (bo AEAD zalezy od nonce)
        assert_ne!(blob_a, blob_b, "bloby musza sie roznic");
        // Ale oba deszyfrowalne do tego samego tekstu
        assert_eq!(decrypt(&key, &blob_a).unwrap(), plaintext);
        assert_eq!(decrypt(&key, &blob_b).unwrap(), plaintext);
    }

    #[test]
    fn test_master_key_persisted_in_settings() {
        // Arrange
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();

        // Act — dwie ekstrakcje master-keya z tej samej DB
        let k1 = ensure_master_key(&db).unwrap();
        let k2 = ensure_master_key(&db).unwrap();

        // Assert — drugi start dostaje ten sam klucz
        assert_eq!(k1, k2, "master-key musi byc persistentny w settings");
        // Sanity: klucz nie jest zerowy
        assert_ne!(k1, [0u8; 32]);

        // Oraz blob zaszyfrowany pierwszym kluczem deszyfruje sie drugim
        let blob = encrypt(&k1, b"persisted").unwrap();
        assert_eq!(decrypt(&k2, &blob).unwrap(), b"persisted");
    }
}
