// =============================================================================
// File: api/dashboard/handlers_browser.rs - one-shot binary handler for
// dashboard-driven browser capture (screenshot / DOM) of the active Chromium
// page inside the teams-bot container. Bridges dashboard WSS to the bot's
// QUIC endpoint via the existing meeting-bot service registration.
// =============================================================================

use std::time::Duration;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    BrowserCapturePayload, BrowserCaptureRequest, BrowserCaptureResponse, BrowserOperation,
    BrowserPayload, BrowserResult, MessageBody, ModelPayload, ModelRequest, ModelResult,
    ProtocolError, ProtocolErrorCode, SessionAuth, BROWSER_CAPTURE_FAILED,
    BROWSER_CAPTURE_FORBIDDEN, BROWSER_CAPTURE_KIND_DOM, BROWSER_CAPTURE_KIND_SCREENSHOT,
    BROWSER_CAPTURE_NOT_FOUND, BROWSER_CAPTURE_OK,
};

use crate::dispatch::HandlerContext;

// Upper bound per dashboard request. The bot already applies its own 10s CDP
// budget; we give the wire path ~2s of slack before giving up on the reply.
const CAPTURE_BUDGET: Duration = Duration::from_secs(12);

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

fn failure(kind: &str, status: &str, error: impl Into<String>) -> BrowserCaptureResponse {
    BrowserCaptureResponse {
        status: status.to_string(),
        kind: kind.to_string(),
        png: Vec::new(),
        html: String::new(),
        error: error.into(),
    }
}

#[handler(variant = "BrowserCaptureRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn browser_capture(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let BrowserCaptureRequest {
        session_id,
        kind,
        full_page,
    } = match req {
        MessageBody::BrowserCaptureBody(BrowserCapturePayload::Request(r)) => r.clone(),
        _ => return Err(bad_request("expected BrowserCaptureRequest")),
    };

    // Resolve kind -> BrowserOperation early: invalid kind is a wire-level bug.
    let operation = match kind.as_str() {
        BROWSER_CAPTURE_KIND_SCREENSHOT => BrowserOperation::Screenshot { full_page },
        BROWSER_CAPTURE_KIND_DOM => BrowserOperation::Dom,
        other => {
            return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
                other,
                BROWSER_CAPTURE_FAILED,
                format!("unknown kind: {other}"),
            ))));
        }
    };

    let me = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;

    // 1. Session lookup + ACL.
    let desc = match ctx.state.meeting_manager.session_detail(session_id) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
                &kind,
                BROWSER_CAPTURE_NOT_FOUND,
                "session not found",
            ))));
        }
        Err(e) => {
            return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
                &kind,
                BROWSER_CAPTURE_FAILED,
                format!("db error: {e}"),
            ))));
        }
    };
    if !is_admin(ctx) && desc.owner_user_id != Some(me) {
        return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
            &kind,
            BROWSER_CAPTURE_FORBIDDEN,
            "not your session",
        ))));
    }

    // 2. Locate the bot's QUIC client. MeetingManager registers under
    // `meeting-bot-{session_id}` as a QUIC LLM service so the existing
    // meeting_bot_connection_loop takes over the reverse-listener wiring.
    let service_name = format!("meeting-bot-{}", session_id);
    let Some(client) = ctx
        .state
        .service_manager
        .get_quic_llm_client(&service_name)
        .await
    else {
        return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
            &kind,
            BROWSER_CAPTURE_FAILED,
            "bot QUIC client not available",
        ))));
    };

    // 3. Dispatch Browser ModelRequest. Bot replies with ModelResult::Browser
    // or ModelResult::Error; we map both onto BrowserCaptureResponse.
    let request = ModelRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        payload: ModelPayload::Browser(BrowserPayload { operation }),
        stream: false,
        metadata: None,
        session_id: None,
    };

    let send = client.send_request(request);
    let response = match tokio::time::timeout(CAPTURE_BUDGET, send).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
                &kind,
                BROWSER_CAPTURE_FAILED,
                format!("bot dispatch: {e}"),
            ))));
        }
        Err(_) => {
            return Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(failure(
                &kind,
                BROWSER_CAPTURE_FAILED,
                "bot reply timeout (12s)",
            ))));
        }
    };

    let payload = match response.result {
        ModelResult::Browser(b) => match b {
            BrowserResult::Screenshot { png } => BrowserCaptureResponse {
                status: BROWSER_CAPTURE_OK.to_string(),
                kind: BROWSER_CAPTURE_KIND_SCREENSHOT.to_string(),
                png,
                html: String::new(),
                error: String::new(),
            },
            BrowserResult::Dom { html } => BrowserCaptureResponse {
                status: BROWSER_CAPTURE_OK.to_string(),
                kind: BROWSER_CAPTURE_KIND_DOM.to_string(),
                png: Vec::new(),
                html,
                error: String::new(),
            },
            BrowserResult::Error { message } => failure(&kind, BROWSER_CAPTURE_FAILED, message),
        },
        ModelResult::Error(e) => failure(&kind, BROWSER_CAPTURE_FAILED, e.message),
        _ => failure(&kind, BROWSER_CAPTURE_FAILED, "unexpected ModelResult variant"),
    };

    Ok(MessageBody::BrowserCaptureBody(BrowserCapturePayload::Response(payload)))
}
