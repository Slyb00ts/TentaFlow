// =============================================================================
// File: dispatch/ui_channel.rs
// Panel lifecycle dispatch for the UI-channel CBOR binary protocol (Faza 6
// Krok 4). Handles PanelOpen, PanelClose and rejects unsupported frontend-
// originated tags.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

use minicbor::{Decode, Encode};
use tentaflow_sdk_spec::validate_canonical;
use tentaflow_sdk_spec::{UiPayload, UiTag};

use super::HandlerContext;

/// Runs a blocking section (synchronous WASM addon call) without starving the
/// async runtime. On a multi-threaded tokio worker it yields via
/// `block_in_place` — other tasks migrate to a replacement worker, so one cold
/// addon start / `on_panel_open` no longer parks a worker and stalls unrelated
/// requests. Off-runtime (unit tests) or on a current-thread runtime it runs
/// inline (block_in_place would panic there).
pub(crate) fn run_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h)
            if matches!(
                h.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Extracts the `user_accounts` UUID from the session (raw 16-byte form).
fn extract_user_id(ctx: &HandlerContext) -> Option<String> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    }
}

/// Decodes the raw CBOR bytes from a `UiChannelCbor` variant into a typed
/// `UiPayload`, validating canonical encoding first.
fn decode_ui_payload(cbor: &[u8]) -> Result<UiPayload, ProtocolError> {
    validate_canonical(cbor).map_err(|e| {
        ProtocolError::bad_request(format!("non-canonical CBOR in UiChannelCbor: {e}"))
    })?;

    let mut decoder = minicbor::Decoder::new(cbor);
    Decode::decode(&mut decoder, &mut ())
        .map_err(|e| ProtocolError::bad_request(format!("CBOR decode error: {e}")))
}

/// Encodes a `UiPayload` into a `UiChannelCbor` response body.
fn encode_response(payload: &UiPayload) -> Result<MessageBody, ProtocolError> {
    let mut buf = Vec::with_capacity(128);
    let mut encoder = minicbor::Encoder::new(&mut buf);
    Encode::encode(payload, &mut encoder, &mut ())
        .map_err(|e| ProtocolError::internal(format!("CBOR encode error: {e}")))?;
    Ok(MessageBody::UiChannelCbor(buf))
}

/// Tags that the frontend is allowed to send toward core. Everything else
/// (SlotContent, StateSnapshot, PanelShell, etc.) originates from the addon
/// side and must be rejected when received from the browser.
fn is_frontend_tag(tag: UiTag) -> bool {
    matches!(
        tag,
        UiTag::PanelOpen | UiTag::PanelReady | UiTag::PanelClose | UiTag::Action
    )
}

#[handler(variant = "UiChannelCbor", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn ui_channel_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let cbor = match req {
        MessageBody::UiChannelCbor(bytes) => bytes,
        _ => {
            return Err(ProtocolError::bad_request(
                "ui_channel_dispatch expected UiChannelCbor variant",
            ));
        }
    };

    let payload = decode_ui_payload(cbor)?;
    let tag = payload.tag();

    if !is_frontend_tag(tag) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::BadRequest,
            format!(
                "tag 0x{:04X} is addon→frontend only and cannot be sent from the browser",
                tag.as_u16()
            ),
        ));
    }

    match payload {
        UiPayload::PanelOpen(panel_open) => handle_panel_open(ctx, panel_open),
        UiPayload::PanelClose(panel_close) => handle_panel_close(ctx, panel_close),
        UiPayload::PanelReady(panel_ready) => handle_panel_ready(ctx, panel_ready),
        UiPayload::Action(action) => handle_action(ctx, action),
        _ => unreachable!(),
    }
}

/// PanelOpen: register panel in session state, lazy-start addon, return epoch.
fn handle_panel_open(
    ctx: &HandlerContext,
    mut panel_open: tentaflow_sdk_spec::protocol::ui::panel::PanelOpen,
) -> Result<MessageBody, ProtocolError> {
    let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
    let epoch = {
        let mut session = session_lock.lock();
        session
            .open_panel(&panel_open.addon_id, &panel_open.panel_id)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?
    };

    // Register addon→connection mapping so host functions (ui_render_cbor)
    // can find this session and register declared actions.
    let user_id_for_conn = extract_user_id(ctx).unwrap_or_default();
    tracing::info!(
        addon = %panel_open.addon_id,
        user_id = user_id_for_conn,
        conn_id = ctx.connection_id,
        "PanelOpen: registering addon connection"
    );
    ctx.state.ui_sessions.register_addon_connection(
        &panel_open.addon_id,
        &user_id_for_conn,
        ctx.connection_id,
    );

    if let Some(addon_mgr) = ctx.state.addon_manager.as_ref() {
        let user_id = extract_user_id(ctx);
        // Multi-tenant: instancja addona musi nieść org sesji, inaczej
        // `instance_org_id` zwraca None i upload dokumentów z panelu odrzuca
        // żądanie ("no running instance").
        let org_id = ctx.org_context.as_ref().map(|o| o.org_id.clone());

        // WASM lifecycle (cold start + on_panel_open) is CPU-bound and runs
        // synchronously; off-load it from the async worker so concurrent panel
        // opens / other requests are not starved while it runs.
        run_blocking(|| -> Result<(), ProtocolError> {
            if addon_mgr.has_running_instance(&panel_open.addon_id) {
                // Addon already running — call on_panel_open on existing instance.
                // If the addon doesn't export on_panel_open (legacy), fall back
                // to stop+start.
                let has_handler = addon_mgr
                    .call_panel_open(
                        &panel_open.addon_id,
                        &panel_open.panel_id,
                        epoch,
                        user_id.clone(),
                    )
                    .unwrap_or(false);

                if !has_handler {
                    let _ = addon_mgr.stop_addon(&panel_open.addon_id);
                    addon_mgr
                        .start_addon(&panel_open.addon_id, user_id.clone(), org_id.clone())
                        .map_err(|e| {
                            let sl = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
                            sl.lock()
                                .close_panel(&panel_open.addon_id, &panel_open.panel_id);
                            ProtocolError::internal(format!(
                                "failed to start addon '{}': {e}",
                                panel_open.addon_id
                            ))
                        })?;
                }
            } else {
                addon_mgr
                    .start_addon(&panel_open.addon_id, user_id.clone(), org_id.clone())
                    .map_err(|e| {
                        let sl = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
                        sl.lock()
                            .close_panel(&panel_open.addon_id, &panel_open.panel_id);
                        ProtocolError::internal(format!(
                            "failed to start addon '{}': {e}",
                            panel_open.addon_id
                        ))
                    })?;

                // start_addon only runs on_start, which never receives the
                // host-assigned panel epoch (so bundled UI addons no longer
                // render there). on_panel_open is the single canonical render
                // entry: deliver the authoritative epoch so the freshly started
                // instance renders its PanelShell with the epoch this session
                // granted. Without this a cold start on a session whose epoch
                // already advanced past 1 (panel reopened, or the instance was
                // restarted by an addon update) would leave the panel blank.
                match addon_mgr.call_panel_open(
                    &panel_open.addon_id,
                    &panel_open.panel_id,
                    epoch,
                    user_id.clone(),
                ) {
                    Ok(has_export) => {
                        // No on_panel_open export means the addon can only have
                        // rendered during on_start; if it also registered no
                        // shell, the panel would be blank — fail the open rather
                        // than return success with nothing to paint.
                        if !has_export {
                            let sl = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
                            let mut session = sl.lock();
                            if !session
                                .is_shell_registered(&panel_open.addon_id, &panel_open.panel_id)
                            {
                                session.close_panel(&panel_open.addon_id, &panel_open.panel_id);
                                return Err(ProtocolError::internal(format!(
                                    "addon '{}' opened panel '{}' but rendered no shell",
                                    panel_open.addon_id, panel_open.panel_id
                                )));
                            }
                        }
                    }
                    Err(e) => {
                        let sl = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
                        sl.lock()
                            .close_panel(&panel_open.addon_id, &panel_open.panel_id);
                        return Err(ProtocolError::internal(format!(
                            "on_panel_open failed for addon '{}': {e}",
                            panel_open.addon_id
                        )));
                    }
                }
            }
            Ok(())
        })?;
    }

    // Track which connection is serving this addon+user panel so host
    // functions can locate the SessionState for outbound validation.
    let user_id = extract_user_id(ctx).unwrap_or_default();
    ctx.state.ui_sessions.register_addon_connection(
        &panel_open.addon_id,
        &user_id,
        ctx.connection_id,
    );

    // Stamp the assigned_epoch into the context before returning.
    panel_open.ctx.assigned_epoch = epoch;

    let response = UiPayload::PanelOpen(panel_open);
    encode_response(&response)
}

/// PanelClose: validate epoch, remove panel from session state.
fn handle_panel_close(
    ctx: &HandlerContext,
    panel_close: tentaflow_sdk_spec::protocol::ui::panel::PanelClose,
) -> Result<MessageBody, ProtocolError> {
    let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
    let mut session = session_lock.lock();

    // Validate that the panel is open and the epoch matches.
    let ownership = session
        .get_panel(&panel_close.addon_id, &panel_close.panel_id)
        .ok_or_else(|| {
            ProtocolError::bad_request(format!(
                "panel not open: addon={} panel={}",
                panel_close.addon_id, panel_close.panel_id
            ))
        })?;

    if ownership.panel_epoch != panel_close.panel_epoch {
        return Err(ProtocolError::bad_request(format!(
            "epoch mismatch: expected {}, got {}",
            ownership.panel_epoch, panel_close.panel_epoch
        )));
    }

    session.close_panel(&panel_close.addon_id, &panel_close.panel_id);

    // Drop session lock before calling into registry (avoids nested lock).
    drop(session);

    let user_id = extract_user_id(ctx).unwrap_or_default();
    ctx.state
        .ui_sessions
        .unregister_addon_connection(&panel_close.addon_id, &user_id);

    // Echo the PanelClose back as acknowledgment.
    let response = UiPayload::PanelClose(panel_close);
    encode_response(&response)
}

/// PanelReady: validate epoch, log first_paint_ms metric, acknowledge.
fn handle_panel_ready(
    ctx: &HandlerContext,
    panel_ready: tentaflow_sdk_spec::protocol::ui::panel::PanelReady,
) -> Result<MessageBody, ProtocolError> {
    let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
    let session = session_lock.lock();

    let ownership = session
        .get_panel(&panel_ready.addon_id, &panel_ready.panel_id)
        .ok_or_else(|| {
            ProtocolError::bad_request(format!(
                "panel not open: addon={} panel={}",
                panel_ready.addon_id, panel_ready.panel_id
            ))
        })?;

    if ownership.panel_epoch != panel_ready.panel_epoch {
        return Err(ProtocolError::bad_request(format!(
            "epoch mismatch: expected {}, got {}",
            ownership.panel_epoch, panel_ready.panel_epoch
        )));
    }

    drop(session);

    tracing::info!(
        addon = %panel_ready.addon_id,
        panel = %panel_ready.panel_id,
        first_paint_ms = panel_ready.first_paint_ms,
        "PanelReady received"
    );

    let response = UiPayload::PanelReady(panel_ready);
    encode_response(&response)
}

/// Converts a `tentaflow_sdk_spec::Value` to `serde_json::Value`.
fn spec_value_to_json(v: &tentaflow_sdk_spec::protocol::value::Value) -> serde_json::Value {
    use tentaflow_sdk_spec::protocol::value::Value as SV;
    match v {
        SV::Null => serde_json::Value::Null,
        SV::Bool(b) => serde_json::Value::Bool(*b),
        SV::U64(n) => serde_json::json!(*n),
        SV::I64(n) => serde_json::json!(*n),
        SV::F64(f) => serde_json::json!(*f),
        SV::Text(s) => serde_json::Value::String(s.clone()),
        SV::Bytes(b) => serde_json::json!(b),
        SV::Array(items) => {
            serde_json::Value::Array(items.iter().map(spec_value_to_json).collect())
        }
        SV::Map(entries) => {
            let mut m = serde_json::Map::new();
            for (k, v) in entries {
                // JSON keys must be strings; use text representation of key.
                let key = match k {
                    SV::Text(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                m.insert(key, spec_value_to_json(v));
            }
            serde_json::Value::Object(m)
        }
    }
}

/// Converts `CborMap` (Vec<(String, Value)>) to `serde_json::Value::Object`.
fn cbor_map_to_json(map: &tentaflow_sdk_spec::protocol::control::CborMap) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (k, v) in &map.0 {
        m.insert(k.clone(), spec_value_to_json(v));
    }
    serde_json::Value::Object(m)
}

/// Builds the addon tool params for a UI action, injecting the HOST-validated
/// `panel_epoch` under the reserved key `__panel_epoch`. WASM instances are pooled /
/// reused, so the addon's static panel-epoch can be stale/foreign — the addon MUST adopt
/// the action's epoch as the source of truth (per-panel field keying + emitting
/// StatePatch/SlotContent with the correct epoch, else the host rejects them as stale).
/// The epoch is validated before this call (ownership.panel_epoch == action.panel_epoch),
/// and carried the same way as `user_id` (request JSON), so the addon reads it inline.
fn action_params_with_epoch(
    params: &tentaflow_sdk_spec::protocol::control::CborMap,
    panel_epoch: u64,
) -> serde_json::Value {
    let mut params_json = cbor_map_to_json(params);
    if let serde_json::Value::Object(map) = &mut params_json {
        map.insert(
            "__panel_epoch".to_string(),
            serde_json::Value::from(panel_epoch),
        );
    }
    params_json
}

/// Action: validate session, delegate to addon, return ActionAck.
fn handle_action(
    ctx: &HandlerContext,
    action: tentaflow_sdk_spec::protocol::ui::action::Action,
) -> Result<MessageBody, ProtocolError> {
    tracing::info!(
        addon = %action.addon_id,
        panel = %action.panel_id,
        action_id = %action.action_id,
        epoch = action.panel_epoch,
        "UI action received"
    );
    let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
    {
        let session = session_lock.lock();

        let ownership = session
            .get_panel(&action.addon_id, &action.panel_id)
            .ok_or_else(|| {
                ProtocolError::bad_request(format!(
                    "panel not open: addon={} panel={}",
                    action.addon_id, action.panel_id
                ))
            })?;

        if ownership.panel_epoch != action.panel_epoch {
            return Err(ProtocolError::bad_request(format!(
                "epoch mismatch: expected {}, got {}",
                ownership.panel_epoch, action.panel_epoch
            )));
        }

        if let Err(e) =
            session.validate_action(&action.addon_id, &action.panel_id, &action.action_id)
        {
            tracing::warn!(error = %e, addon = %action.addon_id, action = %action.action_id, "UI action validation failed");
            return Err(ProtocolError::bad_request(e.to_string()));
        }
    }
    // Session lock dropped before calling addon.

    let addon_mgr = ctx
        .state
        .addon_manager
        .as_ref()
        .ok_or_else(|| ProtocolError::internal("addon manager not configured"))?;

    let user_id = extract_user_id(ctx).unwrap_or_default();
    let tool_name = format!("ui.{}.{}", action.panel_id, action.action_id);
    let params_json = action_params_with_epoch(&action.params, action.panel_epoch);
    tracing::info!(tool = %tool_name, params = %params_json, "UI action calling addon tool");

    // call_tool runs the addon's WASM synchronously — off-load from the async
    // worker so a slow tool doesn't stall other requests on this runtime.
    let call_result =
        run_blocking(|| addon_mgr.call_tool(&action.addon_id, &tool_name, params_json, &user_id));
    let status = match call_result {
        Ok(result) => {
            tracing::info!(result = %result, "UI action tool returned");
            tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Ok
        }
        Err(e) => {
            tracing::error!(error = %e, "UI action tool error");
            tentaflow_sdk_spec::protocol::ui::action::ActionStatus::Error {
                error_code: 0xFFFF,
                message: e.to_string(),
            }
        }
    };

    let ack = tentaflow_sdk_spec::protocol::ui::action::ActionAck {
        addon_id: action.addon_id,
        panel_id: action.panel_id,
        panel_epoch: action.panel_epoch,
        action_id: action.action_id,
        client_action_id: action.client_action_id,
        status,
    };

    let response = UiPayload::ActionAck(ack);
    encode_response(&response)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use std::sync::Arc;
    use tentaflow_protocol::SessionAuth;

    fn test_ctx() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [1u8; 16],
                role: Some("user".to_string()),
            },
            correlation_id: 1,
            connection_id: 42,
            resume_secret: None,
            state: AppState::for_test(),
            org_context: None,
        }
    }

    fn encode_panel_open(addon_id: &str, panel_id: &str) -> Vec<u8> {
        use tentaflow_sdk_spec::protocol::ui::panel::{PanelOpen, PanelOpenContext, Viewport};

        let payload = UiPayload::PanelOpen(PanelOpen {
            addon_id: addon_id.to_owned(),
            panel_id: panel_id.to_owned(),
            ctx: PanelOpenContext {
                user_id: "user-1".to_owned(),
                locale: "en".to_owned(),
                theme: "dark".to_owned(),
                viewport: Viewport {
                    width_px: 1920,
                    height_px: 1080,
                    density: 1.0,
                },
                deep_link: None,
                prefers_reduced_motion: false,
                prefers_high_contrast: false,
                assigned_epoch: 0,
            },
        });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        payload.encode(&mut enc, &mut ()).unwrap();
        buf
    }

    fn encode_panel_close(addon_id: &str, panel_id: &str, epoch: u64) -> Vec<u8> {
        use tentaflow_sdk_spec::protocol::ui::panel::{CloseReason, PanelClose};

        let payload = UiPayload::PanelClose(PanelClose {
            addon_id: addon_id.to_owned(),
            panel_id: panel_id.to_owned(),
            panel_epoch: epoch,
            reason: CloseReason::UserNavigated,
        });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        payload.encode(&mut enc, &mut ()).unwrap();
        buf
    }

    #[test]
    fn panel_open_assigns_epoch() {
        let ctx = test_ctx();
        let cbor = encode_panel_open("contacts", "main");
        let req = MessageBody::UiChannelCbor(cbor);

        let resp = ui_channel_dispatch(&req, &ctx).unwrap();
        let resp_cbor = match &resp {
            MessageBody::UiChannelCbor(b) => b,
            _ => panic!("expected UiChannelCbor response"),
        };

        let mut dec = minicbor::Decoder::new(resp_cbor);
        let ui: UiPayload = UiPayload::decode(&mut dec, &mut ()).unwrap();
        match ui {
            UiPayload::PanelOpen(po) => {
                assert_eq!(po.addon_id, "contacts");
                assert_eq!(po.panel_id, "main");
                assert_eq!(po.ctx.assigned_epoch, 1);
            }
            _ => panic!("expected PanelOpen response"),
        }
    }

    #[test]
    fn panel_open_twice_fails() {
        let ctx = test_ctx();
        let cbor = encode_panel_open("contacts", "main");
        let req = MessageBody::UiChannelCbor(cbor.clone());

        ui_channel_dispatch(&req, &ctx).unwrap();
        let err = ui_channel_dispatch(&req, &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn panel_close_after_open() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        let open_req = MessageBody::UiChannelCbor(open_cbor);
        ui_channel_dispatch(&open_req, &ctx).unwrap();

        let close_cbor = encode_panel_close("contacts", "main", 1);
        let close_req = MessageBody::UiChannelCbor(close_cbor);
        let resp = ui_channel_dispatch(&close_req, &ctx).unwrap();

        match &resp {
            MessageBody::UiChannelCbor(_) => {}
            _ => panic!("expected UiChannelCbor response"),
        }
    }

    #[test]
    fn panel_close_epoch_mismatch() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        let open_req = MessageBody::UiChannelCbor(open_cbor);
        ui_channel_dispatch(&open_req, &ctx).unwrap();

        let close_cbor = encode_panel_close("contacts", "main", 999);
        let close_req = MessageBody::UiChannelCbor(close_cbor);
        let err = ui_channel_dispatch(&close_req, &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn panel_close_not_open() {
        let ctx = test_ctx();
        let close_cbor = encode_panel_close("contacts", "main", 1);
        let close_req = MessageBody::UiChannelCbor(close_cbor);
        let err = ui_channel_dispatch(&close_req, &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn addon_only_tag_rejected() {
        use tentaflow_sdk_spec::protocol::ui::state::StateSnapshot;

        let payload = UiPayload::StateSnapshot(StateSnapshot {
            addon_id: "x".to_owned(),
            panel_id: "y".to_owned(),
            panel_epoch: 1,
            state_revision: 0,
            entries: vec![],
            truncated: false,
        });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        payload.encode(&mut enc, &mut ()).unwrap();

        let ctx = test_ctx();
        let req = MessageBody::UiChannelCbor(buf);
        let err = ui_channel_dispatch(&req, &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("addon→frontend only"));
    }

    // =========================================================================
    // PanelReady tests
    // =========================================================================

    fn encode_panel_ready(
        addon_id: &str,
        panel_id: &str,
        epoch: u64,
        first_paint_ms: u32,
    ) -> Vec<u8> {
        use tentaflow_sdk_spec::protocol::ui::panel::PanelReady;

        let payload = UiPayload::PanelReady(PanelReady {
            addon_id: addon_id.to_owned(),
            panel_id: panel_id.to_owned(),
            panel_epoch: epoch,
            first_paint_ms,
        });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        payload.encode(&mut enc, &mut ()).unwrap();
        buf
    }

    #[test]
    fn panel_ready_acknowledged() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        let open_req = MessageBody::UiChannelCbor(open_cbor);
        ui_channel_dispatch(&open_req, &ctx).unwrap();

        let ready_cbor = encode_panel_ready("contacts", "main", 1, 42);
        let ready_req = MessageBody::UiChannelCbor(ready_cbor);
        let resp = ui_channel_dispatch(&ready_req, &ctx).unwrap();

        let resp_cbor = match &resp {
            MessageBody::UiChannelCbor(b) => b,
            _ => panic!("expected UiChannelCbor response"),
        };

        let mut dec = minicbor::Decoder::new(resp_cbor);
        let ui: UiPayload = UiPayload::decode(&mut dec, &mut ()).unwrap();
        match ui {
            UiPayload::PanelReady(pr) => {
                assert_eq!(pr.addon_id, "contacts");
                assert_eq!(pr.panel_id, "main");
                assert_eq!(pr.panel_epoch, 1);
                assert_eq!(pr.first_paint_ms, 42);
            }
            _ => panic!("expected PanelReady response"),
        }
    }

    #[test]
    fn panel_ready_rejects_not_open() {
        let ctx = test_ctx();
        let ready_cbor = encode_panel_ready("contacts", "main", 1, 10);
        let ready_req = MessageBody::UiChannelCbor(ready_cbor);
        let err = ui_channel_dispatch(&ready_req, &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn panel_ready_rejects_epoch_mismatch() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        ui_channel_dispatch(&MessageBody::UiChannelCbor(open_cbor), &ctx).unwrap();

        let ready_cbor = encode_panel_ready("contacts", "main", 999, 10);
        let err = ui_channel_dispatch(&MessageBody::UiChannelCbor(ready_cbor), &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    // =========================================================================
    // Action tests
    // =========================================================================

    fn encode_action(addon_id: &str, panel_id: &str, epoch: u64, action_id: &str) -> Vec<u8> {
        use tentaflow_sdk_spec::protocol::control::CborMap;
        use tentaflow_sdk_spec::protocol::ids::ClientActionId;
        use tentaflow_sdk_spec::protocol::ui::action::Action;

        let payload = UiPayload::Action(Action {
            addon_id: addon_id.to_owned(),
            panel_id: panel_id.to_owned(),
            panel_epoch: epoch,
            action_id: action_id.to_owned(),
            params: CborMap(vec![]),
            form_values: None,
            user_gesture: true,
            client_action_id: ClientActionId::from_bytes([1; 16]),
        });

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        payload.encode(&mut enc, &mut ()).unwrap();
        buf
    }

    /// Register a shell with declared actions for a panel that is already open.
    fn register_shell_with_actions(
        ctx: &HandlerContext,
        addon_id: &str,
        panel_id: &str,
        epoch: u64,
        actions: &[&str],
    ) {
        use std::collections::HashSet;
        let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
        let mut session = session_lock.lock();
        let acts: HashSet<String> = actions.iter().map(|s| s.to_string()).collect();
        session
            .register_shell(
                addon_id,
                panel_id,
                epoch,
                HashSet::new(),
                acts,
                Vec::new(),
                Vec::new(),
                HashSet::new(),
            )
            .unwrap();
    }

    #[test]
    fn action_dispatches_to_addon() {
        // Without AddonManager, call_tool path returns "addon manager not
        // configured". We verify session validation passes and the error comes
        // from the missing addon manager.
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        ui_channel_dispatch(&MessageBody::UiChannelCbor(open_cbor), &ctx).unwrap();

        register_shell_with_actions(&ctx, "contacts", "main", 1, &["save"]);

        let action_cbor = encode_action("contacts", "main", 1, "save");
        let err = ui_channel_dispatch(&MessageBody::UiChannelCbor(action_cbor), &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::Internal);
        assert!(err.message.contains("addon manager not configured"));
    }

    #[test]
    fn action_rejects_undeclared_action() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        ui_channel_dispatch(&MessageBody::UiChannelCbor(open_cbor), &ctx).unwrap();

        register_shell_with_actions(&ctx, "contacts", "main", 1, &["save"]);

        let action_cbor = encode_action("contacts", "main", 1, "delete");
        let err = ui_channel_dispatch(&MessageBody::UiChannelCbor(action_cbor), &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("delete"));
    }

    #[test]
    fn action_params_inject_validated_panel_epoch() {
        use tentaflow_sdk_spec::protocol::control::CborMap;
        use tentaflow_sdk_spec::protocol::value::Value as SV;

        // params nioslo wlasne pole (field) — wstrzykniety epoch nie nadpisuje go.
        let params = CborMap(vec![("field".into(), SV::Text("chat_question".into()))]);
        let out = action_params_with_epoch(&params, 7);
        assert_eq!(
            out.get("field").and_then(|v| v.as_str()),
            Some("chat_question")
        );
        assert_eq!(
            out.get("__panel_epoch").and_then(|v| v.as_u64()),
            Some(7),
            "epoch z akcji musi trafic do params jako __panel_epoch (zrodlo prawdy addona)"
        );

        // Pusty params: addon i tak dostaje epoch.
        let empty = action_params_with_epoch(&CborMap(vec![]), 42);
        assert_eq!(
            empty.get("__panel_epoch").and_then(|v| v.as_u64()),
            Some(42)
        );
    }

    #[test]
    fn action_rejects_panel_not_open() {
        let ctx = test_ctx();
        let action_cbor = encode_action("contacts", "main", 1, "save");
        let err = ui_channel_dispatch(&MessageBody::UiChannelCbor(action_cbor), &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("panel not open"));
    }

    #[test]
    fn action_rejects_epoch_mismatch() {
        let ctx = test_ctx();
        let open_cbor = encode_panel_open("contacts", "main");
        ui_channel_dispatch(&MessageBody::UiChannelCbor(open_cbor), &ctx).unwrap();

        register_shell_with_actions(&ctx, "contacts", "main", 1, &["save"]);

        let action_cbor = encode_action("contacts", "main", 999, "save");
        let err = ui_channel_dispatch(&MessageBody::UiChannelCbor(action_cbor), &ctx).unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("epoch mismatch"));
    }
}
