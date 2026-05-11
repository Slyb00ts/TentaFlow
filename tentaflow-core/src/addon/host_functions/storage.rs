// =============================================================================
// Plik: addon/host_functions/storage.rs
// Opis: Host functions Storage API — sandboxowany key-value store per addon.
//       Kazdy addon widzi tylko swoje dane, izolacja przez addon_id + instance_id.
// Uprawnienia: "storage" (get/set/delete/list). Fail-closed — brak uprawnienia
//              blokuje dostep do storage zanim dotknie DB. Scoping per addon_id
//              jest wymuszony przez zapytania SQL.
// =============================================================================

use super::{
    audit_log, check_permission, get_memory, read_guest_bytes, read_guest_string,
    write_guest_output, AddonState, WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION,
    ABI_ERR_PERMISSION, ABI_OK,
};

/// CR-009: Maksymalna dlugosc klucza storage (1024 bajtow)
const MAX_KEY_LENGTH: usize = 1024;

/// CR-009: Maksymalny rozmiar wartosci storage (1 MB)
const MAX_VALUE_SIZE: usize = 1_048_576;

/// CR-009: Maksymalna liczba kluczy per addon
const MAX_KEYS_PER_ADDON: i64 = 10_000;

// =============================================================================
// storage_get — pobranie wartosci po kluczu
// =============================================================================

/// Host function: pobiera wartosc z sandboxowanego storage addonu.
///
/// ABI:
/// - key_ptr/key_len: klucz (UTF-8)
/// - out_ptr/out_cap: bufor na wartosc
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK, ABI_ERR_NOT_FOUND lub kod bledu
pub fn storage_get(
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

    // CR-009: Walidacja dlugosci klucza
    if key.len() > MAX_KEY_LENGTH {
        audit_log(
            caller.data(),
            "storage.get",
            Some("storage"),
            Some(&key[..64]),
            "error",
            Some("klucz za dlugi (max 1024B)"),
        );
        return ABI_ERR_OPERATION;
    }

    // Sprawdz uprawnienie storage (ro)
    if !check_permission(caller.data(), "storage", None) {
        audit_log(
            caller.data(),
            "storage.get",
            Some("storage"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();

    // Pobierz z DB
    let value: Option<Vec<u8>> = {
        match caller.data().db.lock() {
            Ok(conn) => {
                // VULN-037: Strict per-instance isolation — bez fallback na instance_id IS NULL
                conn.query_row(
                    "SELECT storage_value FROM addon_storage \
                     WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                    rusqlite::params![&addon_id, &instance_id, &key],
                    |row| row.get(0),
                )
                .ok()
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    match value {
        Some(data) => {
            audit_log(
                caller.data(),
                "storage.get",
                Some("storage"),
                Some(&key),
                "ok",
                None,
            );
            write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &data)
        }
        None => {
            audit_log(
                caller.data(),
                "storage.get",
                Some("storage"),
                Some(&key),
                "ok",
                Some("not found"),
            );
            ABI_ERR_NOT_FOUND
        }
    }
}

// =============================================================================
// storage_set — zapisanie wartosci pod kluczem
// =============================================================================

/// Host function: zapisuje wartosc w sandboxowanym storage addonu.
///
/// ABI:
/// - key_ptr/key_len: klucz (UTF-8)
/// - value_ptr/value_len: wartosc (bajty)
/// - Zwraca: ABI_OK lub kod bledu
pub fn storage_set(
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

    // CR-009: Walidacja dlugosci klucza
    if key.len() > MAX_KEY_LENGTH {
        audit_log(
            caller.data(),
            "storage.set",
            Some("storage"),
            Some(&key[..64]),
            "error",
            Some("klucz za dlugi (max 1024B)"),
        );
        return ABI_ERR_OPERATION;
    }

    let value = match read_guest_bytes(&memory, &caller, value_ptr, value_len) {
        Some(b) => b.to_vec(),
        None => return ABI_ERR_OPERATION,
    };

    // CR-009: Walidacja rozmiaru wartosci
    if value.len() > MAX_VALUE_SIZE {
        audit_log(
            caller.data(),
            "storage.set",
            Some("storage"),
            Some(&key),
            "error",
            Some("wartosc za duza (max 1MB)"),
        );
        return ABI_ERR_OPERATION;
    }

    // Sprawdz uprawnienie storage (rw)
    if !check_permission(caller.data(), "storage", None) {
        audit_log(
            caller.data(),
            "storage.set",
            Some("storage"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();
    let value_size = value.len() as i64;

    // Sprawdz limit storage
    let within_limit = {
        match caller.data().db.lock() {
            Ok(conn) => {
                // Pobierz aktualny rozmiar storage addonu
                let current_size: i64 = conn.query_row(
                    "SELECT COALESCE(SUM(value_size_bytes), 0) FROM addon_storage WHERE addon_id = ?1",
                    rusqlite::params![&addon_id],
                    |row| row.get(0),
                ).unwrap_or(0);

                // Pobierz limit
                let limit_mb: i64 = conn
                    .query_row(
                        "SELECT storage_limit_mb FROM addon_resource_limits WHERE addon_id = ?1",
                        rusqlite::params![&addon_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(100);

                let limit_bytes = limit_mb * 1024 * 1024;
                current_size + value_size <= limit_bytes
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    if !within_limit {
        audit_log(
            caller.data(),
            "storage.set",
            Some("storage"),
            Some(&key),
            "error",
            Some("storage limit exceeded"),
        );
        return ABI_ERR_OPERATION;
    }

    // CR-009: Sprawdz limit liczby kluczy per addon
    let key_count_ok = {
        match caller.data().db.lock() {
            Ok(conn) => {
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM addon_storage WHERE addon_id = ?1",
                        rusqlite::params![&addon_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                count < MAX_KEYS_PER_ADDON
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    if !key_count_ok {
        audit_log(
            caller.data(),
            "storage.set",
            Some("storage"),
            Some(&key),
            "error",
            Some(&format!(
                "limit kluczy przekroczony (max {})",
                MAX_KEYS_PER_ADDON
            )),
        );
        return ABI_ERR_OPERATION;
    }

    // Zapisz w DB (INSERT OR REPLACE)
    let result = {
        match caller.data().db.lock() {
            Ok(conn) => {
                conn.execute(
                    "INSERT OR REPLACE INTO addon_storage \
                     (addon_id, instance_id, storage_key, storage_value, value_size_bytes, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
                    rusqlite::params![&addon_id, &instance_id, &key, &value, value_size],
                )
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    match result {
        Ok(_) => {
            audit_log(
                caller.data(),
                "storage.set",
                Some("storage"),
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
                "storage.set",
                Some("storage"),
                Some(&key),
                "error",
                Some(&msg),
            );
            ABI_ERR_OPERATION
        }
    }
}

// =============================================================================
// storage_delete — usuwanie klucza
// =============================================================================

/// Host function: usuwa klucz z sandboxowanego storage addonu.
///
/// ABI:
/// - key_ptr/key_len: klucz (UTF-8)
/// - Zwraca: ABI_OK lub kod bledu
pub fn storage_delete(mut caller: WasmCaller<'_, AddonState>, key_ptr: i32, key_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let key = match read_guest_string(&memory, &caller, key_ptr, key_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Sprawdz uprawnienie storage (rwd — delete wymaga 'd')
    if !check_permission(caller.data(), "storage", None) {
        audit_log(
            caller.data(),
            "storage.delete",
            Some("storage"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();

    let result = {
        match caller.data().db.lock() {
            Ok(conn) => {
                conn.execute(
                    "DELETE FROM addon_storage WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
                    rusqlite::params![&addon_id, &instance_id, &key],
                )
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    match result {
        Ok(_) => {
            audit_log(
                caller.data(),
                "storage.delete",
                Some("storage"),
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
                "storage.delete",
                Some("storage"),
                Some(&key),
                "error",
                Some(&msg),
            );
            ABI_ERR_OPERATION
        }
    }
}

// =============================================================================
// storage_list — lista kluczy z opcjonalnym prefixem
// =============================================================================

/// Host function: listuje klucze w storage addonu z opcjonalnym prefixem.
///
/// ABI:
/// - prefix_ptr/prefix_len: prefix filtrowania (0,0 = wszystkie)
/// - out_ptr/out_cap: bufor na JSON array of strings
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn storage_list(
    mut caller: WasmCaller<'_, AddonState>,
    prefix_ptr: i32,
    prefix_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let prefix = if prefix_ptr != 0 && prefix_len > 0 {
        read_guest_string(&memory, &caller, prefix_ptr, prefix_len).map(|s| s.to_string())
    } else {
        None
    };

    // Sprawdz uprawnienie storage (ro)
    if !check_permission(caller.data(), "storage", None) {
        audit_log(
            caller.data(),
            "storage.list",
            Some("storage"),
            prefix.as_deref(),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();

    let keys: Vec<String> = {
        let conn = match caller.data().db.lock() {
            Ok(c) => c,
            Err(_) => return ABI_ERR_OPERATION,
        };

        let result = if let Some(ref pfx) = prefix {
            let like_pattern = format!("{}%", pfx);
            // VULN-037: Strict per-instance isolation
            let mut stmt = match conn.prepare(
                "SELECT storage_key FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key LIKE ?3 \
                 ORDER BY storage_key",
            ) {
                Ok(s) => s,
                Err(_) => return ABI_ERR_OPERATION,
            };
            let rows: Vec<String> = match stmt.query_map(
                rusqlite::params![&addon_id, &instance_id, &like_pattern],
                |row| row.get::<_, String>(0),
            ) {
                Ok(r) => r.filter_map(|r| r.ok()).collect(),
                Err(_) => return ABI_ERR_OPERATION,
            };
            rows
        } else {
            // VULN-037: Strict per-instance isolation
            let mut stmt = match conn.prepare(
                "SELECT storage_key FROM addon_storage \
                 WHERE addon_id = ?1 AND instance_id = ?2 \
                 ORDER BY storage_key",
            ) {
                Ok(s) => s,
                Err(_) => return ABI_ERR_OPERATION,
            };
            let rows: Vec<String> = match stmt
                .query_map(rusqlite::params![&addon_id, &instance_id], |row| {
                    row.get::<_, String>(0)
                }) {
                Ok(r) => r.filter_map(|r| r.ok()).collect(),
                Err(_) => return ABI_ERR_OPERATION,
            };
            rows
        };

        result
    };

    // Serializuj do JSON
    let json = match serde_json::to_vec(&keys) {
        Ok(j) => j,
        Err(_) => return ABI_ERR_OPERATION,
    };

    audit_log(
        caller.data(),
        "storage.list",
        Some("storage"),
        prefix.as_deref(),
        "ok",
        None,
    );

    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::host_functions::check_permission;
    use crate::addon::host_functions::network::NetworkConnectionManager;
    use crate::addon::permissions::PermissionChecker;
    use crate::addon::AddonManifest;
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::Arc;

    fn make_state(permissions: Vec<String>) -> AddonState {
        let db = crate::db::init(Path::new(":memory:")).unwrap();
        AddonState {
            addon_id: "storage-test-addon".to_string(),
            instance_id: "t".to_string(),
            user_id: None,
            db: db.clone(),
            permissions,
            event_bus: Arc::new(EventBus::new()),
            permission_checker: Arc::new(PermissionChecker::new(db)),
            fuel_consumed: 0,
            is_system_call: true,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher: Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32])),
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            oauth_refresh_guard: std::sync::Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            router: None,
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    #[test]
    fn storage_read_denied_without_permission() {
        // Addon bez uprawnienia "storage" — get/set/delete/list odrzucone.
        let state = make_state(vec!["llm".to_string()]);
        assert!(
            !check_permission(&state, "storage", None),
            "Brak 'storage' w permissions → Denied"
        );
    }
}
