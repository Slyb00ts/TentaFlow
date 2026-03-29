// =============================================================================
// Plik: addon/host_functions/user.rs
// Opis: Host functions User API — informacje o aktualnym uzytkowniku
//       i sprawdzanie uprawnien. Addon moze sprawdzic kim jest uzytkownik
//       i czy ma konkretne uprawnienie.
// =============================================================================

use super::{
    AddonState, ABI_OK, ABI_ERR_PERMISSION, ABI_ERR_OPERATION, ABI_ERR_NOT_FOUND,
    get_memory, read_guest_string, write_guest_output, audit_log, check_permission,
    WasmCaller,
};

// =============================================================================
// user_get_current — informacje o aktualnym uzytkowniku
// =============================================================================

/// Host function: pobiera informacje o aktualnym uzytkowniku.
///
/// ABI:
/// - out_ptr/out_cap: bufor na JSON {id, username, display_name, email, groups: [...]}
/// - out_len_ptr: ile bajtow zapisano
/// - Zwraca: ABI_OK lub kod bledu
pub fn user_get_current(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Sprawdz uprawnienie user_info
    if !check_permission(caller.data(), "user_info", None) {
        audit_log(caller.data(), "user.get_current", Some("user_info"), None, "denied", None);
        return ABI_ERR_PERMISSION;
    }

    let user_id = match caller.data().user_id {
        Some(id) => id,
        None => {
            // Brak uzytkownika — instancja systemowa
            let system_json = serde_json::json!({
                "id": null,
                "username": "system",
                "display_name": "System",
                "email": null,
                "groups": ["system"],
            });
            let bytes = serde_json::to_vec(&system_json).unwrap_or_default();
            return write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &bytes);
        }
    };

    // Pobierz dane uzytkownika z DB
    let user_json = {
        match caller.data().db.lock() {
            Ok(conn) => {
                // Pobierz uzytkownika
                let user_data: Option<(String, Option<String>, Option<String>)> = conn.query_row(
                    "SELECT username, display_name, email FROM users WHERE id = ?1",
                    rusqlite::params![user_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                ).ok();

                match user_data {
                    Some((username, display_name, email)) => {
                        // Pobierz grupy uzytkownika
                        let groups: Vec<String> = {
                            let mut stmt = match conn.prepare(
                                "SELECT g.name FROM groups g \
                                 JOIN user_groups ug ON g.id = ug.group_id \
                                 WHERE ug.user_id = ?1"
                            ) {
                                Ok(s) => s,
                                Err(_) => return ABI_ERR_OPERATION,
                            };
                            stmt.query_map(rusqlite::params![user_id], |row| row.get(0))
                                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                                .unwrap_or_default()
                        };

                        serde_json::json!({
                            "id": user_id,
                            "username": username,
                            "display_name": display_name,
                            "email": email,
                            "groups": groups,
                        })
                    }
                    None => return ABI_ERR_NOT_FOUND,
                }
            }
            Err(_) => return ABI_ERR_OPERATION,
        }
    };

    let bytes = match serde_json::to_vec(&user_json) {
        Ok(b) => b,
        Err(_) => return ABI_ERR_OPERATION,
    };

    audit_log(caller.data(), "user.get_current", Some("user_info"), None, "ok", None);

    write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &bytes)
}

// =============================================================================
// user_check_permission — sprawdzenie uprawnienia
// =============================================================================

/// Host function: sprawdza czy aktualny uzytkownik ma dane uprawnienie.
///
/// ABI:
/// - permission_type_ptr/permission_type_len: typ uprawnienia
/// - resource_ptr/resource_len: zasob (opcjonalny)
/// - access_level_ptr/access_level_len: poziom dostepu ("ro", "rw", "rwd")
/// - Zwraca: 0 = przyznano, -1 = odmowiono
pub fn user_check_permission(
    mut caller: WasmCaller<'_, AddonState>,
    permission_type_ptr: i32,
    permission_type_len: i32,
    resource_ptr: i32,
    resource_len: i32,
    access_level_ptr: i32,
    access_level_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let permission_type = match read_guest_string(&memory, &caller, permission_type_ptr, permission_type_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let resource = if resource_ptr != 0 && resource_len > 0 {
        read_guest_string(&memory, &caller, resource_ptr, resource_len).map(|s| s.to_string())
    } else {
        None
    };

    let _access_level_str = if access_level_ptr != 0 && access_level_len > 0 {
        read_guest_string(&memory, &caller, access_level_ptr, access_level_len)
            .unwrap_or("ro")
            .to_string()
    } else {
        "ro".to_string()
    };

    // access_level ignorowany — uprawnienia sa boolean (przyznane/nieprzyznane)

    let user_id = match caller.data().user_id {
        Some(id) => id,
        None => return ABI_OK, // Systemowe wywolanie — zawsze przyznane
    };

    let granted = caller.data().permission_checker.check(
        &caller.data().addon_id,
        user_id,
        &permission_type,
        resource.as_deref(),
    ).is_granted();

    if granted { ABI_OK } else { ABI_ERR_PERMISSION }
}
