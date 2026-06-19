// =============================================================================
// Plik: addon/host_functions/config.rs
// Opis: Host function Config API — odczyt instalacyjnej konfiguracji addonu z
//       tabeli `addon_config`, scope'owany WYLACZNIE po addon_id wolajacego.
//       Sluzy do czytania per-instancyjnych wartosci (np. IP robota) ustawionych
//       przy instalacji z formularza connection_param.
// Uprawnienia: "sql.read" — odczyt wlasnej konfiguracji to odczyt per-addon.
//              Fail-closed: brak uprawnienia blokuje dostep przed dotknieciem DB.
// =============================================================================

use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION, ABI_ERR_PERMISSION,
};

/// Maksymalna dlugosc klucza konfiguracji (1024 bajty).
const MAX_KEY_LENGTH: usize = 1024;

// =============================================================================
// config_get_v1 — odczyt jednej wartosci konfiguracji wolajacego addonu
// =============================================================================

/// Host function: zwraca wartosc `addon_config` dla klucza, scope'owana po
/// addon_id wolajacego (zero cross-addon read). Sekrety NIE sa zwracane przez
/// te sciezke — instalacyjne connection-paramy zapisywane sa jako jawne.
///
/// ABI:
/// - key_ptr/key_len: klucz (UTF-8)
/// - out_ptr/out_cap: bufor na wartosc (bajty UTF-8)
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK, ABI_ERR_NOT_FOUND lub kod bledu
pub fn config_get_v1(
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

    if key.is_empty() || key.len() > MAX_KEY_LENGTH {
        audit_log(
            caller.data(),
            "config.get",
            Some("sql.read"),
            Some(&key),
            "error",
            Some("klucz pusty lub za dlugi"),
        );
        return ABI_ERR_OPERATION;
    }

    if !check_permission(caller.data(), "sql.read", None) {
        audit_log(
            caller.data(),
            "config.get",
            Some("sql.read"),
            Some(&key),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();

    let value: Option<String> = {
        match caller.data().db.read() {
            Ok(conn) => conn
                .query_row(
                    "SELECT value FROM addon_config \
                     WHERE addon_id = ?1 AND key = ?2 AND is_secret = 0",
                    rusqlite::params![&addon_id, &key],
                    |row| row.get(0),
                )
                .ok(),
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    match value {
        Some(v) => {
            audit_log(
                caller.data(),
                "config.get",
                Some("sql.read"),
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
                v.as_bytes(),
            )
        }
        None => {
            audit_log(
                caller.data(),
                "config.get",
                Some("sql.read"),
                Some(&key),
                "ok",
                Some("not found"),
            );
            ABI_ERR_NOT_FOUND
        }
    }
}
