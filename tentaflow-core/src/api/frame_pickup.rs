// =============================================================================
// File: api/frame_pickup.rs — Service-to-Core POST /core/frame/pickup
// =============================================================================
//
// Endpoint a TentaVision service (yolo, ocr, …) hits to fetch the raw bytes
// for a `RawFrameRef` previously announced to it via a `service_call_v1`
// PickupToken. This is the **only** way frame bytes leave the core process,
// so the security model lives here: HMAC on the token, header cross-checks
// against the token payload, one-shot consume of both the token and the LRU
// entry. Every outcome — ok or not — writes a row to `frame_pickup_log`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::db::DbPool;
use crate::services::frame_storage::{FramePixelFormat, FrameStorage, RawFrameRef, StoredFrame};
use crate::services::pickup_tokens::{PickupTokenIssuer, PickupVerifyError};

/// Header names — kept here so the test suite, the host-side service_call_v1
/// wiring, and the HTTP handler agree on a single source of truth.
pub const HDR_PICKUP_TOKEN: &str = "X-Pickup-Token";
pub const HDR_FRAME_REF: &str = "X-Frame-Raw-Ref";
pub const HDR_SERVICE_ID: &str = "X-Service-Id";
pub const HDR_REQUEST_ID: &str = "X-Request-Id";

/// Response headers that carry frame metadata alongside the byte body.
pub const HDR_FRAME_WIDTH: &str = "X-Frame-Width";
pub const HDR_FRAME_HEIGHT: &str = "X-Frame-Height";
pub const HDR_FRAME_PIXEL_FORMAT: &str = "X-Frame-Pixel-Format";
pub const HDR_FRAME_TS_MS: &str = "X-Frame-Timestamp-Ms";
pub const HDR_FRAME_PTS: &str = "X-Frame-Pts";

/// Outcome variant returned to the HTTP layer. The HTTP layer is responsible
/// for mapping these to status codes + bodies; pure logic here so the handler
/// is testable without spinning up hyper.
#[derive(Debug)]
pub enum PickupOutcome {
    Ok {
        bytes: Arc<[u8]>,
        width: u32,
        height: u32,
        pixel_format: &'static str,
        timestamp_unix_ms: u64,
        pts: Option<u64>,
    },
    /// Missing / malformed required header.
    BadHeaders(&'static str),
    /// Verify rejected the token (forge / replay / unknown / expired).
    Unauthorized(PickupVerifyError),
    /// Header values do not match the token payload (cross-service replay).
    HeaderMismatch(&'static str),
    /// Frame already evicted from the LRU before pickup.
    FramePurged,
}

impl PickupOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadHeaders(_) => 400,
            Self::Unauthorized(PickupVerifyError::Expired) => 410,
            Self::Unauthorized(_) => 403,
            Self::HeaderMismatch(_) => 403,
            Self::FramePurged => 404,
        }
    }

    /// `frame_pickup_log.result` enum value for the DB row.
    pub fn log_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadHeaders(_) => "token_invalid",
            Self::Unauthorized(e) => e.as_log_result(),
            Self::HeaderMismatch(_) => "unauthorized",
            Self::FramePurged => "frame_purged",
        }
    }
}

/// Required headers extracted up front. Borrowed string args keep the hot
/// path allocation-free.
pub struct PickupRequest<'a> {
    pub pickup_token: Option<&'a str>,
    pub frame_ref: Option<&'a str>,
    pub service_id: Option<&'a str>,
    pub request_id: Option<&'a str>,
}

/// Single point of truth for the pickup logic — pure function over the
/// extracted headers + injected dependencies. The hyper handler in
/// `dashboard/server.rs` just maps request → `PickupRequest` and the outcome
/// back to a `Response`.
pub fn handle_pickup(
    req: PickupRequest<'_>,
    issuer: &PickupTokenIssuer,
    storage: &FrameStorage,
    db: &DbPool,
) -> PickupOutcome {
    let token = match req.pickup_token {
        Some(t) if !t.is_empty() => t,
        _ => return log_and_return(db, &req, PickupOutcome::BadHeaders("missing_token")),
    };
    let frame_ref = match req.frame_ref {
        Some(t) if !t.is_empty() => t,
        _ => return log_and_return(db, &req, PickupOutcome::BadHeaders("missing_frame_ref")),
    };
    let service_id = match req.service_id {
        Some(t) if !t.is_empty() => t,
        _ => return log_and_return(db, &req, PickupOutcome::BadHeaders("missing_service_id")),
    };
    let request_id = match req.request_id {
        Some(t) if !t.is_empty() => t,
        _ => return log_and_return(db, &req, PickupOutcome::BadHeaders("missing_request_id")),
    };

    // Verify + one-shot consume. Done BEFORE the header cross-check so that a
    // tampered header cannot exhaust a still-good token; an unauthorized
    // verdict from the issuer means the token is already useless.
    let payload = match issuer.verify_and_consume(token) {
        Ok(p) => p,
        Err(e) => return log_and_return(db, &req, PickupOutcome::Unauthorized(e)),
    };

    // Defense-in-depth: token-bound fields MUST match the headers verbatim.
    // Without this a stolen token tied to service A could be replayed against
    // service B by lying in the `X-Service-Id` header.
    if payload.raw_ref != frame_ref {
        return log_and_return(db, &req, PickupOutcome::HeaderMismatch("frame_ref_mismatch"));
    }
    if payload.service_id != service_id {
        return log_and_return(db, &req, PickupOutcome::HeaderMismatch("service_id_mismatch"));
    }
    if payload.request_id != request_id {
        return log_and_return(db, &req, PickupOutcome::HeaderMismatch("request_id_mismatch"));
    }

    let raw_ref = RawFrameRef::from_string(frame_ref.to_string());
    let stored: StoredFrame = match storage.remove(&raw_ref) {
        Some(s) => s,
        None => return log_and_return(db, &req, PickupOutcome::FramePurged),
    };

    let pf = match stored.metadata.pixel_format {
        FramePixelFormat::Rgb24 => "rgb24",
    };
    let outcome = PickupOutcome::Ok {
        bytes: stored.data,
        width: stored.metadata.width,
        height: stored.metadata.height,
        pixel_format: pf,
        timestamp_unix_ms: stored.metadata.timestamp_unix_ms,
        pts: stored.metadata.pts,
    };
    log_pickup(db, &req, &outcome);
    outcome
}

fn log_and_return(db: &DbPool, req: &PickupRequest<'_>, outcome: PickupOutcome) -> PickupOutcome {
    log_pickup(db, req, &outcome);
    outcome
}

/// Best-effort INSERT into `frame_pickup_log`. Schema (v12):
/// `(raw_frame_ref, service_id, caller_addon_id, request_id, picked_up_at, result)`.
/// `caller_addon_id` is unknown at the HTTP layer (services authenticate as
/// themselves via the token, not as the originating addon), so we leave it
/// NULL and rely on the matching `alias_calls` row to bridge addon → service.
fn log_pickup(db: &DbPool, req: &PickupRequest<'_>, outcome: &PickupOutcome) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let result = outcome.log_result();
    if let Ok(conn) = db.lock() {
        let _ = conn.execute(
            "INSERT INTO frame_pickup_log \
                 (raw_frame_ref, service_id, caller_addon_id, request_id, picked_up_at, result) \
             VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
            params![
                req.frame_ref.unwrap_or(""),
                req.service_id.unwrap_or(""),
                req.request_id.unwrap_or(""),
                ts,
                result,
            ],
        );
    }
}
