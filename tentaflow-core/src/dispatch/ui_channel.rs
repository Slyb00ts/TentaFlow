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

/// Extracts the SQLite i64 user_id from the session (marker-byte format).
fn extract_user_id_i64(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] == 0xFF && user_id[1..8].iter().all(|&b| b == 0) {
                let mut le = [0u8; 8];
                le.copy_from_slice(&user_id[8..]);
                Some(i64::from_le_bytes(le))
            } else {
                None
            }
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
    Decode::decode(&mut decoder, &mut ()).map_err(|e| {
        ProtocolError::bad_request(format!("CBOR decode error: {e}"))
    })
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
            ))
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
        UiPayload::PanelReady(_) | UiPayload::Action(_) => Err(ProtocolError::new(
            ProtocolErrorCode::NotImplemented,
            format!(
                "tag 0x{:04X} is not yet implemented in ui_channel_dispatch",
                tag.as_u16()
            ),
        )),
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

    // Lazy-start the addon via AddonManager so it is ready to receive
    // PanelReady / Action messages.
    if let Some(addon_mgr) = ctx.state.addon_manager.as_ref() {
        if !addon_mgr.has_running_instance(&panel_open.addon_id) {
            let user_id = extract_user_id_i64(ctx);
            addon_mgr
                .start_addon(&panel_open.addon_id, user_id, None)
                .map_err(|e| {
                    // Roll back the panel open on addon start failure.
                    let session_lock =
                        ctx.state.ui_sessions.get_or_create(ctx.connection_id);
                    let mut session = session_lock.lock();
                    session.close_panel(&panel_open.addon_id, &panel_open.panel_id);
                    ProtocolError::internal(format!(
                        "failed to start addon '{}': {e}",
                        panel_open.addon_id
                    ))
                })?;
        }
    }

    // Track which connection is serving this addon+user panel so host
    // functions can locate the SessionState for outbound validation.
    let user_id = extract_user_id_i64(ctx).unwrap_or(0);
    ctx.state
        .ui_sessions
        .register_addon_connection(&panel_open.addon_id, user_id, ctx.connection_id);

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

    let user_id = extract_user_id_i64(ctx).unwrap_or(0);
    ctx.state
        .ui_sessions
        .unregister_addon_connection(&panel_close.addon_id, user_id);

    // Echo the PanelClose back as acknowledgment.
    let response = UiPayload::PanelClose(panel_close);
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
                user_id: [0xFFu8, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0],
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
}
