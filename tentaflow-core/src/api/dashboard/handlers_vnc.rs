// =============================================================================
// File: api/dashboard/handlers_vnc.rs - binary protocol handlers for the
// same-node noVNC tunnel (websockify bridge through the dashboard WSS).
// =============================================================================

use std::sync::Arc;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth, VncTunnelCloseResponse,
    VncTunnelOpenResponse, VncTunnelPayload, VncTunnelSendResponse, VncTunnelStreamEnd,
    VNC_TUNNEL_OPEN_FAILED, VNC_TUNNEL_OPEN_FORBIDDEN, VNC_TUNNEL_OPEN_NOT_FOUND,
    VNC_TUNNEL_OPEN_NO_PORT, VNC_TUNNEL_OPEN_OK, VNC_TUNNEL_OPEN_REMOTE_NODE,
};

use super::vnc_tunnel::{self, MAX_TUNNELS_PER_USER};
use crate::dispatch::subscription::{
    push_chunk, push_end, find_stream_handler, StreamHandlerMeta, Subscription,
};
use crate::dispatch::{HandlerContext, SessionAuthKind};

// -----------------------------------------------------------------------------
// Helpers (mirror the ones in handlers_meeting.rs - kept local to avoid a
// crate-wide re-export for a single-feature module).
// -----------------------------------------------------------------------------

fn current_user_id(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] != 0xFF {
                return None;
            }
            let mut le = [0u8; 8];
            le.copy_from_slice(&user_id[8..]);
            Some(i64::from_le_bytes(le))
        }
        _ => None,
    }
}

fn is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

fn bad_request(msg: &str) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::InvalidFrame, msg.to_string())
}

fn emit_open_status(sub: &Subscription, status: &str, error: &str) {
    let resp = VncTunnelOpenResponse {
        status: status.to_string(),
        tunnel_id: String::new(),
        error: error.to_string(),
    };
    let _ = push_chunk(
        sub,
        MessageBody::VncTunnelBody(VncTunnelPayload::ResOpen(resp)),
    );
    // Terminal chunk - ws_binary follows `push_end` with IS_STREAM_END and
    // cleans up the subscription. Client distinguishes "ok" vs failure by the
    // `status` field of the ResOpen payload.
    let _ = push_end(
        sub,
        Some(MessageBody::VncTunnelBody(VncTunnelPayload::StreamEnd(
            VncTunnelStreamEnd {
                tunnel_id: String::new(),
                reason: status.to_string(),
            },
        ))),
    );
}

// -----------------------------------------------------------------------------
// Streaming handler: VncTunnelOpenRequest.
// -----------------------------------------------------------------------------
// Initial response is delivered as the FIRST `SubscriptionEvent::Chunk` carrying
// `ResOpen { status, tunnel_id }`. Subsequent chunks are raw RFB bytes in
// `Chunk { bytes }`. On any failure before the TCP dial we emit ResOpen with a
// non-ok status followed by push_end (no bridge task ever spawns).

fn vnc_tunnel_open_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    let session_id = match req {
        MessageBody::VncTunnelBody(VncTunnelPayload::ReqOpen(r)) => r.session_id,
        _ => {
            emit_open_status(&sub, VNC_TUNNEL_OPEN_FAILED, "expected VncTunnelOpenRequest");
            return;
        }
    };

    let Some(me) = current_user_id(&ctx) else {
        emit_open_status(&sub, VNC_TUNNEL_OPEN_FORBIDDEN, "session missing user_id");
        return;
    };

    tokio::spawn(async move {
        // 1. Session lookup through MeetingManager (same path as HTTP dashboard).
        let desc = match ctx.state.meeting_manager.session_detail(session_id) {
            Ok(Some(d)) => d,
            Ok(None) => {
                emit_open_status(&sub, VNC_TUNNEL_OPEN_NOT_FOUND, "session not found");
                return;
            }
            Err(e) => {
                emit_open_status(&sub, VNC_TUNNEL_OPEN_FAILED, &format!("db error: {e}"));
                return;
            }
        };

        // 2. BOLA check: only owner or admin.
        if !is_admin(&ctx) && desc.owner_user_id != Some(me) {
            emit_open_status(&sub, VNC_TUNNEL_OPEN_FORBIDDEN, "not your session");
            return;
        }

        // 3. Same-node gate. `meeting_sessions` does not persist node_id today,
        // so sessions created locally are always local. When cross-node mesh
        // forwarding lands (phase B) this is where a remote dispatch would go.
        // The constant is preserved so the wire contract stays stable.
        let _remote_sentinel = VNC_TUNNEL_OPEN_REMOTE_NODE;

        // 4. Port lookup. Use the raw RFB port (x11vnc on container 5900) rather
        // than the websockify port (6080). noVNC's RFB client speaks raw RFB and
        // runs its own framing; websockify would expect a WebSocket handshake
        // we don't perform on the tunnel, leading to a silent stall.
        let Some(port) = desc.vnc_port else {
            emit_open_status(&sub, VNC_TUNNEL_OPEN_NO_PORT, "container has no vnc port");
            return;
        };

        // 5. Per-user tunnel budget.
        if vnc_tunnel::count_for_user(&ctx.state.vnc_tunnels, me) >= MAX_TUNNELS_PER_USER {
            emit_open_status(
                &sub,
                VNC_TUNNEL_OPEN_FAILED,
                "tunnel limit reached (3 concurrent per user)",
            );
            return;
        }

        // 6. Allocate tunnel_id, announce ok, spawn bridge.
        let tunnel_id = uuid::Uuid::new_v4().to_string();
        let ok = VncTunnelOpenResponse {
            status: VNC_TUNNEL_OPEN_OK.to_string(),
            tunnel_id: tunnel_id.clone(),
            error: String::new(),
        };
        if push_chunk(
            &sub,
            MessageBody::VncTunnelBody(VncTunnelPayload::ResOpen(ok)),
        )
        .is_err()
        {
            // Client already gone - no bridge to start.
            return;
        }

        vnc_tunnel::spawn_tunnel_bridge(
            Arc::clone(&ctx.state.vnc_tunnels),
            tunnel_id,
            me,
            port,
            sub,
        );
    });
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "VncTunnelOpenRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: vnc_tunnel_open_handler,
    }
}

// -----------------------------------------------------------------------------
// One-shot handler: VncTunnelSendRequest (browser -> container RFB bytes).
// -----------------------------------------------------------------------------

#[handler(variant = "VncTunnelSendRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn vnc_tunnel_send(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::VncTunnelBody(VncTunnelPayload::ReqSend(p)) => p,
        _ => return Err(bad_request("expected VncTunnelSendRequest")),
    };
    let me = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;

    // ACL: lookup owner inside the DashMap. Use a scoped borrow so the map
    // reference lock is dropped before any await.
    let authorized = {
        match ctx.state.vnc_tunnels.get(&payload.tunnel_id) {
            Some(entry) => entry.owner_user_id == me || is_admin(ctx),
            None => {
                return Ok(MessageBody::VncTunnelBody(VncTunnelPayload::ResSend(
                    VncTunnelSendResponse {
                        ok: false,
                        error: "tunnel not found".to_string(),
                    },
                )));
            }
        }
    };
    if !authorized {
        return Ok(MessageBody::VncTunnelBody(VncTunnelPayload::ResSend(
            VncTunnelSendResponse {
                ok: false,
                error: "forbidden".to_string(),
            },
        )));
    }

    match vnc_tunnel::write_to_tunnel(&ctx.state.vnc_tunnels, &payload.tunnel_id, &payload.bytes)
        .await
    {
        Ok(()) => Ok(MessageBody::VncTunnelBody(VncTunnelPayload::ResSend(
            VncTunnelSendResponse {
                ok: true,
                error: String::new(),
            },
        ))),
        Err(e) => {
            // Drop the broken tunnel so subsequent sends short-circuit and the
            // bridge read loop wakes with EOF to notify the subscription.
            vnc_tunnel::close_tunnel(&ctx.state.vnc_tunnels, &payload.tunnel_id);
            Ok(MessageBody::VncTunnelBody(VncTunnelPayload::ResSend(
                VncTunnelSendResponse {
                    ok: false,
                    error: e,
                },
            )))
        }
    }
}

// -----------------------------------------------------------------------------
// One-shot handler: VncTunnelCloseRequest.
// -----------------------------------------------------------------------------

#[handler(variant = "VncTunnelCloseRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn vnc_tunnel_close(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::VncTunnelBody(VncTunnelPayload::ReqClose(p)) => p,
        _ => return Err(bad_request("expected VncTunnelCloseRequest")),
    };
    let me = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;

    // ACL check, then remove atomically.
    let authorized = match ctx.state.vnc_tunnels.get(&payload.tunnel_id) {
        Some(entry) => entry.owner_user_id == me || is_admin(ctx),
        None => false,
    };
    let removed = if authorized {
        vnc_tunnel::close_tunnel(&ctx.state.vnc_tunnels, &payload.tunnel_id)
    } else {
        false
    };
    Ok(MessageBody::VncTunnelBody(VncTunnelPayload::ResClose(
        VncTunnelCloseResponse { ok: removed },
    )))
}

// Keep the symbol referenced so `find_stream_handler` inventory wiring isn't
// dead-code-eliminated in minimal test builds.
#[allow(dead_code)]
fn _force_link() {
    let _ = find_stream_handler("VncTunnelOpenRequest");
}
