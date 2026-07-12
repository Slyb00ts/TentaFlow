// ===== File: dispatch/camera_detections.rs — camera detection overlay stream (binary) =====
//
// A streaming handler (R-STREAM): the dashboard `<tf-live-camera-tile>` sends
// one `CameraDetectionsSubscribeRequest{camera_id}` per VISIBLE tile and
// receives a long-lived stream of `CameraDetectionsFrame` chunks drained from
// the per-camera `detection_bus` broadcast. The stream stays open until the
// client cancels (`MetaCancelStream` / `StreamCloseRequest`) or disconnects —
// the writer task then drops the subscription, `push_chunk_async` errors, and
// the spawned task exits, releasing the broadcast receiver.
//
// This replaces the removed REST/WS-JSON detection routes. Detections never
// travel over REST anymore: real inference (`decoder_detect`) and the dev stub
// both publish into `detection_bus`, and only this binary stream fans them out.
//
// Scale: hundreds of cameras @ 25fps, but ONLY visible cameras are ever
// subscribed (one stream per visible tile). The handler is cheap per message —
// it maps `DetectionsMessage -> CameraDetectionsFrame` and sends. On broadcast
// lag it DROPS the missed frames (best-effort, latest-wins) and keeps going; a
// dropped overlay frame is irrelevant because the next frame overwrites it.
//
// ACL: the same gate as `camera_admin::camera_frame_url` — a user session with
// `camera.read`, a syntactically valid `cam_<uuid v4>` id, and org isolation
// via `camera_exists_in_org`. A caller without the permission, or for a camera
// outside their org, gets an immediate stream end with an error frame (the
// not-found path never echoes the camera id — cross-tenant probe defense).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tentaflow_protocol::{
    CameraAdminPayload, CameraDetectionsFrame, DetectionItem, MessageBody, ProtocolError,
    ProtocolErrorCode,
};
use tokio::sync::broadcast::error::RecvError;

use crate::services::detection_bus::{self, Detection, DetectionsMessage};
use crate::services::rbac::OrgContext;

use super::subscription::{push_chunk_async, push_end, StreamHandlerMeta, Subscription};
use super::{HandlerContext, SessionAuthKind};

const PERM_READ: &str = "camera.read";

/// Env var that turns on the dev/test detection stub. DEFAULT OFF: in
/// production only real inference publishing into `detection_bus` ever reaches
/// the browser. When set to a truthy value (`1` / `true`), the first subscribe
/// for a camera that has no real publisher spawns a single synthetic source so
/// the e2e suite can drive overlay rendering without deployed models.
const STUB_ENV: &str = "TENTAFLOW_DETECTION_STUB";

/// True when the dev/test detection stub is enabled via env. Read on every
/// subscribe (cheap) so tests can flip it without a process restart.
fn detection_stub_enabled() -> bool {
    matches!(
        std::env::var(STUB_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

/// Process-wide registry of running stub tasks keyed by camera id. Guarantees
/// at most ONE stub task per camera no matter how many tiles subscribe (or how
/// often a tile re-subscribes after scrolling out and back in). Tasks live for
/// the process lifetime — a stub is a fixed-rate fake source, not tied to any
/// single subscription.
fn stub_registry() -> &'static Mutex<HashMap<String, tokio::task::JoinHandle<()>>> {
    static REG: OnceLock<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ensures exactly one stub task exists for `camera_id`. A finished/aborted
/// handle is replaced; a live one is left untouched so repeated subscribes do
/// not multiply the source.
fn ensure_stub(camera_id: &str) {
    let mut reg = stub_registry().lock().unwrap();
    if let Some(handle) = reg.get(camera_id) {
        if !handle.is_finished() {
            return;
        }
    }
    let handle = detection_bus::spawn_detection_stub(camera_id.to_string());
    reg.insert(camera_id.to_string(), handle);
}

/// Camera id validator matching the wire format minted across the codebase:
/// `cam_<uuid v4>` (40 chars). Kept local — same rules as
/// `camera_admin::validate_camera_id` — so this module does not reach across
/// into the admin RPC's private helper.
fn validate_camera_id(id: &str) -> bool {
    if id.len() != 40 || !id.starts_with("cam_") {
        return false;
    }
    let bytes = id.as_bytes();
    for (i, &b) in bytes[4..].iter().enumerate() {
        let dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if dash_pos {
            if b != b'-' {
                return false;
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return false;
        }
    }
    bytes[4 + 14] == b'4' && matches!(bytes[4 + 19], b'8' | b'9' | b'a' | b'b')
}

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

/// Validates the subscribe request and resolves the camera id after the ACL
/// check. Returns the camera id to subscribe under, or an error to send as an
/// immediate stream end.
fn authorize(ctx: &HandlerContext, camera_id: &str) -> Result<String, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "camera.read permission required",
        ));
    }
    if !validate_camera_id(camera_id) {
        return Err(ProtocolError::bad_request("camera_id_invalid_format"));
    }
    let exists =
        crate::db::repository::camera_exists_in_org(&ctx.state.db, camera_id, &org.org_id)
            .map_err(|e| ProtocolError::internal(format!("camera lookup failed: {e}")))?;
    if !exists {
        // Static reason — never echo the camera id (cross-tenant probe defense).
        return Err(ProtocolError::not_found("camera_not_found"));
    }
    Ok(camera_id.to_string())
}

/// Maps one bus message into the wire frame. Allocation-cheap: it moves the
/// per-frame `Vec` and the per-item strings out of the broadcast clone.
pub(crate) fn to_wire(msg: DetectionsMessage) -> CameraDetectionsFrame {
    CameraDetectionsFrame {
        camera_id: msg.camera_id,
        ts_ms: msg.ts_ms,
        pts_ns: msg.pts_ns,
        proc_ms: msg.proc_ms,
        items: msg.items.into_iter().map(item_to_wire).collect(),
    }
}

fn item_to_wire(d: Detection) -> DetectionItem {
    DetectionItem {
        klasa: d.klasa,
        bbox: d.bbox,
        score: d.score,
        stan: d.stan,
        tekst: d.tekst,
        tekst_conf: d.tekst_conf,
        track_id: d.track_id,
        vx: d.vx,
        vy: d.vy,
    }
}

fn camera_detections_subscribe_handler(
    req: MessageBody,
    ctx: HandlerContext,
    sub: Arc<Subscription>,
) {
    let camera_id = match req {
        MessageBody::CameraAdminBody(CameraAdminPayload::DetectionsSubscribeRequest(r)) => {
            r.camera_id
        }
        _ => {
            let _ = push_end(
                &sub,
                Some(MessageBody::Error(ProtocolError::bad_request(
                    "expected CameraDetectionsSubscribeRequest",
                ))),
            );
            return;
        }
    };

    tokio::spawn(async move {
        // ACL siega do rusqlite (camera_exists_in_org, sync) — biegnie na puli
        // blocking, zeby nie blokowac watkow tokio.
        let auth_camera_id = camera_id.clone();
        let camera_id = match tokio::task::spawn_blocking(move || authorize(&ctx, &auth_camera_id))
            .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(err)) => {
                let _ = push_end(&sub, Some(MessageBody::Error(err)));
                return;
            }
            Err(e) => {
                let _ = push_end(
                    &sub,
                    Some(MessageBody::Error(ProtocolError::internal(format!(
                        "camera authorization task failed: {e}"
                    )))),
                );
                return;
            }
        };

        // Production path: start the always-on RF-DETR analysis loop for this
        // camera (idempotent — one task per camera regardless of subscribers).
        // Real detections flow into `detection_bus` and out through this
        // stream. A camera owned by the vision-worker fleet runs its analysis
        // in the worker process, which relays detections into this SAME bus —
        // starting a local loop too would double-publish overlays.
        #[cfg(feature = "inference-vision-gpu")]
        {
            #[cfg(all(unix, feature = "camera"))]
            let worker_owned =
                crate::services::vision_worker::fleet::is_worker_camera(&camera_id).is_some();
            #[cfg(not(all(unix, feature = "camera")))]
            let worker_owned = false;
            if !worker_owned {
                crate::services::camera_ingest::vision_analysis::ensure_analysis(&camera_id);
            }
        }

        // Dev/test only, behind the env flag (default off): when no real detector
        // publishes for this camera, spawn one synthetic source so the e2e suite
        // sees overlay data without deployed models. The registry keeps it to one
        // task per camera across re-subscribes. Production (flag off) only ever
        // streams real inference.
        if detection_stub_enabled() {
            ensure_stub(&camera_id);
        }

        let mut rx = detection_bus::subscribe(&camera_id);
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let body = MessageBody::CameraAdminBody(CameraAdminPayload::DetectionsFrame(
                        to_wire(msg),
                    ));
                    // Receiver closed (client cancelled / disconnected) → stop
                    // draining the bus so the broadcast ring can be reclaimed.
                    if push_chunk_async(&sub, body).await.is_err() {
                        return;
                    }
                }
                // Slow subscriber fell behind the broadcast ring: DROP the
                // missed frames and keep going. Overlays are latest-wins, so a
                // skipped frame is irrelevant — the next one overwrites it.
                // Never block, never end the stream on lag.
                Err(RecvError::Lagged(_)) => continue,
                // The sender is process-wide and never closes for a healthy
                // node, but handle it defensively.
                Err(RecvError::Closed) => break,
            }
        }
    });

    // No `push_end` here: the subscription lives until the client cancels or
    // disconnects. The spawned task drains until the WS writer drops the
    // receiver.
}

inventory::submit! {
    StreamHandlerMeta {
        variant_name: "CameraDetectionsSubscribeRequest",
        required_auth: SessionAuthKind::UserSession,
        handler_fn: camera_detections_subscribe_handler,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_handler_is_registered_and_user_gated() {
        let meta = crate::dispatch::subscription::find_stream_handler(
            "CameraDetectionsSubscribeRequest",
        )
        .expect("camera-detections stream handler registered in inventory");
        assert_eq!(meta.required_auth, SessionAuthKind::UserSession);
    }

    #[test]
    fn validate_camera_id_accepts_canonical_and_rejects_junk() {
        assert!(validate_camera_id("cam_550e8400-e29b-41d4-a716-446655440000"));
        assert!(!validate_camera_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!validate_camera_id("cam_short"));
        // Version nibble must be 4.
        assert!(!validate_camera_id("cam_550e8400-e29b-31d4-a716-446655440000"));
        // Variant nibble must be 8/9/a/b.
        assert!(!validate_camera_id("cam_550e8400-e29b-41d4-c716-446655440000"));
    }

    #[test]
    fn stub_disabled_by_default() {
        // No env set in the default test process → stub stays off.
        std::env::remove_var(STUB_ENV);
        assert!(!detection_stub_enabled());
    }

    #[test]
    fn to_wire_mirrors_detection_fields() {
        let msg = DetectionsMessage {
            msg_type: "detections",
            camera_id: "cam_550e8400-e29b-41d4-a716-446655440000".into(),
            ts_ms: 1_700_000_000_123,
            pts_ns: Some(1_234_567_890),
            proc_ms: 42,
            items: vec![
                Detection {
                    klasa: "tablica_adr".into(),
                    bbox: [0.41, 0.22, 0.12, 0.06],
                    score: 0.96,
                    stan: Vec::new(),
                    tekst: Some("30/1202".into()),
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 7,
                    vehicle_id: 0,
                    vx: 0.01,
                    vy: -0.02,
                },
                Detection {
                    klasa: "nalepka_3".into(),
                    bbox: [0.30, 0.15, 0.05, 0.07],
                    score: 0.94,
                    stan: vec!["uszkodzona".into()],
                    tekst: None,
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 0,
                    vehicle_id: 0,
                    vx: 0.,
                    vy: 0.,
                },
            ],
        };
        let frame = to_wire(msg);
        assert_eq!(frame.camera_id, "cam_550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(frame.ts_ms, 1_700_000_000_123);
        assert_eq!(frame.items.len(), 2);
        assert_eq!(frame.items[0].klasa, "tablica_adr");
        assert_eq!(frame.items[0].bbox, [0.41, 0.22, 0.12, 0.06]);
        assert_eq!(frame.items[0].score, 0.96);
        assert!(frame.items[0].stan.is_empty());
        assert_eq!(frame.items[0].tekst.as_deref(), Some("30/1202"));
        assert_eq!(frame.items[1].stan, vec!["uszkodzona".to_string()]);
        assert!(frame.items[1].tekst.is_none());
    }

    // End-to-end of the handler-side mapping path: a publish on the bus is
    // received by the subscriber, mapped through `to_wire`, and a Lagged event
    // (forced by overrunning the ring) is handled WITHOUT erroring — exactly
    // the loop's behavior in the spawned task.
    #[tokio::test]
    async fn published_detection_maps_to_frame_and_lag_is_dropped() {
        let cam = "cam_550e8400-e29b-41d4-a716-44665544aaaa";
        let mut rx = detection_bus::subscribe(cam);
        detection_bus::publish_detections(
            cam,
            0,
            None,
            0,
            vec![Detection {
                klasa: "termometr".into(),
                bbox: [0.1, 0.1, 0.2, 0.2],
                score: 0.5,
                stan: Vec::new(),
                tekst: None,
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.,
                vy: 0.,
            }],
        );

        let frame = match rx.recv().await {
            Ok(msg) => to_wire(msg),
            other => panic!("expected a detection message, got {other:?}"),
        };
        assert_eq!(frame.camera_id, cam);
        assert_eq!(frame.items.len(), 1);
        assert_eq!(frame.items[0].klasa, "termometr");

        // Force the broadcast ring to overrun this slow receiver so the next
        // recv yields Lagged; the loop treats it as a no-op drop.
        for _ in 0..200 {
            detection_bus::publish_detections(
                cam,
                0,
                None,
                0,
                vec![Detection {
                    klasa: "nalepka_9".into(),
                    bbox: [0.0, 0.0, 0.1, 0.1],
                    score: 0.9,
                    stan: Vec::new(),
                    tekst: None,
                    tekst_conf: None,
                    tekst_thumb_ref: None,
                    track_id: 0,
                    vehicle_id: 0,
                    vx: 0.,
                    vy: 0.,
                }],
            );
        }
        let mut saw_lag = false;
        loop {
            match rx.recv().await {
                Ok(_) => continue,
                Err(RecvError::Lagged(n)) => {
                    assert!(n > 0, "lag count should be positive");
                    saw_lag = true;
                    break;
                }
                Err(RecvError::Closed) => break,
            }
        }
        assert!(saw_lag, "overrunning the ring must surface a Lagged error");
    }
}
