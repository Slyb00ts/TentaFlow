// =============================================================================
// Plik: addon/host_functions/ui.rs
// Opis: Host functions UI API — renderowanie deklaratywnego UI addonu.
//       Addon wysyla opis UI jako JSON, Core renderuje na HTML lub przekazuje
//       do frontendu.
// =============================================================================

use tracing::info;

use super::{
    audit_log, check_permission, get_memory, read_guest_string, AddonState, WasmCaller,
    ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};

// =============================================================================
// ui_render — renderowanie panelu UI
// =============================================================================

/// Host function: renderuje panel UI addonu.
///
/// ABI:
/// - panel_id_ptr/panel_id_len: identyfikator panelu
/// - ui_json_ptr/ui_json_len: deklaratywny opis UI (JSON)
/// - Zwraca: ABI_OK lub kod bledu
pub fn ui_render(
    mut caller: WasmCaller<'_, AddonState>,
    panel_id_ptr: i32,
    panel_id_len: i32,
    ui_json_ptr: i32,
    ui_json_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let panel_id = match read_guest_string(&memory, &caller, panel_id_ptr, panel_id_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let ui_json_str = match read_guest_string(&memory, &caller, ui_json_ptr, ui_json_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    // Sprawdz uprawnienie ui
    if !check_permission(caller.data(), "ui", None) {
        audit_log(
            caller.data(),
            "ui.render",
            Some("ui"),
            Some(&panel_id),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    // Waliduj JSON UI
    let ui_value: serde_json::Value = match serde_json::from_str(&ui_json_str) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("Niepoprawny UI JSON: {}", e);
            audit_log(
                caller.data(),
                "ui.render",
                Some("ui"),
                Some(&panel_id),
                "error",
                Some(&msg),
            );
            return ABI_ERR_OPERATION;
        }
    };

    let addon_id = caller.data().addon_id.clone();
    info!("ui_render: addon='{}', panel_id='{}'", addon_id, panel_id);

    // Zapisz drzewo UI do globalnego cache panelu — frontend GUI pyta
    // przez `AddonUiPanelGetRequest` i renderuje przez tf-* komponenty.
    // Host nie renderuje HTML; addon SDK przekazuje "czyste" drzewo.
    if let Some(cache) = caller.data().ui_panels.clone() {
        cache
            .write()
            .insert((addon_id.clone(), panel_id.clone()), ui_value.clone());
    }

    // Event "ui.panel_rendered" zostaje — inne addony moga reagowac (np.
    // notification overlay) + przyszly push do frontu przez bus subscribe.
    caller
        .data()
        .event_bus
        .publish(crate::addon::event_bus::Event {
            event_type: "ui.panel_rendered".to_string(),
            source_addon: Some(addon_id.clone()),
            source_user: caller.data().user_id,
            payload: serde_json::json!({
                "addon_id": &addon_id,
                "panel_id": &panel_id,
                "tree": &ui_value,
            }),
            timestamp: chrono::Utc::now(),
        });

    audit_log(
        caller.data(),
        "ui.render",
        Some("ui"),
        Some(&panel_id),
        "ok",
        None,
    );

    ABI_OK
}

// =============================================================================
// ui_notify — wyswietlenie notyfikacji
// =============================================================================

/// Host function: wyswietla notyfikacje uzytkownikowi.
///
/// ABI:
/// - title_ptr/title_len: tytul notyfikacji
/// - body_ptr/body_len: tresc notyfikacji
/// - level_ptr/level_len: poziom ("info", "warning", "error", "success")
/// - Zwraca: ABI_OK lub kod bledu
pub fn ui_notify(
    mut caller: WasmCaller<'_, AddonState>,
    title_ptr: i32,
    title_len: i32,
    body_ptr: i32,
    body_len: i32,
    level_ptr: i32,
    level_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let title = match read_guest_string(&memory, &caller, title_ptr, title_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let body = match read_guest_string(&memory, &caller, body_ptr, body_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    let level = if level_ptr != 0 && level_len > 0 {
        read_guest_string(&memory, &caller, level_ptr, level_len)
            .unwrap_or("info")
            .to_string()
    } else {
        "info".to_string()
    };

    // Sprawdz uprawnienie notifications
    if !check_permission(caller.data(), "notifications", None) {
        audit_log(
            caller.data(),
            "ui.notify",
            Some("notifications"),
            None,
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    info!(
        "ui_notify: addon='{}', level='{}', title='{}'",
        addon_id, level, title
    );

    // Wyslij event z notyfikacja
    caller
        .data()
        .event_bus
        .publish(crate::addon::event_bus::Event {
            event_type: "ui.notification".to_string(),
            source_addon: Some(addon_id.clone()),
            source_user: caller.data().user_id,
            payload: serde_json::json!({
                "title": &title,
                "body": &body,
                "level": &level,
            }),
            timestamp: chrono::Utc::now(),
        });

    audit_log(
        caller.data(),
        "ui.notify",
        Some("notifications"),
        None,
        "ok",
        None,
    );

    ABI_OK
}
