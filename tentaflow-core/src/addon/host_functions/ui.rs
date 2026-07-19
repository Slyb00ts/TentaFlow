// =============================================================================
// File: addon/host_functions/ui.rs
// UI host functions — CBOR-based UI channel + notifications.
// =============================================================================

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use tracing::info;

use super::abi_helpers::{enforce_payload_size, PayloadKind};
use super::{
    audit_log, check_permission, get_memory, read_guest_bytes, read_guest_string, AddonState,
    WasmCaller, ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};
use tentaflow_sdk_spec::{validate_canonical, UiTag};

// Thread-local reusable buffer for guest CBOR bytes. Avoids a fresh heap
// allocation + deallocation on every ui_render_cbor call.  The buffer grows
// to high-water-mark and stays allocated across calls on the same thread.
thread_local! {
    static CBOR_BUF: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(4096));
}

// =============================================================================
// ui_render_cbor — CBOR UI payload from addon guest
// =============================================================================

/// Host function: receives CBOR-encoded UI payload from addon.
///
/// ABI:
/// - cbor_ptr/cbor_len: CBOR bytes encoding a UiPayload message
/// - Returns: ABI_OK or error code
pub fn ui_render_cbor(mut caller: WasmCaller<'_, AddonState>, cbor_ptr: i32, cbor_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    // Size check before copying — avoids allocation for oversized payloads.
    let payload_len = cbor_len as usize;
    if enforce_payload_size(payload_len, PayloadKind::UiRender).is_err() {
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

    // Copy guest bytes into a thread-local reusable buffer.  The buffer
    // grows to high-water-mark and stays allocated across calls on the
    // same thread, eliminating per-call alloc/dealloc overhead.
    let cbor_bytes: Vec<u8> = match CBOR_BUF.with(|cell| {
        let guest_slice = read_guest_bytes(&memory, &caller, cbor_ptr, cbor_len)?;
        let mut buf = cell.borrow_mut();
        buf.clear();
        buf.extend_from_slice(guest_slice);
        Some(buf.clone())
    }) {
        Some(v) => v,
        None => return ABI_ERR_OPERATION,
    };

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
        tracing::warn!(
            addon = %caller.data().addon_id,
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
            tracing::warn!(
                addon = %caller.data().addon_id,
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

    // Single clone of addon_id — used by session validation, cache key,
    // event bus payload, and tracing.  All error paths above borrow from
    // caller.data() directly to avoid cloning on rejection.
    let addon_id = caller.data().addon_id.clone();

    // Validate outbound slot/shell messages against session state.
    if let Some(registry) = crate::addon::ui_session::global_registry() {
        let user_id = caller.data().user_id.clone().unwrap_or_default();
        tracing::debug!(addon = %addon_id, user_id, tag = tag.as_u16(), "ui_render_cbor: looking up connection");

        if let Some(conn_id) = registry.find_connection(&addon_id, &user_id) {
            tracing::debug!(conn_id, "ui_render_cbor: found connection");
            let session_lock = registry.get_or_create(conn_id);
            let mut session = session_lock.lock();

            // Credit-based rate limiting — consume 1 credit per outbound message.
            if session.try_consume_credit().is_err() {
                tracing::warn!(addon = %addon_id, "ui_render_cbor: UI credits exhausted");
                audit_log(
                    caller.data(),
                    "ui.render_cbor",
                    Some("ui"),
                    None,
                    "denied",
                    Some("credits_exhausted"),
                );
                return ABI_ERR_OPERATION;
            }
            if session.should_grant_credits() {
                session.grant_credits(256);
            }

            if let Err(e) = validate_tag_session(&cbor_bytes, tag, &addon_id, &mut session) {
                tracing::warn!(
                    addon = %addon_id,
                    tag = tag.as_u16(),
                    "ui_render_cbor: tag validation rejected: {e}"
                );
                audit_log(
                    caller.data(),
                    "ui.render_cbor",
                    Some("ui"),
                    None,
                    "denied",
                    Some(&e),
                );
                return ABI_ERR_OPERATION;
            }
        }
    }

    // The addon binary stamps its compile-time package id into every payload,
    // but multi-instance addons run under a host-assigned instance id. The
    // host is authoritative about sender identity, so the addon_id field is
    // rewritten before the bytes reach any sink — this also prevents an addon
    // from impersonating another addon's panels.
    let cbor_bytes = rewrite_addon_id(&cbor_bytes, &addon_id).unwrap_or(cbor_bytes);

    tracing::debug!(
        "ui_render_cbor: addon='{}', tag=0x{:04X}, bytes={}",
        addon_id,
        tag.as_u16(),
        cbor_bytes.len()
    );

    // Store raw validated CBOR bytes in the ui_panels cache.
    // Key uses "cbor_msg" as the panel slot — the actual panel routing
    // happens downstream in the CBOR dispatch layer.
    let cache_user_id = caller.data().user_id.clone();
    let cbor_arc: Arc<[u8]> = cbor_bytes.into();

    if let Some(cache) = caller.data().ui_panels.clone() {
        let key_user = cache_user_id.clone().unwrap_or_default();
        cache.write().insert(
            (key_user, addon_id.clone(), "cbor_msg".into()),
            cbor_arc.to_vec(),
        );
    }

    // Publish event with tag and raw CBOR bytes on event bus.
    caller
        .data()
        .event_bus
        .publish(crate::addon::event_bus::Event {
            event_type: "ui.cbor_message".into(),
            source_addon: Some(addon_id),
            source_user: cache_user_id.clone(),
            payload: serde_json::json!({
                "tag": tag.as_u16(),
                "cbor": &*cbor_arc,
            }),
            timestamp: chrono::Utc::now(),
        });

    // Publish to tokio broadcast for WS push to the frontend connection.
    // Arc<[u8]> avoids cloning CBOR payload per broadcast subscriber.
    crate::dispatch::ui_cbor_broadcast::publish(crate::dispatch::ui_cbor_broadcast::UiCborPush {
        user_id: cache_user_id.unwrap_or_default(),
        cbor: cbor_arc,
    });

    ABI_OK
}

/// Validates tag-specific session constraints.  Consolidates per-tag match
/// arms to keep the main function lean and avoid duplicated audit_log/warn.
fn validate_tag_session(
    cbor_bytes: &[u8],
    tag: UiTag,
    addon_id: &str,
    session: &mut crate::addon::ui_session::SessionState,
) -> Result<(), String> {
    match tag {
        UiTag::PanelShell => handle_panel_shell_registration(cbor_bytes, session, addon_id),
        UiTag::SlotContent => {
            if let Some((panel_id, slot_id)) = extract_panel_and_slot_id(cbor_bytes) {
                session
                    .validate_slot_ownership(addon_id, &panel_id, &slot_id)
                    .map_err(|e| format!("slot_ownership_violation: {e}"))?;
                // Dynamically register action_ids from SlotContent CBOR.
                let new_actions = extract_action_ids_from_cbor_bytes(cbor_bytes);
                if !new_actions.is_empty() {
                    session.extend_declared_actions(addon_id, &panel_id, new_actions);
                }
                Ok(())
            } else {
                Ok(())
            }
        }
        UiTag::SlotClear | UiTag::SlotShow | UiTag::SlotHide => {
            if let Some((panel_id, slot_id)) = extract_panel_and_slot_id(cbor_bytes) {
                session
                    .validate_slot_ownership(addon_id, &panel_id, &slot_id)
                    .map_err(|e| format!("slot_ownership_violation: {e}"))
            } else {
                Ok(())
            }
        }
        UiTag::StateSnapshot => handle_state_snapshot(cbor_bytes, session, addon_id),
        UiTag::StatePatch => handle_state_patch(cbor_bytes, session, addon_id),
        UiTag::StateReset => handle_state_reset(cbor_bytes, session, addon_id),
        UiTag::Command => validate_command_security(cbor_bytes),
        UiTag::Event => validate_event_topic(cbor_bytes, addon_id, session),
        UiTag::Batch => validate_batch(cbor_bytes, addon_id, session),
        _ => Ok(()),
    }
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

/// Splices the host-side addon_id over the body map's key-0 text value of a
/// `[tag, body]` UI payload. Every addon-originated UI struct carries
/// `addon_id` (or `source_addon_id`) at map key 0, so the rewrite is uniform
/// across tags; a `Batch` body holds nested `[tag, body]` members instead, so
/// each member's body gets the same key-0 splice. Returns `None` when no
/// rewrite is needed (values already match) or the payload shape carries no
/// key-0 text value (e.g. Command) — callers must then forward the original
/// bytes unchanged.
fn rewrite_addon_id(cbor: &[u8], addon_id: &str) -> Option<Vec<u8>> {
    let mut dec = minicbor::Decoder::new(cbor);
    if dec.array().ok()?? != 2 {
        return None;
    }
    let tag = dec.u16().ok()?;

    let mut splices: Vec<(usize, usize)> = Vec::new();
    if tag == UiTag::Batch.as_u16() {
        // Batch bodies use TEXT keys; the addon_ids live inside each member's
        // own [tag, body] pair under "members".
        let entries = dec.map().ok()??;
        for _ in 0..entries {
            if dec.str().ok()? != "members" {
                dec.skip().ok()?;
                continue;
            }
            let members = dec.array().ok()??;
            for _ in 0..members {
                if dec.array().ok()?? != 2 {
                    return None;
                }
                dec.u16().ok()?;
                let body_start = dec.position();
                dec.skip().ok()?;
                // A member without a rewritable key-0 text value (e.g. a
                // Command body) contributes no splice and stays as-is.
                if let Some((start, end)) =
                    find_key0_splice(cbor.get(body_start..dec.position())?, addon_id)
                {
                    splices.push((body_start + start, body_start + end));
                }
            }
        }
    } else {
        let body_start = dec.position();
        if let Some((start, end)) = find_key0_splice(cbor.get(body_start..)?, addon_id) {
            splices.push((body_start + start, body_start + end));
        }
    }
    if splices.is_empty() {
        return None;
    }

    // 9 bytes covers the largest possible CBOR text header.
    let mut out = Vec::with_capacity(cbor.len() + splices.len() * (addon_id.len() + 9));
    let mut cursor = 0;
    for (start, end) in splices {
        out.extend_from_slice(cbor.get(cursor..start)?);
        minicbor::encode(addon_id, &mut out).ok()?;
        cursor = end;
    }
    out.extend_from_slice(cbor.get(cursor..)?);
    Some(out)
}

/// Locates a body map's key-0 text value and returns its byte range
/// (relative to `body`) when it differs from `addon_id`.
fn find_key0_splice(body: &[u8], addon_id: &str) -> Option<(usize, usize)> {
    let mut dec = minicbor::Decoder::new(body);
    // Derive emits definite-length maps; indefinite (None) is left untouched.
    let entries = dec.map().ok()??;
    for _ in 0..entries {
        let key = dec.u32().ok()?;
        if key != 0 {
            dec.skip().ok()?;
            continue;
        }
        let start = dec.position();
        if dec.str().ok()? == addon_id {
            return None;
        }
        return Some((start, dec.position()));
    }
    None
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
            source_user: caller.data().user_id.clone(),
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

// =============================================================================
// CBOR extraction helpers for slot dispatch validation
// =============================================================================

/// Extracts `panel_id` (key 1) and `slot_id` (key 3) from the body map of a
/// slot message (SlotContent/SlotClear/SlotShow/SlotHide).
/// Wire: array(2) [ tag: u16, body: map { 0: addon_id, 1: panel_id, 2: epoch, 3: slot_id, ... } ]
fn extract_panel_and_slot_id(bytes: &[u8]) -> Option<(String, String)> {
    let mut dec = minicbor::Decoder::new(bytes);
    // Skip outer array header + tag
    let _arr_len = dec.array().ok()??;
    let _tag = dec.u16().ok()?;

    // Body is a map — scan for keys 1 and 3.
    let map_len = dec.map().ok()??;
    let mut panel_id: Option<String> = None;
    let mut slot_id: Option<String> = None;

    for _ in 0..map_len {
        let key = dec.u32().ok()?;
        match key {
            1 => panel_id = Some(dec.str().ok()?.to_owned()),
            3 => slot_id = Some(dec.str().ok()?.to_owned()),
            _ => {
                dec.skip().ok()?;
            }
        }
        if panel_id.is_some() && slot_id.is_some() {
            break;
        }
    }

    Some((panel_id?, slot_id?))
}

/// Processes a PanelShell message: extracts slot declarations and registers the
/// shell in the session state.
/// Wire: array(2) [ 0x0102, body: map { 0: addon_id, 1: panel_id, 2: epoch, 4: slots, ... } ]
fn handle_panel_shell_registration(
    bytes: &[u8],
    session: &mut crate::addon::ui_session::SessionState,
    addon_id: &str,
) -> Result<(), String> {
    let mut dec = minicbor::Decoder::new(bytes);
    // Skip outer array header + tag
    dec.array()
        .map_err(|e| format!("array: {e}"))?
        .ok_or("indefinite array")?;
    dec.u16().map_err(|e| format!("tag: {e}"))?;

    // Decode PanelShell body using minicbor derive (map-keyed struct).
    tracing::debug!("PanelShell: decoding body...");
    let shell: tentaflow_sdk_spec::protocol::ui::panel::PanelShell =
        match minicbor::Decode::decode(&mut dec, &mut ()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "PanelShell decode FAILED");
                return Err(format!("PanelShell decode: {e}"));
            }
        };

    let slots: HashSet<String> = shell.slots.iter().map(|s| s.id.clone()).collect();

    // Decode the full CBOR payload as generic Value so nested handlers inside
    // FieldMap children are visible without depending on concrete layout types.
    let actions = extract_action_ids_from_cbor_bytes(bytes);
    tracing::debug!(
        actions = ?actions,
        layout_tag = shell.layout.tag,
        layout_has_handlers = shell.layout.handlers.is_some(),
        layout_fields_count = shell.layout.fields.0.len(),
        "PanelShell action extraction"
    );

    session
        .register_shell(
            addon_id,
            &shell.panel_id,
            shell.panel_epoch,
            slots,
            actions,
            Vec::new(),
            Vec::new(),
            HashSet::new(),
        )
        .map_err(|e| e.to_string())
}

fn extract_action_ids_from_cbor_bytes(bytes: &[u8]) -> HashSet<String> {
    let Ok(value) = minicbor::decode::<tentaflow_sdk_spec::protocol::value::Value>(bytes) else {
        return HashSet::new();
    };
    let mut actions = HashSet::new();
    extract_action_ids_from_value(&value, &mut actions);
    actions
}

fn extract_action_ids_from_value(
    value: &tentaflow_sdk_spec::protocol::value::Value,
    actions: &mut HashSet<String>,
) {
    use tentaflow_sdk_spec::protocol::value::Value;

    match value {
        Value::Array(items) => {
            for item in items {
                extract_action_ids_from_value(item, actions);
            }
        }
        Value::Map(entries) => {
            let mut handler_kind = false;
            let mut handler_action_id = None;

            for (k, v) in entries {
                if matches!(
                    (k, v),
                    (Value::Text(key), Value::Text(kind))
                        if key == "kind" && (kind == "backend" || kind == "both")
                ) {
                    handler_kind = true;
                }
                if let (Value::Text(key), Value::Text(action_id)) = (k, v) {
                    if key == "action_id" {
                        handler_action_id = Some(action_id.clone());
                    }
                }
                extract_action_ids_from_value(k, actions);
                extract_action_ids_from_value(v, actions);
            }

            if handler_kind {
                if let Some(action_id) = handler_action_id {
                    actions.insert(action_id);
                }
            }

            extract_component_declared_actions(entries, actions);
        }
        _ => {}
    }
}

/// Registers action_ids carried as DECLARATIVE component fields instead of a
/// HandlerMap (`AudioCapture.action_id`, `SearchBox.on_search_action_id`,
/// `FileInput.upload_action_id`, …). The renderer emits these actions itself,
/// so they must count as declared or the dispatcher rejects the round-trip.
/// The catalog schema is the single source of which fields carry actions.
fn extract_component_declared_actions(
    entries: &[(
        tentaflow_sdk_spec::protocol::value::Value,
        tentaflow_sdk_spec::protocol::value::Value,
    )],
    actions: &mut HashSet<String>,
) {
    use tentaflow_sdk_spec::protocol::value::Value;

    // Component envelope shape: {0: tag(u16), 1: id, 2: fields(map<u8,Value>)}.
    let mut tag: Option<u16> = None;
    let mut fields: Option<&Vec<(Value, Value)>> = None;
    for (k, v) in entries {
        match (k, v) {
            (Value::U64(0), Value::U64(t)) => tag = u16::try_from(*t).ok(),
            (Value::U64(2), Value::Map(m)) => fields = Some(m),
            _ => {}
        }
    }
    let (Some(tag), Some(fields)) = (tag, fields) else {
        return;
    };
    let Some(meta) = tentaflow_sdk_spec::protocol::ui::schema::ALL_COMPONENTS
        .iter()
        .find(|c| c.tag == tag)
    else {
        return;
    };
    for field_meta in meta.fields {
        if !field_meta.name.ends_with("action_id") {
            continue;
        }
        let declared = fields.iter().find_map(|(k, v)| match (k, v) {
            (Value::U64(key), Value::Text(action_id)) if *key == u64::from(field_meta.key) => {
                Some(action_id.clone())
            }
            _ => None,
        });
        if let Some(action_id) = declared {
            actions.insert(action_id);
        }
    }
}

// =============================================================================
// State dispatch helpers — StateSnapshot / StatePatch / StateReset
// =============================================================================

/// Decode the body struct after skipping the outer array + tag u16.
fn decode_state_body<'b, T>(bytes: &'b [u8]) -> Result<T, String>
where
    T: minicbor::Decode<'b, ()>,
{
    let mut dec = minicbor::Decoder::new(bytes);
    dec.array()
        .map_err(|e| format!("array: {e}"))?
        .ok_or("indefinite array")?;
    dec.u16().map_err(|e| format!("tag: {e}"))?;
    minicbor::Decode::decode(&mut dec, &mut ()).map_err(|e| format!("body decode: {e}"))
}

/// Validates panel open + epoch match.
fn validate_panel_epoch(
    session: &crate::addon::ui_session::SessionState,
    addon_id: &str,
    panel_id: &str,
    panel_epoch: u64,
) -> Result<(), String> {
    match session.get_panel(addon_id, panel_id) {
        Some(ownership) => {
            if ownership.panel_epoch != panel_epoch {
                Err(format!(
                    "epoch_mismatch: expected={}, got={}",
                    ownership.panel_epoch, panel_epoch
                ))
            } else {
                Ok(())
            }
        }
        None => Err(format!("panel_not_open: addon={addon_id} panel={panel_id}")),
    }
}

fn handle_state_snapshot(
    bytes: &[u8],
    session: &mut crate::addon::ui_session::SessionState,
    addon_id: &str,
) -> Result<(), String> {
    let snap: tentaflow_sdk_spec::protocol::ui::state::StateSnapshot = decode_state_body(bytes)?;

    validate_panel_epoch(session, addon_id, &snap.panel_id, snap.panel_epoch)?;

    session
        .advance_state_revision(addon_id, &snap.panel_id, snap.state_revision)
        .map_err(|e| e.to_string())
}

fn handle_state_patch(
    bytes: &[u8],
    session: &mut crate::addon::ui_session::SessionState,
    addon_id: &str,
) -> Result<(), String> {
    let patch: tentaflow_sdk_spec::protocol::ui::state::StatePatch = decode_state_body(bytes)?;

    validate_panel_epoch(session, addon_id, &patch.panel_id, patch.panel_epoch)?;

    session
        .validate_state_revision(addon_id, &patch.panel_id, patch.base_revision)
        .map_err(|e| e.to_string())?;

    // §8.3: addon-initiated patches cannot write to reserved namespaces.
    for op in &patch.ops {
        if let Some(tentaflow_sdk_spec::protocol::ui::bind::PathSegment::Key(root)) =
            op.path.segments.first()
        {
            crate::addon::ui_session::SessionState::validate_state_path_writable(root, false)
                .map_err(|e| e.to_string())?;
        }
    }

    session
        .advance_state_revision(addon_id, &patch.panel_id, patch.new_revision)
        .map_err(|e| e.to_string())
}

fn handle_state_reset(
    bytes: &[u8],
    session: &mut crate::addon::ui_session::SessionState,
    addon_id: &str,
) -> Result<(), String> {
    let reset: tentaflow_sdk_spec::protocol::ui::state::StateReset = decode_state_body(bytes)?;

    validate_panel_epoch(session, addon_id, &reset.panel_id, reset.panel_epoch)?;

    session
        .advance_state_revision(addon_id, &reset.panel_id, reset.new_revision)
        .map_err(|e| e.to_string())
}

// =============================================================================
// Event topic validation
// =============================================================================

/// Extracts topic segments from an Event message and validates against session
/// declared_event_publish patterns.
fn validate_event_topic(
    bytes: &[u8],
    addon_id: &str,
    session: &crate::addon::ui_session::SessionState,
) -> Result<(), String> {
    let segments = extract_event_topic_segments(bytes)?;
    session
        .validate_event_publish(addon_id, &segments)
        .map_err(|e| e.to_string())
}

/// Decodes the Event body and extracts topic as `(kind, value)` segment pairs.
fn extract_event_topic_segments(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let event: tentaflow_sdk_spec::protocol::ui::event::Event = decode_state_body(bytes)?;
    let segments: Vec<(String, String)> = event
        .topic
        .segments
        .iter()
        .map(|seg| match seg {
            tentaflow_sdk_spec::protocol::ui::event::TopicSegment::Literal { value } => {
                ("literal".to_owned(), value.clone())
            }
            tentaflow_sdk_spec::protocol::ui::event::TopicSegment::Id { value } => {
                ("id".to_owned(), value.clone())
            }
        })
        .collect();
    Ok(segments)
}

// =============================================================================
// Batch validation
// =============================================================================

/// Maximum batch members enforced at the host level.
const BATCH_MAX_MEMBERS: usize = tentaflow_sdk_spec::BATCH_MAX_MEMBERS;

/// Validates a Batch message: member count, no nested batch, per-member
/// validation with the same rules as standalone messages.
fn validate_batch(
    bytes: &[u8],
    addon_id: &str,
    session: &mut crate::addon::ui_session::SessionState,
) -> Result<(), String> {
    let batch: tentaflow_sdk_spec::Batch = decode_state_body(bytes)?;

    if batch.members.len() > BATCH_MAX_MEMBERS {
        return Err(format!(
            "batch member count {} exceeds maximum {}",
            batch.members.len(),
            BATCH_MAX_MEMBERS
        ));
    }

    for (i, member) in batch.members.iter().enumerate() {
        if member.tag == tentaflow_sdk_spec::UiTag::Batch {
            return Err(format!("nested batch not allowed (member index {i})"));
        }

        // Re-encode the member as [tag, body] for per-tag validation.
        let member_bytes = encode_member_as_payload(member)?;
        validate_outbound_member(member.tag, &member_bytes, addon_id, session)
            .map_err(|e| format!("batch member {i}: {e}"))?;
    }

    Ok(())
}

/// Re-encodes a BatchMember into a standalone `[tag, body]` CBOR payload so
/// existing per-tag validators can consume it.
fn encode_member_as_payload(member: &tentaflow_sdk_spec::BatchMember) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    minicbor::encode(&member.body, &mut buf).map_err(|e| format!("re-encode member: {e}"))?;
    Ok(buf)
}

/// Per-tag validation for a single batch member (same rules as standalone).
fn validate_outbound_member(
    tag: tentaflow_sdk_spec::UiTag,
    member_bytes: &[u8],
    addon_id: &str,
    session: &mut crate::addon::ui_session::SessionState,
) -> Result<(), String> {
    use tentaflow_sdk_spec::UiTag;

    match tag {
        UiTag::PanelShell => handle_panel_shell_registration(member_bytes, session, addon_id),
        UiTag::SlotContent | UiTag::SlotClear | UiTag::SlotShow | UiTag::SlotHide => {
            if let Some((panel_id, slot_id)) = extract_panel_and_slot_id(member_bytes) {
                session
                    .validate_slot_ownership(addon_id, &panel_id, &slot_id)
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        }
        UiTag::StateSnapshot => handle_state_snapshot(member_bytes, session, addon_id),
        UiTag::StatePatch => handle_state_patch(member_bytes, session, addon_id),
        UiTag::StateReset => handle_state_reset(member_bytes, session, addon_id),
        UiTag::Command => validate_command_security(member_bytes),
        UiTag::Event => validate_event_topic(member_bytes, addon_id, session),
        UiTag::Batch => Err("nested batch not allowed".to_string()),
        _ => Ok(()),
    }
}

// =============================================================================
// Command security validation (defense-in-depth)
// =============================================================================

/// Defense-in-depth: validates security-sensitive Command fields even though
/// the SDK spec encoder already rejects invalid values. Catches a malicious
/// addon that crafts raw CBOR bypassing the encoder.
fn validate_command_security(bytes: &[u8]) -> Result<(), String> {
    let cmd: tentaflow_sdk_spec::protocol::ui::command::Command = decode_state_body(bytes)?;

    match &cmd {
        tentaflow_sdk_spec::protocol::ui::command::Command::NavigateExternal { url, .. } => {
            if !url.starts_with("https://") {
                return Err(format!(
                    "NavigateExternal URL must use https:// scheme: {url}"
                ));
            }
        }
        tentaflow_sdk_spec::protocol::ui::command::Command::Download { filename, .. } => {
            if filename.is_empty() || filename.len() > 128 {
                return Err(format!(
                    "Download filename length out of range: {}",
                    filename.len()
                ));
            }
            if !filename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
            {
                return Err(format!(
                    "Download filename contains invalid characters: {filename}"
                ));
            }
        }
        _ => {}
    }

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use minicbor::Encode;
    use tentaflow_sdk_spec::protocol::control::CborMap;
    use tentaflow_sdk_spec::protocol::ui::a11y::EventKind;
    use tentaflow_sdk_spec::protocol::ui::component::HandlerMap;
    use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
    use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};
    use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
    use tentaflow_sdk_spec::protocol::ui::slot::{
        CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility,
    };
    use tentaflow_sdk_spec::protocol::ui::slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow};
    use tentaflow_sdk_spec::protocol::ui::typed_field::encode_to_value;
    use tentaflow_sdk_spec::UiPayload;

    fn encode_payload(p: &UiPayload) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        p.encode(&mut enc, &mut ()).unwrap();
        buf
    }

    fn empty_comp() -> Component {
        Component {
            tag: 0x0001,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    #[test]
    fn extract_panel_and_slot_from_slot_content() {
        let payload = UiPayload::SlotContent(SlotContent {
            addon_id: "contacts".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            slot_id: "content".into(),
            fragment: empty_comp(),
            state_overlay: None,
        });
        let bytes = encode_payload(&payload);
        let (panel, slot) = extract_panel_and_slot_id(&bytes).unwrap();
        assert_eq!(panel, "main");
        assert_eq!(slot, "content");
    }

    #[test]
    fn extract_panel_and_slot_from_slot_clear() {
        let payload = UiPayload::SlotClear(SlotClear {
            addon_id: "a".into(),
            panel_id: "settings".into(),
            panel_epoch: 2,
            slot_id: "sidebar".into(),
        });
        let bytes = encode_payload(&payload);
        let (panel, slot) = extract_panel_and_slot_id(&bytes).unwrap();
        assert_eq!(panel, "settings");
        assert_eq!(slot, "sidebar");
    }

    #[test]
    fn extract_panel_and_slot_from_slot_show_hide() {
        for payload in [
            UiPayload::SlotShow(SlotShow {
                addon_id: "a".into(),
                panel_id: "p".into(),
                panel_epoch: 1,
                slot_id: "modal".into(),
            }),
            UiPayload::SlotHide(SlotHide {
                addon_id: "a".into(),
                panel_id: "p".into(),
                panel_epoch: 1,
                slot_id: "modal".into(),
            }),
        ] {
            let bytes = encode_payload(&payload);
            let (panel, slot) = extract_panel_and_slot_id(&bytes).unwrap();
            assert_eq!(panel, "p");
            assert_eq!(slot, "modal");
        }
    }

    #[test]
    fn panel_shell_registration_creates_slots() {
        let shell = UiPayload::PanelShell(PanelShell {
            addon_id: "contacts".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            layout: empty_comp(),
            slots: vec![
                SlotDecl {
                    id: "content".into(),
                    semantics: SlotSemantics::MainContent,
                    default_state: SlotDefault::Empty,
                    cache_policy: CachePolicy::None,
                    visibility: SlotVisibility::Always,
                    max_payload_bytes: None,
                },
                SlotDecl {
                    id: "drawer".into(),
                    semantics: SlotSemantics::Drawer,
                    default_state: SlotDefault::Empty,
                    cache_policy: CachePolicy::None,
                    visibility: SlotVisibility::Hidden,
                    max_payload_bytes: None,
                },
            ],
            initial_state: vec![],
            initial_commands: vec![],
        });

        let bytes = encode_payload(&shell);
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("contacts", "main").unwrap();

        handle_panel_shell_registration(&bytes, &mut session, "contacts").unwrap();

        // Declared slots pass validation.
        assert!(session
            .validate_slot_ownership("contacts", "main", "content")
            .is_ok());
        assert!(session
            .validate_slot_ownership("contacts", "main", "drawer")
            .is_ok());

        // Undeclared slot fails.
        assert!(session
            .validate_slot_ownership("contacts", "main", "other")
            .is_err());
    }

    #[test]
    fn panel_shell_registration_declares_nested_actions() {
        let child = Component {
            tag: 0x0002,
            id: "nav".into(),
            fields: FieldMap::default(),
            handlers: Some(HandlerMap(vec![(
                EventKind::Click,
                Handler::Backend {
                    action_id: "panel-navigate".into(),
                    params: CborMap::default(),
                    optimistic: None,
                    on_failure: FailurePolicy::Toast,
                },
            )])),
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        let mut layout = empty_comp();
        layout.fields = FieldMap(vec![(2, encode_to_value(&vec![child]).unwrap())]);
        let shell = UiPayload::PanelShell(PanelShell {
            addon_id: "tentavision".into(),
            panel_id: "overview".into(),
            panel_epoch: 1,
            layout,
            slots: vec![],
            initial_state: vec![],
            initial_commands: vec![],
        });

        let bytes = encode_payload(&shell);
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("tentavision", "overview").unwrap();

        handle_panel_shell_registration(&bytes, &mut session, "tentavision").unwrap();

        assert!(session
            .validate_action("tentavision", "overview", "panel-navigate")
            .is_ok());
    }

    #[test]
    fn panel_shell_registration_declares_component_action_fields() {
        // AudioCapture carries its action as a declarative field (no
        // HandlerMap) — the renderer emits it itself, so registration must
        // pick it up from the catalog schema or the dispatch rejects it.
        let capture = tentaflow_sdk_spec::AudioCapture {
            action_id: "dictation_utterance".into(),
            mode: tentaflow_sdk_spec::AudioCaptureMode::Vad,
            silence_ms: None,
            min_speech_ms: None,
            language_hint: None,
            recording_path: None,
            disabled: None,
            active_path: None,
            variant: None,
        }
        .into_component("mic")
        .unwrap();
        let mut layout = empty_comp();
        layout.fields = FieldMap(vec![(2, encode_to_value(&vec![capture]).unwrap())]);
        let shell = UiPayload::PanelShell(PanelShell {
            addon_id: "notes".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            layout,
            slots: vec![],
            initial_state: vec![],
            initial_commands: vec![],
        });

        let bytes = encode_payload(&shell);
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("notes", "main").unwrap();

        handle_panel_shell_registration(&bytes, &mut session, "notes").unwrap();

        assert!(session
            .validate_action("notes", "main", "dictation_utterance")
            .is_ok());
    }

    #[test]
    fn panel_shell_registration_rejects_double_register() {
        let shell = UiPayload::PanelShell(PanelShell {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            layout: empty_comp(),
            slots: vec![],
            initial_state: vec![],
            initial_commands: vec![],
        });

        let bytes = encode_payload(&shell);
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "p").unwrap();

        handle_panel_shell_registration(&bytes, &mut session, "a").unwrap();
        let err = handle_panel_shell_registration(&bytes, &mut session, "a");
        assert!(err.is_err());
    }

    // =========================================================================
    // State dispatch tests
    // =========================================================================

    use tentaflow_sdk_spec::protocol::ui::bind::{PathSegment, StatePath};
    use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
    use tentaflow_sdk_spec::protocol::ui::slot::StateEntry;
    use tentaflow_sdk_spec::protocol::ui::state::{StatePatch, StateReset, StateSnapshot};
    use tentaflow_sdk_spec::protocol::value::Value;

    #[test]
    fn state_snapshot_advances_revision() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        let payload = UiPayload::StateSnapshot(StateSnapshot {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            state_revision: 5,
            entries: vec![StateEntry {
                path: StatePath::new(vec![PathSegment::Key("count".into())]),
                value: Value::U64(42),
            }],
            truncated: false,
        });
        let bytes = encode_payload(&payload);

        handle_state_snapshot(&bytes, &mut session, "a").unwrap();

        assert_eq!(session.get_panel("a", "main").unwrap().state_revision, 5);
    }

    #[test]
    fn state_snapshot_rejects_epoch_mismatch() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        let payload = UiPayload::StateSnapshot(StateSnapshot {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 999,
            state_revision: 1,
            entries: vec![],
            truncated: false,
        });
        let bytes = encode_payload(&payload);

        let err = handle_state_snapshot(&bytes, &mut session, "a");
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("epoch_mismatch"));
    }

    #[test]
    fn state_patch_revision_mismatch_rejected() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        // Current revision is 0, send base_revision = 5.
        let payload = UiPayload::StatePatch(StatePatch {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            base_revision: 5,
            new_revision: 6,
            ops: vec![PatchOp {
                path: StatePath::new(vec![PathSegment::Key("items".into())]),
                op: PatchOpKind::Set { value: Value::Null },
            }],
        });
        let bytes = encode_payload(&payload);

        let err = handle_state_patch(&bytes, &mut session, "a");
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("revision mismatch"));
    }

    #[test]
    fn state_patch_reserved_namespace_rejected() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        let payload = UiPayload::StatePatch(StatePatch {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            base_revision: 0,
            new_revision: 1,
            ops: vec![PatchOp {
                path: StatePath::new(vec![
                    PathSegment::Key("__system".into()),
                    PathSegment::Key("theme".into()),
                ]),
                op: PatchOpKind::Set {
                    value: Value::Text("dark".into()),
                },
            }],
        });
        let bytes = encode_payload(&payload);

        let err = handle_state_patch(&bytes, &mut session, "a");
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("reserved namespace"));
    }

    #[test]
    fn state_patch_non_reserved_namespace_accepted() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        let payload = UiPayload::StatePatch(StatePatch {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            base_revision: 0,
            new_revision: 1,
            ops: vec![PatchOp {
                path: StatePath::new(vec![PathSegment::Key("items".into())]),
                op: PatchOpKind::Set {
                    value: Value::U64(10),
                },
            }],
        });
        let bytes = encode_payload(&payload);

        handle_state_patch(&bytes, &mut session, "a").unwrap();

        assert_eq!(session.get_panel("a", "main").unwrap().state_revision, 1);
    }

    #[test]
    fn state_patch_all_reserved_roots_rejected() {
        for root in crate::addon::ui_session::RESERVED_STATE_ROOTS {
            let mut session = crate::addon::ui_session::SessionState::new();
            session.open_panel("a", "main").unwrap();

            let payload = UiPayload::StatePatch(StatePatch {
                addon_id: "a".into(),
                panel_id: "main".into(),
                panel_epoch: 1,
                base_revision: 0,
                new_revision: 1,
                ops: vec![PatchOp {
                    path: StatePath::new(vec![PathSegment::Key(root.to_string())]),
                    op: PatchOpKind::Delete,
                }],
            });
            let bytes = encode_payload(&payload);

            let err = handle_state_patch(&bytes, &mut session, "a");
            assert!(err.is_err(), "expected rejection for root={root}");
        }
    }

    #[test]
    fn state_reset_advances_revision() {
        let mut session = crate::addon::ui_session::SessionState::new();
        session.open_panel("a", "main").unwrap();

        let payload = UiPayload::StateReset(StateReset {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            new_revision: 10,
        });
        let bytes = encode_payload(&payload);

        handle_state_reset(&bytes, &mut session, "a").unwrap();

        assert_eq!(session.get_panel("a", "main").unwrap().state_revision, 10);
    }

    #[test]
    fn state_reset_rejects_panel_not_open() {
        let mut session = crate::addon::ui_session::SessionState::new();

        let payload = UiPayload::StateReset(StateReset {
            addon_id: "a".into(),
            panel_id: "main".into(),
            panel_epoch: 1,
            new_revision: 1,
        });
        let bytes = encode_payload(&payload);

        let err = handle_state_reset(&bytes, &mut session, "a");
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("panel_not_open"));
    }

    // =========================================================================
    // Command security validation tests
    // =========================================================================

    use tentaflow_sdk_spec::protocol::ui::command::Command;
    use tentaflow_sdk_spec::protocol::ui::tokens::NavigateTarget;

    fn encode_command(cmd: &Command) -> Vec<u8> {
        let payload = UiPayload::Command(cmd.clone());
        encode_payload(&payload)
    }

    #[test]
    fn command_navigate_external_https_accepted() {
        let cmd = Command::NavigateExternal {
            url: "https://example.com".into(),
            target: NavigateTarget::NewTab,
        };
        let bytes = encode_command(&cmd);
        assert!(validate_command_security(&bytes).is_ok());
    }

    #[test]
    fn command_navigate_external_http_rejected() {
        // Manually craft CBOR bypassing the encoder's check.
        // The SDK decoder itself also rejects non-https URLs, so the
        // rejection comes from the decode layer (defense-in-depth stack).
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0140).unwrap();
            enc.map(3).unwrap();
            enc.str("kind").unwrap().str("navigate_external").unwrap();
            enc.str("url").unwrap().str("http://evil.com").unwrap();
            enc.str("target").unwrap().str("new_tab").unwrap();
        }
        let err = validate_command_security(&buf);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("https"));
    }

    #[test]
    fn command_download_filename_valid() {
        let cmd = Command::Download {
            signed_url_ref: "ref-123".into(),
            filename: "report_2026.pdf".into(),
        };
        let bytes = encode_command(&cmd);
        assert!(validate_command_security(&bytes).is_ok());
    }

    #[test]
    fn command_download_filename_invalid_rejected() {
        // Craft CBOR with path traversal filename. The SDK decoder itself
        // rejects filenames that don't match [a-zA-Z0-9._-]+, so the error
        // surfaces from the decode layer (defense-in-depth stack).
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0140).unwrap();
            enc.map(3).unwrap();
            enc.str("kind").unwrap().str("download").unwrap();
            enc.str("signed_url_ref").unwrap().str("ref-1").unwrap();
            enc.str("filename")
                .unwrap()
                .str("../../etc/passwd")
                .unwrap();
        }
        let err = validate_command_security(&buf);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("filename"));
    }

    #[test]
    fn command_download_filename_too_long_rejected() {
        let long_name = "a".repeat(129);
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0140).unwrap();
            enc.map(3).unwrap();
            enc.str("kind").unwrap().str("download").unwrap();
            enc.str("signed_url_ref").unwrap().str("ref-1").unwrap();
            enc.str("filename").unwrap().str(&long_name).unwrap();
        }
        let err = validate_command_security(&buf);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("filename"));
    }

    #[test]
    fn command_download_filename_empty_rejected() {
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0140).unwrap();
            enc.map(3).unwrap();
            enc.str("kind").unwrap().str("download").unwrap();
            enc.str("signed_url_ref").unwrap().str("ref-1").unwrap();
            enc.str("filename").unwrap().str("").unwrap();
        }
        let err = validate_command_security(&buf);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("filename"));
    }

    #[test]
    fn command_focus_passes_without_validation() {
        let cmd = Command::Focus {
            component_id: "btn-1".into(),
        };
        let bytes = encode_command(&cmd);
        assert!(validate_command_security(&bytes).is_ok());
    }

    // =========================================================================
    // Event topic validation tests
    // =========================================================================

    use crate::addon::ui_session::TopicPattern;
    use tentaflow_sdk_spec::protocol::ui::event::{
        Event as UiEvent, Topic, TopicSegment as EvTopicSegment,
    };

    #[test]
    fn event_topic_permitted_passes() {
        let mut session = crate::addon::ui_session::SessionState::new();
        let epoch = session.open_panel("addon-a", "main").unwrap();
        session
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                vec![TopicPattern::parse("addon-a.*.updated")],
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let payload = UiPayload::Event(UiEvent {
            source_addon_id: "addon-a".into(),
            topic: Topic::new(vec![
                EvTopicSegment::Literal {
                    value: "addon-a".into(),
                },
                EvTopicSegment::Id {
                    value: "entity-5".into(),
                },
                EvTopicSegment::Literal {
                    value: "updated".into(),
                },
            ]),
            payload: tentaflow_sdk_spec::protocol::value::Value::Null,
            ts_ms: 1_700_000_000_000,
        });
        let bytes = encode_payload(&payload);

        assert!(validate_event_topic(&bytes, "addon-a", &session).is_ok());
    }

    #[test]
    fn event_topic_not_permitted_rejected() {
        let mut session = crate::addon::ui_session::SessionState::new();
        let epoch = session.open_panel("addon-a", "main").unwrap();
        session
            .register_shell(
                "addon-a",
                "main",
                epoch,
                HashSet::new(),
                HashSet::new(),
                vec![TopicPattern::parse("addon-a.contacts.updated")],
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let payload = UiPayload::Event(UiEvent {
            source_addon_id: "addon-a".into(),
            topic: Topic::new(vec![
                EvTopicSegment::Literal {
                    value: "addon-a".into(),
                },
                EvTopicSegment::Literal {
                    value: "contacts".into(),
                },
                EvTopicSegment::Literal {
                    value: "deleted".into(),
                },
            ]),
            payload: tentaflow_sdk_spec::protocol::value::Value::Null,
            ts_ms: 1_700_000_000_000,
        });
        let bytes = encode_payload(&payload);

        let err = validate_event_topic(&bytes, "addon-a", &session);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("not declared"));
    }

    // =========================================================================
    // Batch validation tests
    // =========================================================================

    use tentaflow_sdk_spec::{Batch, BatchMember};

    #[test]
    fn batch_validates_all_members() {
        let mut session = crate::addon::ui_session::SessionState::new();
        let epoch = session.open_panel("contacts", "main").unwrap();

        let mut slots = HashSet::new();
        slots.insert("content".to_owned());
        session
            .register_shell(
                "contacts",
                "main",
                epoch,
                slots,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();

        let batch = UiPayload::Batch(Batch {
            atomic: true,
            members: vec![BatchMember {
                tag: tentaflow_sdk_spec::UiTag::SlotContent,
                body: UiPayload::SlotContent(SlotContent {
                    addon_id: "contacts".into(),
                    panel_id: "main".into(),
                    panel_epoch: 1,
                    slot_id: "content".into(),
                    fragment: empty_comp(),
                    state_overlay: None,
                }),
            }],
        });
        let bytes = encode_payload(&batch);

        assert!(validate_batch(&bytes, "contacts", &mut session).is_ok());
    }

    #[test]
    fn batch_rejects_nested_batch() {
        let mut session = crate::addon::ui_session::SessionState::new();

        // Hand-craft CBOR for a batch that contains a nested batch tag.
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0160).unwrap();
            enc.map(2).unwrap();
            enc.str("atomic").unwrap().bool(false).unwrap();
            enc.str("members").unwrap();
            enc.array(1).unwrap();
            // Member [0x0160, {atomic:false, members:[]}]
            enc.array(2).unwrap();
            enc.u16(0x0160).unwrap();
            enc.map(2).unwrap();
            enc.str("atomic").unwrap().bool(false).unwrap();
            enc.str("members").unwrap().array(0).unwrap();
        }

        // The sdk-spec decoder itself rejects nested batch, so validate_batch
        // will get a decode error.
        let err = validate_batch(&buf, "a", &mut session);
        assert!(err.is_err());
    }

    #[test]
    fn batch_rejects_oversized() {
        let mut session = crate::addon::ui_session::SessionState::new();

        // Build a batch with 65 members (exceeds BATCH_MAX_MEMBERS=64).
        // The sdk-spec decoder rejects >64 members during decode.
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0160).unwrap();
            enc.map(2).unwrap();
            enc.str("atomic").unwrap().bool(false).unwrap();
            enc.str("members").unwrap();
            enc.array(65).unwrap();
            for _ in 0..65 {
                // Each member: PanelReady
                enc.array(2).unwrap();
                enc.u16(0x0103).unwrap();
                enc.map(4).unwrap();
                enc.u32(0).unwrap().str("a").unwrap();
                enc.u32(1).unwrap().str("p").unwrap();
                enc.u32(2).unwrap().u64(1).unwrap();
                enc.u32(3).unwrap().u64(5).unwrap();
            }
        }

        let err = validate_batch(&buf, "a", &mut session);
        assert!(err.is_err());
    }

    // =========================================================================
    // addon_id rewrite tests
    // =========================================================================

    fn sample_panel_shell(addon_id: &str) -> PanelShell {
        PanelShell {
            addon_id: addon_id.into(),
            panel_id: "main".into(),
            panel_epoch: 7,
            layout: empty_comp(),
            slots: vec![SlotDecl {
                id: "content".into(),
                semantics: SlotSemantics::MainContent,
                default_state: SlotDefault::Empty,
                cache_policy: CachePolicy::None,
                visibility: SlotVisibility::Always,
                max_payload_bytes: None,
            }],
            initial_state: vec![],
            initial_commands: vec![],
        }
    }

    #[test]
    fn rewrite_addon_id_panel_shell_to_longer_id() {
        let bytes = encode_payload(&UiPayload::PanelShell(sample_panel_shell("tentavision")));

        let rewritten = rewrite_addon_id(&bytes, "tentavision-47128c6a").unwrap();

        let decoded: UiPayload = minicbor::decode(&rewritten).unwrap();
        assert_eq!(
            decoded,
            UiPayload::PanelShell(sample_panel_shell("tentavision-47128c6a"))
        );
    }

    #[test]
    fn rewrite_addon_id_panel_shell_to_shorter_id() {
        let bytes = encode_payload(&UiPayload::PanelShell(sample_panel_shell(
            "tentavision-47128c6a",
        )));

        let rewritten = rewrite_addon_id(&bytes, "tv").unwrap();

        let decoded: UiPayload = minicbor::decode(&rewritten).unwrap();
        assert_eq!(decoded, UiPayload::PanelShell(sample_panel_shell("tv")));
    }

    #[test]
    fn rewrite_addon_id_noop_when_already_matching() {
        let bytes = encode_payload(&UiPayload::PanelShell(sample_panel_shell("tentavision")));
        assert!(rewrite_addon_id(&bytes, "tentavision").is_none());
    }

    #[test]
    fn rewrite_addon_id_garbage_bytes_returns_none() {
        assert!(rewrite_addon_id(&[], "a").is_none());
        assert!(rewrite_addon_id(&[0xff, 0x00, 0x12, 0x34], "a").is_none());
        // Valid outer array but non-map body.
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0102).unwrap();
            enc.str("not-a-map").unwrap();
        }
        assert!(rewrite_addon_id(&buf, "a").is_none());
    }

    #[test]
    fn rewrite_addon_id_slot_content_roundtrip() {
        let make = |addon_id: &str| {
            UiPayload::SlotContent(SlotContent {
                addon_id: addon_id.into(),
                panel_id: "main".into(),
                panel_epoch: 3,
                slot_id: "content".into(),
                fragment: empty_comp(),
                state_overlay: None,
            })
        };
        let bytes = encode_payload(&make("contacts"));

        let rewritten = rewrite_addon_id(&bytes, "contacts-deadbeef").unwrap();

        let decoded: UiPayload = minicbor::decode(&rewritten).unwrap();
        assert_eq!(decoded, make("contacts-deadbeef"));
    }

    #[test]
    fn rewrite_addon_id_batch_rewrites_all_members() {
        use tentaflow_sdk_spec::protocol::ui::state::StateSnapshot;

        let make = |addon_id: &str| {
            UiPayload::Batch(Batch {
                atomic: true,
                members: vec![
                    BatchMember {
                        tag: tentaflow_sdk_spec::UiTag::SlotContent,
                        body: UiPayload::SlotContent(SlotContent {
                            addon_id: addon_id.into(),
                            panel_id: "main".into(),
                            panel_epoch: 3,
                            slot_id: "content".into(),
                            fragment: empty_comp(),
                            state_overlay: None,
                        }),
                    },
                    BatchMember {
                        tag: tentaflow_sdk_spec::UiTag::StateSnapshot,
                        body: UiPayload::StateSnapshot(StateSnapshot {
                            addon_id: addon_id.into(),
                            panel_id: "main".into(),
                            panel_epoch: 3,
                            state_revision: 1,
                            entries: vec![],
                            truncated: false,
                        }),
                    },
                ],
            })
        };
        let bytes = encode_payload(&make("tentavision"));

        let rewritten = rewrite_addon_id(&bytes, "tentavision-47128c6a").unwrap();

        let decoded: UiPayload = minicbor::decode(&rewritten).unwrap();
        assert_eq!(decoded, make("tentavision-47128c6a"));
    }

    #[test]
    fn rewrite_addon_id_batch_noop_when_members_already_match() {
        let batch = UiPayload::Batch(Batch {
            atomic: false,
            members: vec![BatchMember {
                tag: tentaflow_sdk_spec::UiTag::SlotContent,
                body: UiPayload::SlotContent(SlotContent {
                    addon_id: "tentavision-47128c6a".into(),
                    panel_id: "main".into(),
                    panel_epoch: 3,
                    slot_id: "content".into(),
                    fragment: empty_comp(),
                    state_overlay: None,
                }),
            }],
        });
        let bytes = encode_payload(&batch);

        assert!(rewrite_addon_id(&bytes, "tentavision-47128c6a").is_none());
    }

    #[test]
    fn rewrite_addon_id_non_string_key0_returns_none() {
        // Hand-encoded body map {0: 5} — key 0 holds an int, not text.
        let mut buf = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2).unwrap();
            enc.u16(0x0102).unwrap();
            enc.map(1).unwrap();
            enc.u32(0).unwrap().u32(5).unwrap();
        }
        assert!(rewrite_addon_id(&buf, "a").is_none());
    }
}
