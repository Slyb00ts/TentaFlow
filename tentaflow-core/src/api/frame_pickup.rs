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
use crate::services::pickup_tokens::{
    PickupTokenIssuer, PickupVerifyError, TokenPayload, VerifySource,
};

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
    /// F1b P3.C-3 — peer reported NotFound for a cross-node fetch.
    UpstreamNotFound,
    /// F1b P3.C-3 — peer reported Unavailable / dispatch failure / timeout.
    /// Caller MUST send `Retry-After: 5` along with the 503.
    UpstreamUnavailable(&'static str),
    /// F1b P3.C-3 — cross-node B-side replay protection rejected the token
    /// (a previous mesh-fallback consume for the same wire already won).
    Replay,
}

impl PickupOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadHeaders(_) => 400,
            Self::Unauthorized(PickupVerifyError::Expired) => 410,
            Self::Unauthorized(_) => 403,
            Self::HeaderMismatch(_) => 403,
            Self::FramePurged | Self::UpstreamNotFound => 404,
            Self::UpstreamUnavailable(_) => 503,
            Self::Replay => 403,
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
            Self::UpstreamNotFound => "frame_purged",
            Self::UpstreamUnavailable(_) => "upstream_unavailable",
            Self::Replay => "replay",
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

/// F1b P3.C-3 — outcome of the header-verify split. On success the caller
/// inspects the `VerifySource` to decide between the local fast path
/// (`handle_pickup`) and the cross-node mesh-fallback path
/// (`dashboard/server.rs` → `frame_proxy::client::fetch_from_peer`). On
/// failure the caller emits the same audit row + HTTP response it would
/// have emitted before the split — the failure-mode contract is unchanged.
#[derive(Debug)]
pub struct VerifiedPickup {
    pub token: String,
    pub payload: TokenPayload,
    pub source: VerifySource,
}

/// F1b P3.C-3 — header extraction + HMAC verify + header cross-check, WITHOUT
/// consuming the one-shot bit and WITHOUT touching the local LRU. The hyper
/// handler runs this first; on `Ok(VerifiedPickup)` it dispatches by
/// `source` (Local → `handle_pickup`, Peer → frame proxy fetch). On `Err`
/// the audit row is already written and the caller maps the outcome to the
/// HTTP response.
pub fn verify_pickup_headers(
    req: &PickupRequest<'_>,
    issuer: &PickupTokenIssuer,
    db: &DbPool,
) -> Result<VerifiedPickup, PickupOutcome> {
    let token = match req.pickup_token {
        Some(t) if !t.is_empty() => t,
        _ => return Err(log_outcome(db, req, PickupOutcome::BadHeaders("missing_token"), None)),
    };
    let frame_ref = match req.frame_ref {
        Some(t) if !t.is_empty() => t,
        _ => return Err(log_outcome(db, req, PickupOutcome::BadHeaders("missing_frame_ref"), None)),
    };
    let service_id = match req.service_id {
        Some(t) if !t.is_empty() => t,
        _ => return Err(log_outcome(db, req, PickupOutcome::BadHeaders("missing_service_id"), None)),
    };
    let request_id = match req.request_id {
        Some(t) if !t.is_empty() => t,
        _ => return Err(log_outcome(db, req, PickupOutcome::BadHeaders("missing_request_id"), None)),
    };

    let (payload, source) = match issuer.verify_only_with_source(token) {
        Ok(p) => p,
        Err(e) => return Err(log_outcome(db, req, PickupOutcome::Unauthorized(e), None)),
    };
    let source_node = match &source {
        VerifySource::Local => None,
        VerifySource::Peer(id) => Some(id.clone()),
    };
    if payload.raw_ref != frame_ref {
        return Err(log_outcome(
            db,
            req,
            PickupOutcome::HeaderMismatch("frame_ref_mismatch"),
            source_node,
        ));
    }
    if payload.service_id != service_id {
        return Err(log_outcome(
            db,
            req,
            PickupOutcome::HeaderMismatch("service_id_mismatch"),
            source_node,
        ));
    }
    if payload.request_id != request_id {
        return Err(log_outcome(
            db,
            req,
            PickupOutcome::HeaderMismatch("request_id_mismatch"),
            source_node,
        ));
    }
    Ok(VerifiedPickup {
        token: token.to_string(),
        payload,
        source,
    })
}

/// Local-source pickup. Header verify already passed; consume the one-shot
/// inflight entry, then remove the bytes from the local LRU. Cross-node
/// callers go through `dashboard/server.rs` proxy dispatch instead — this
/// function never reaches a peer.
pub fn handle_pickup(
    req: PickupRequest<'_>,
    issuer: &PickupTokenIssuer,
    storage: &FrameStorage,
    db: &DbPool,
) -> PickupOutcome {
    let verified = match verify_pickup_headers(&req, issuer, db) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    // Local path only — Peer source is handled by the HTTP layer dispatch.
    debug_assert!(matches!(verified.source, VerifySource::Local));

    if let Err(e) = issuer.consume_one_shot(&verified.token) {
        return log_outcome(db, &req, PickupOutcome::Unauthorized(e), None);
    }

    let raw_ref = RawFrameRef::from_string(verified.payload.raw_ref.clone());
    let stored: StoredFrame = match storage.remove(&raw_ref) {
        Some(s) => s,
        None => return log_outcome(db, &req, PickupOutcome::FramePurged, None),
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
    log_pickup(db, &req, &outcome, None);
    outcome
}

/// Same-shape helper for the HTTP-layer cross-node path: write the
/// `frame_pickup_log` row with `source_node_id` set, then return the outcome.
pub fn log_outcome(
    db: &DbPool,
    req: &PickupRequest<'_>,
    outcome: PickupOutcome,
    source_node_id: Option<String>,
) -> PickupOutcome {
    log_pickup(db, req, &outcome, source_node_id.as_deref());
    outcome
}

/// Best-effort INSERT into `frame_pickup_log`. Schema (v12 + v24):
/// `(raw_frame_ref, service_id, caller_addon_id, request_id, picked_up_at,
///   result, source_node_id)`.
/// `caller_addon_id` is unknown at the HTTP layer (services authenticate as
/// themselves via the token, not as the originating addon), so we leave it
/// NULL and rely on the matching `alias_calls` row to bridge addon → service.
/// `source_node_id` is `Some(<peer>)` only for cross-node mesh-fallback
/// pickups (F1b P3.C-3); local-source pickups leave it NULL.
fn log_pickup(
    db: &DbPool,
    req: &PickupRequest<'_>,
    outcome: &PickupOutcome,
    source_node_id: Option<&str>,
) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let result = outcome.log_result();
    if let Ok(conn) = db.lock() {
        let _ = conn.execute(
            "INSERT INTO frame_pickup_log \
                 (raw_frame_ref, service_id, caller_addon_id, request_id, \
                  picked_up_at, result, source_node_id) \
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
            params![
                req.frame_ref.unwrap_or(""),
                req.service_id.unwrap_or(""),
                req.request_id.unwrap_or(""),
                ts,
                result,
                source_node_id,
            ],
        );
    }
}
