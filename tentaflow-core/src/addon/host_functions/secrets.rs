// =============================================================================
// Plik: addon/host_functions/secrets.rs
// Opis: Host functions Secrets API — szyfrowane sekrety per addon per user.
//       Sekrety sa przechowywane w DB zaszyfrowane AES-256-GCM.
// Uprawnienia: "secrets" (get/set). Fail-closed — brak uprawnienia blokuje
//              dostep zanim klucz szyfrujacy zostanie derivowany. Scoping do
//              (addon_id, user_id) wymuszany przez zapytania DB.
// =============================================================================

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use sha2::Sha256;
use tracing::{info, warn};

use super::{
    audit_log, check_permission, get_memory, read_guest_bytes, read_guest_string,
    write_guest_output, AddonState, WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION,
    ABI_ERR_PERMISSION, ABI_OK,
};
use crate::db;

/// Staly klucz derivacji — w produkcji powinien byc z konfiguracji/HSM
/// Uzywa HKDF do generowania klucza per addon
const SECRET_KEY_SALT: &[u8] = b"tentaflow-addon-secrets-v1";

// =============================================================================
// secret_get — pobranie sekretu
// =============================================================================

/// Host function: pobiera odszyfrowany sekret.
///
/// ABI:
/// - key_ptr/key_len: nazwa sekretu
/// - out_ptr/out_cap: bufor na wartosc
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK, ABI_ERR_NOT_FOUND lub kod bledu
pub fn secret_get(
    mut caller: WasmCaller<'_, AddonState>,
    key_ptr: i32,
    key_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let key = match read_guest_string(&memory, &caller, key_ptr, key_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Sprawdz uprawnienie secrets (ro)
    if !check_permission(caller.data(), "secrets", None) {
        audit_log(
            caller.data(),
            "secret.get",
            Some("secrets"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let user_id = caller.data().user_id;

    // Pobierz zaszyfrowany sekret z DB
    let secret_data: Option<(Vec<u8>, Vec<u8>)> = {
        match caller.data().db.lock() {
            Ok(conn) => {
                // Probuj sekret per-user, potem globalny
                conn.query_row(
                    "SELECT encrypted_value, nonce FROM addon_secrets \
                     WHERE addon_id = ?1 AND (user_id = ?2 OR user_id IS NULL) AND secret_key = ?3 \
                     ORDER BY user_id DESC LIMIT 1",
                    rusqlite::params![&addon_id, user_id, &key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok()
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    let (encrypted_value, nonce_bytes) = match secret_data {
        Some(data) => data,
        None => {
            audit_log(
                caller.data(),
                "secret.get",
                Some("secrets"),
                Some(&key),
                "ok",
                Some("not found"),
            );
            return ABI_ERR_NOT_FOUND;
        }
    };

    // Odszyfruj wartosc — VULN-022: brak master key = blad
    let decryption_key = match derive_key(&addon_id, caller.data()) {
        Some(k) => k,
        None => {
            audit_log(
                caller.data(),
                "secret.get",
                Some("secrets"),
                Some(&key),
                "error",
                Some("Brak encryption_master_key"),
            );
            return ABI_ERR_OPERATION;
        }
    };
    let cipher = Aes256Gcm::new_from_slice(&decryption_key).unwrap();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let decrypted = match cipher.decrypt(nonce, encrypted_value.as_ref()) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("Blad deszyfrowania: {}", e);
            warn!("secret_get: {}", msg);
            audit_log(
                caller.data(),
                "secret.get",
                Some("secrets"),
                Some(&key),
                "error",
                Some(&msg),
            );
            return ABI_ERR_OPERATION;
        }
    };

    audit_log(
        caller.data(),
        "secret.get",
        Some("secrets"),
        Some(&key),
        "ok",
        None,
    );

    write_guest_output(
        &memory,
        &mut caller,
        out_ptr,
        out_cap,
        out_len_ptr,
        &decrypted,
    )
}

// =============================================================================
// secret_set — zapisanie sekretu
// =============================================================================

/// Host function: zapisuje zaszyfrowany sekret.
///
/// ABI:
/// - key_ptr/key_len: nazwa sekretu
/// - value_ptr/value_len: wartosc (tekst lub bajty)
/// - Zwraca: ABI_OK lub kod bledu
pub fn secret_set(
    mut caller: WasmCaller<'_, AddonState>,
    key_ptr: i32,
    key_len: i32,
    value_ptr: i32,
    value_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let key = match read_guest_string(&memory, &caller, key_ptr, key_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let value = match read_guest_bytes(&memory, &caller, value_ptr, value_len) {
        Some(b) => b.to_vec(),
        None => return ABI_ERR_OPERATION,
    };

    // Sprawdz uprawnienie secrets (rw)
    if !check_permission(caller.data(), "secrets", None) {
        audit_log(
            caller.data(),
            "secret.set",
            Some("secrets"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let user_id = caller.data().user_id;

    // Zaszyfruj wartosc — VULN-022: brak master key = blad
    let encryption_key = match derive_key(&addon_id, caller.data()) {
        Some(k) => k,
        None => {
            audit_log(
                caller.data(),
                "secret.set",
                Some("secrets"),
                Some(&key),
                "error",
                Some("Brak encryption_master_key"),
            );
            return ABI_ERR_OPERATION;
        }
    };
    let cipher = Aes256Gcm::new_from_slice(&encryption_key).unwrap();

    // Generuj losowy nonce (12 bajtow)
    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let encrypted = match cipher.encrypt(nonce, value.as_ref()) {
        Ok(e) => e,
        Err(e) => {
            let msg = format!("Blad szyfrowania: {}", e);
            warn!("secret_set: {}", msg);
            audit_log(
                caller.data(),
                "secret.set",
                Some("secrets"),
                Some(&key),
                "error",
                Some(&msg),
            );
            return ABI_ERR_OPERATION;
        }
    };

    // Zapisz w DB
    let result = {
        match caller.data().db.lock() {
            Ok(conn) => conn.execute(
                "INSERT OR REPLACE INTO addon_secrets \
                     (addon_id, user_id, secret_key, encrypted_value, nonce, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
                rusqlite::params![&addon_id, user_id, &key, &encrypted, &nonce_bytes[..]],
            ),
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    match result {
        Ok(_) => {
            info!("secret_set: addon='{}', key='{}'", addon_id, key);
            audit_log(
                caller.data(),
                "secret.set",
                Some("secrets"),
                Some(&key),
                "ok",
                None,
            );
            ABI_OK
        }
        Err(e) => {
            let msg = e.to_string();
            audit_log(
                caller.data(),
                "secret.set",
                Some("secrets"),
                Some(&key),
                "error",
                Some(&msg),
            );
            ABI_ERR_OPERATION
        }
    }
}

// =============================================================================
// Funkcje pomocnicze
// =============================================================================

/// Derywuje klucz AES-256 z master key i addon_id za pomoca HKDF-SHA256.
/// Master key pochodzi z ustawien DB (encryption_master_key).
/// addon_id jest uzywany jako info w fazie expand — kazdy addon ma unikalny klucz.
/// Zwraca None jesli brak encryption_master_key w konfiguracji.
fn derive_key(addon_id: &str, state: &AddonState) -> Option<[u8; 32]> {
    use hkdf::Hkdf;

    // VULN-022: Brak fallbacku — jesli nie ma master key, zwracamy None
    let master_key = match db::repository::get_setting_secure(
        &state.db,
        "encryption_master_key",
        &state.settings_cipher,
    ) {
        Ok(Some(key)) if !key.is_empty() => key,
        _ => {
            tracing::error!(
                "Brak encryption_master_key w ustawieniach — operacje na sekretach zablokowane"
            );
            return None;
        }
    };

    let hk = Hkdf::<Sha256>::new(Some(SECRET_KEY_SALT), master_key.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(format!("addon:{}:secrets", addon_id).as_bytes(), &mut key)
        .expect("HKDF expand nie powinien zawodzic z 32 bajtami");
    Some(key)
}
