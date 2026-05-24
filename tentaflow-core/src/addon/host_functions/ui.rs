// =============================================================================
// File: addon/host_functions/ui.rs
// UI host functions — CBOR-based UI channel + notifications.
// =============================================================================

use tracing::info;

use super::{
    audit_log, check_permission, get_memory, read_guest_bytes, read_guest_string, AddonState,
    WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};
use super::abi_helpers::{enforce_payload_size, PayloadKind};
use tentaflow_sdk_spec::{validate_canonical, UiTag};

// =============================================================================
// ui_render_cbor — CBOR UI payload from addon guest
// =============================================================================

/// Host function: receives CBOR-encoded UI payload from addon.
///
/// ABI:
/// - cbor_ptr/cbor_len: CBOR bytes encoding a UiPayload message
/// - Returns: ABI_OK or error code
pub fn ui_render_cbor(
    mut caller: WasmCaller<'_, AddonState>,
    cbor_ptr: i32,
    cbor_len: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Detach guest memory borrow before any caller.data() access.
    let cbor_bytes = match read_guest_bytes(&memory, &caller, cbor_ptr, cbor_len) {
        Some(b) => b.to_vec(),
        None => return ABI_ERR_OPERATION,
    };

    if enforce_payload_size(cbor_bytes.len(), PayloadKind::UiRender).is_err() {
        audit_log(
            caller.data(),
            "ui.render_cbor",
            Some("ui"),
            None,
            "denied",
            Some("payload too large"),
        );
        return ABI_ERR_OPERATION;
    }

    if !check_permission(caller.data(), "ui", None) {
        audit_log(
            caller.data(),
            "ui.render_cbor",
            Some("ui"),
            None,
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    // Validate canonical CBOR encoding (RFC 8949 Core Deterministic).
    if let Err(e) = validate_canonical(&cbor_bytes) {
        let addon_id = caller.data().addon_id.clone();
        tracing::warn!(
            addon = %addon_id,
            bytes = cbor_bytes.len(),
            error = %e,
            "ui_render_cbor: canonical CBOR validation failed"
        );
        audit_log(
            caller.data(),
            "ui.render_cbor",
            Some("ui"),
            None,
            "denied",
            Some("malformed_cbor"),
        );
        return ABI_ERR_OPERATION;
    }

    // Extract the tag from the outer CBOR array [tag: u16, body].
    let tag = match extract_ui_tag(&cbor_bytes) {
        Some(t) => t,
        None => {
            let addon_id = caller.data().addon_id.clone();
            tracing::warn!(
                addon = %addon_id,
                bytes = cbor_bytes.len(),
                "ui_render_cbor: unknown or malformed UI tag"
            );
            audit_log(
                caller.data(),
                "ui.render_cbor",
                Some("ui"),
                None,
                "denied",
                Some("unknown_ui_tag"),
            );
            return ABI_ERR_OPERATION;
        }
    };

    let addon_id = caller.data().addon_id.clone();
    info!(
        "ui_render_cbor: addon='{}', tag=0x{:04X}, bytes={}",
        addon_id,
        tag.as_u16(),
        cbor_bytes.len()
    );

    // Store raw validated CBOR bytes in the ui_panels cache.
    // Key uses "cbor_msg" as the panel slot — the actual panel routing
    // happens downstream in the CBOR dispatch layer.
    let cache_user_id = caller.data().user_id;
    if let Some(cache) = caller.data().ui_panels.clone() {
        let key_user = cache_user_id.unwrap_or(0);
        cache.write().insert(
            (key_user, addon_id.clone(), "cbor_msg".to_string()),
            cbor_bytes.clone(),
        );
    }

    // Publish event with tag and raw CBOR bytes on event bus.
    caller
        .data()
        .event_bus
        .publish(crate::addon::event_bus::Event {
            event_type: "ui.cbor_message".to_string(),
            source_addon: Some(addon_id.clone()),
            source_user: cache_user_id,
            payload: serde_json::json!({
                "addon_id": &addon_id,
                "tag": tag.as_u16(),
                "cbor": cbor_bytes,
            }),
            timestamp: chrono::Utc::now(),
        });

    audit_log(
        caller.data(),
        "ui.render_cbor",
        Some("ui"),
        None,
        "ok",
        None,
    );

    ABI_OK
}

/// Decode the outer CBOR array to extract the UI tag (u16).
/// Expected wire format: array(2) [ tag: u16, body: ... ].
fn extract_ui_tag(bytes: &[u8]) -> Option<UiTag> {
    let mut dec = minicbor::Decoder::new(bytes);
    // Expect a 2-element array
    let len = dec.array().ok()??;
    if len != 2 {
        return None;
    }
    let tag_raw: u16 = dec.u16().ok()?;
    UiTag::from_u16(tag_raw)
}

// =============================================================================
// ui_notify — user notification
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
