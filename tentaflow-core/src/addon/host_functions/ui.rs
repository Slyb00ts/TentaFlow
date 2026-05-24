// =============================================================================
// File: addon/host_functions/ui.rs
// UI host functions — CBOR-based UI channel + notifications.
// =============================================================================

use std::collections::HashSet;

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

    // Validate outbound slot/shell messages against session state.
    if let Some(registry) = crate::addon::ui_session::global_registry() {
        let user_id = caller.data().user_id.unwrap_or(0);

        if let Some(conn_id) = registry.find_connection(&addon_id, user_id) {
            let session_lock = registry.get_or_create(conn_id);
            let mut session = session_lock.lock();

            match tag {
                UiTag::PanelShell => {
                    if let Err(e) = handle_panel_shell_registration(&cbor_bytes, &mut session, &addon_id) {
                        tracing::warn!(
                            addon = %addon_id,
                            "ui_render_cbor: PanelShell registration rejected: {e}"
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
                UiTag::SlotContent | UiTag::SlotClear | UiTag::SlotShow | UiTag::SlotHide => {
                    if let Some((panel_id, slot_id)) = extract_panel_and_slot_id(&cbor_bytes) {
                        if let Err(e) = session.validate_slot_ownership(&addon_id, &panel_id, &slot_id) {
                            tracing::warn!(
                                addon = %addon_id,
                                panel = %panel_id,
                                slot = %slot_id,
                                "ui_render_cbor: slot ownership violation: {e}"
                            );
                            audit_log(
                                caller.data(),
                                "ui.render_cbor",
                                Some("ui"),
                                None,
                                "denied",
                                Some(&format!("slot_ownership_violation: {e}")),
                            );
                            return ABI_ERR_OPERATION;
                        }
                    }
                }
                UiTag::StateSnapshot => {
                    if let Err(e) = handle_state_snapshot(&cbor_bytes, &mut session, &addon_id) {
                        tracing::warn!(
                            addon = %addon_id,
                            "ui_render_cbor: StateSnapshot rejected: {e}"
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
                UiTag::StatePatch => {
                    if let Err(e) = handle_state_patch(&cbor_bytes, &mut session, &addon_id) {
                        tracing::warn!(
                            addon = %addon_id,
                            "ui_render_cbor: StatePatch rejected: {e}"
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
                UiTag::StateReset => {
                    if let Err(e) = handle_state_reset(&cbor_bytes, &mut session, &addon_id) {
                        tracing::warn!(
                            addon = %addon_id,
                            "ui_render_cbor: StateReset rejected: {e}"
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
                _ => {}
            }
        }
    }

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
            _ => { dec.skip().ok()?; }
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
    let shell: tentaflow_sdk_spec::protocol::ui::panel::PanelShell =
        minicbor::Decode::decode(&mut dec, &mut ())
            .map_err(|e| format!("PanelShell decode: {e}"))?;

    let slots: HashSet<String> = shell.slots.iter().map(|s| s.id.clone()).collect();

    session
        .register_shell(
            addon_id,
            &shell.panel_id,
            shell.panel_epoch,
            slots,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            HashSet::new(),
        )
        .map_err(|e| e.to_string())
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
    minicbor::Decode::decode(&mut dec, &mut ())
        .map_err(|e| format!("body decode: {e}"))
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
        None => Err(format!(
            "panel_not_open: addon={addon_id} panel={panel_id}"
        )),
    }
}

fn handle_state_snapshot(
    bytes: &[u8],
    session: &mut crate::addon::ui_session::SessionState,
    addon_id: &str,
) -> Result<(), String> {
    let snap: tentaflow_sdk_spec::protocol::ui::state::StateSnapshot =
        decode_state_body(bytes)?;

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
    let patch: tentaflow_sdk_spec::protocol::ui::state::StatePatch =
        decode_state_body(bytes)?;

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
    let reset: tentaflow_sdk_spec::protocol::ui::state::StateReset =
        decode_state_body(bytes)?;

    validate_panel_epoch(session, addon_id, &reset.panel_id, reset.panel_epoch)?;

    session
        .advance_state_revision(addon_id, &reset.panel_id, reset.new_revision)
        .map_err(|e| e.to_string())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use minicbor::Encode;
    use tentaflow_sdk_spec::UiPayload;
    use tentaflow_sdk_spec::protocol::ui::slot_msg::{SlotClear, SlotContent, SlotHide, SlotShow};
    use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
    use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
    use tentaflow_sdk_spec::protocol::ui::slot::{
        CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility,
    };

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
        assert!(session.validate_slot_ownership("contacts", "main", "content").is_ok());
        assert!(session.validate_slot_ownership("contacts", "main", "drawer").is_ok());

        // Undeclared slot fails.
        assert!(session.validate_slot_ownership("contacts", "main", "other").is_err());
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

    use tentaflow_sdk_spec::protocol::ui::state::{StatePatch, StateReset, StateSnapshot};
    use tentaflow_sdk_spec::protocol::ui::bind::{PathSegment, StatePath};
    use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
    use tentaflow_sdk_spec::protocol::ui::slot::StateEntry;
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
}
