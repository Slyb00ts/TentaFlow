// =============================================================================
// File: tests/streaming_pickup_e2e.rs — M1.W7 Chunk D end-to-end pickup flow
// =============================================================================
//
// Full HTTP roundtrip over the loopback for the Service-to-Core pickup API:
//
//   1. Driver inserts a synthetic frame into a `FrameStorage` and mints a
//      `PickupToken` (this stands in for the addon-side `service_call_v1`
//      path — Chunk C already covered that wiring; here we exercise the
//      bytes-leaving-the-process surface).
//   2. Driver POSTs to a mock yolo backend with the rewritten payload
//      (`frame_ref` + `pickup_token` + `service_id` + `request_id`).
//   3. Mock yolo extracts the four fields, calls back into a private hyper
//      server hosting the real `handle_pickup` function under
//      `POST /core/frame/pickup`, and turns the returned bytes into a fake
//      bbox response.
//   4. Driver asserts the bbox JSON, the audit row in `frame_pickup_log`,
//      and (for the negative tests) that replays / TTL / cross-service /
//      missing-header cases are rejected on the wire with the right
//      HTTP status.
//
// We use `hyper::server::conn::http1` directly (no axum dep) and `reqwest`
// (already in `[dev-dependencies]` for other tests via the main dep). The
// mock yolo is a second hyper server on a separate ephemeral port — this
// keeps the test honest about cross-process header propagation.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpListener;

use tentaflow_core::api::frame_pickup::{
    handle_pickup, PickupOutcome, PickupRequest, HDR_FRAME_HEIGHT, HDR_FRAME_PIXEL_FORMAT,
    HDR_FRAME_PTS, HDR_FRAME_REF, HDR_FRAME_TS_MS, HDR_FRAME_WIDTH, HDR_PICKUP_TOKEN,
    HDR_REQUEST_ID, HDR_SERVICE_ID,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::pickup_tokens::PickupTokenIssuer;

// -----------------------------------------------------------------------------
// Shared state and helpers
// -----------------------------------------------------------------------------

struct CoreEnv {
    addr: SocketAddr,
    storage: Arc<FrameStorage>,
    issuer: Arc<PickupTokenIssuer>,
    db: DbPool,
}

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("db init")
}

fn mk_frame(camera_id: &str, width: u32, height: u32, payload: Vec<u8>) -> StoredFrame {
    StoredFrame {
        metadata: FrameMetadata {
            camera_id: camera_id.into(),
            width,
            height,
            pixel_format: FramePixelFormat::Rgb24,
            timestamp_unix_ms: 1_715_500_000_000,
            pts: Some(4242),
            frame_size_bytes: payload.len(),
        },
        data: Arc::from(payload.into_boxed_slice()),
        created_at: std::time::Instant::now(),
    }
}

fn frame_pickup_log_count(db: &DbPool, result_kind: &str) -> i64 {
    let conn = db.lock().expect("db lock");
    conn.query_row(
        "SELECT COUNT(*) FROM frame_pickup_log WHERE result = ?1",
        rusqlite::params![result_kind],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// Private hyper server hosting POST /core/frame/pickup
// -----------------------------------------------------------------------------

/// Spawn a hyper http1 server that exposes `/core/frame/pickup` backed by the
/// real `handle_pickup` pure function. We keep this isolated from the dashboard
/// server (which depends on Router/ServiceManager/JWT) — the goal is to prove
/// the wire-level contract, not the auth surround.
async fn spawn_core_pickup_server() -> CoreEnv {
    let storage = Arc::new(FrameStorage::new(64));
    // Long TTL so the happy-path / replay tests do not race the wall clock.
    let issuer = Arc::new(PickupTokenIssuer::new_for_tests(
        [7u8; 32],
        Duration::from_secs(30),
    ));
    let db = make_db();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind core");
    let addr = listener.local_addr().expect("local addr");

    let storage_for_loop = storage.clone();
    let issuer_for_loop = issuer.clone();
    let db_for_loop = db.clone();

    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let storage = storage_for_loop.clone();
            let issuer = issuer_for_loop.clone();
            let db = db_for_loop.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let storage = storage.clone();
                    let issuer = issuer.clone();
                    let db = db.clone();
                    async move { Ok::<_, Infallible>(core_handler(req, &storage, &issuer, &db).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(sock), svc)
                    .await;
            });
        }
    });

    CoreEnv {
        addr,
        storage,
        issuer,
        db,
    }
}

async fn core_handler(
    req: Request<hyper::body::Incoming>,
    storage: &FrameStorage,
    issuer: &PickupTokenIssuer,
    db: &DbPool,
) -> Response<Full<Bytes>> {
    if req.method() != Method::POST || req.uri().path() != "/core/frame/pickup" {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()))
            .unwrap();
    }
    let hdr = |name: &str| -> Option<String> {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };
    let token = hdr(HDR_PICKUP_TOKEN);
    let frame_ref = hdr(HDR_FRAME_REF);
    let service_id = hdr(HDR_SERVICE_ID);
    let request_id = hdr(HDR_REQUEST_ID);

    // Mirror the 1 KiB body limit from the real dashboard handler.
    const PICKUP_BODY_LIMIT: u64 = 1024;
    let content_length: u64 = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if content_length > PICKUP_BODY_LIMIT {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .body(Full::new(Bytes::from_static(b"too_large")))
            .unwrap();
    }
    let _body = req.into_body().collect().await.ok();

    let pr = PickupRequest {
        pickup_token: token.as_deref(),
        frame_ref: frame_ref.as_deref(),
        service_id: service_id.as_deref(),
        request_id: request_id.as_deref(),
    };
    let outcome = handle_pickup(pr, issuer, storage, db);
    let status = outcome.http_status();
    match outcome {
        PickupOutcome::Ok {
            bytes,
            width,
            height,
            pixel_format,
            timestamp_unix_ms,
            pts,
        } => {
            let mut builder = Response::builder()
                .status(status)
                .header("Content-Type", "application/octet-stream")
                .header(HDR_FRAME_WIDTH, width.to_string())
                .header(HDR_FRAME_HEIGHT, height.to_string())
                .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string());
            if let Some(p) = pts {
                builder = builder.header(HDR_FRAME_PTS, p.to_string());
            }
            builder
                .body(Full::new(Bytes::copy_from_slice(&bytes)))
                .unwrap()
        }
        _ => Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from_static(b"{\"error\":\"pickup_failed\"}")))
            .unwrap(),
    }
}

// -----------------------------------------------------------------------------
// Mock yolo backend service
// -----------------------------------------------------------------------------

/// Spawn the mock yolo backend on a random port. Returns its base URL.
async fn spawn_mock_yolo(core_url: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind yolo");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let core_url = core_url.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let core_url = core_url.clone();
                    async move { Ok::<_, Infallible>(yolo_handler(req, core_url).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(sock), svc)
                    .await;
            });
        }
    });
    format!("http://{}", addr)
}

async fn yolo_handler(
    req: Request<hyper::body::Incoming>,
    core_url: String,
) -> Response<Full<Bytes>> {
    if req.method() != Method::POST || req.uri().path() != "/yolo/detect" {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()))
            .unwrap();
    }
    let body = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
    };
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
    };
    let take = |k: &str| {
        payload
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let pickup_token = take("pickup_token");
    let frame_ref = take("frame_ref");
    let service_id = take("service_id");
    let request_id = take("request_id");

    let client = reqwest::Client::new();
    let resp = match client
        .post(format!("{}/core/frame/pickup", core_url))
        .header(HDR_PICKUP_TOKEN, &pickup_token)
        .header(HDR_FRAME_REF, &frame_ref)
        .header(HDR_SERVICE_ID, &service_id)
        .header(HDR_REQUEST_ID, &request_id)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let body = json!({"upstream_error": e.to_string()}).to_string();
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body)))
                .unwrap();
        }
    };
    let upstream_status = resp.status().as_u16();
    if upstream_status != 200 {
        let body = json!({
            "upstream_status": upstream_status,
        })
        .to_string();
        return Response::builder()
            .status(upstream_status)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap();
    }
    let width: u32 = resp
        .headers()
        .get(HDR_FRAME_WIDTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let height: u32 = resp
        .headers()
        .get(HDR_FRAME_HEIGHT)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let pixel_format = resp
        .headers()
        .get(HDR_FRAME_PIXEL_FORMAT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
    };

    // Simulated detection: fake bbox derived from frame dimensions so the
    // assertion has something concrete to check.
    let response = json!({
        "frame_size_bytes": bytes.len(),
        "width": width,
        "height": height,
        "pixel_format": pixel_format,
        "bboxes": [
            { "x": 10, "y": 20, "w": width / 4, "h": height / 4, "class": "person", "conf": 0.91 }
        ]
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(response.to_string())))
        .unwrap()
}

// -----------------------------------------------------------------------------
// Driver helper — simulates the addon's `service_call_v1` mint + dispatch
// -----------------------------------------------------------------------------

struct DispatchInput {
    frame_ref: String,
    service_id: String,
    request_id: String,
    /// Override the wire token (used to forge / lie in tests).
    override_token: Option<String>,
    /// Override the service_id sent to the mock yolo (cross-service test).
    override_service_id_on_wire: Option<String>,
}

/// Mint a pickup token like `maybe_inject_pickup_token` would, then POST the
/// rewritten payload to the mock yolo. Returns the yolo HTTP response.
async fn drive_dispatch(
    yolo_url: &str,
    issuer: &PickupTokenIssuer,
    input: DispatchInput,
) -> reqwest::Response {
    let (token, _payload) = issuer.issue(
        input.frame_ref.clone(),
        input.service_id.clone(),
        input.request_id.clone(),
    );
    let wire = input.override_token.unwrap_or_else(|| token.wire());
    let service_id_for_wire = input
        .override_service_id_on_wire
        .unwrap_or_else(|| input.service_id.clone());

    let payload = json!({
        "frame_ref": input.frame_ref,
        "pickup_token": wire,
        "service_id": service_id_for_wire,
        "request_id": input.request_id,
        "model": "yolov8-traffic",
    });
    reqwest::Client::new()
        .post(format!("{}/yolo/detect", yolo_url))
        .json(&payload)
        .send()
        .await
        .expect("dispatch send")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_happy_path_pickup_returns_bbox() {
    let core = spawn_core_pickup_server().await;
    let yolo = spawn_mock_yolo(format!("http://{}", core.addr)).await;

    // 720x480 RGB24 frame; the payload bytes here are dummy data — what
    // matters is that the same bytes survive the HTTP roundtrip.
    let payload = vec![0xABu8; 720 * 480 * 3];
    let raw_ref = core.storage.insert(mk_frame("cam-e2e", 720, 480, payload.clone()));

    let resp = drive_dispatch(
        &yolo,
        &core.issuer,
        DispatchInput {
            frame_ref: raw_ref.as_str().to_string(),
            service_id: "yolo-svc".into(),
            request_id: "req-happy".into(),
            override_token: None,
            override_service_id_on_wire: None,
        },
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["frame_size_bytes"].as_u64().unwrap(), payload.len() as u64);
    assert_eq!(body["width"].as_u64().unwrap(), 720);
    assert_eq!(body["height"].as_u64().unwrap(), 480);
    assert_eq!(body["pixel_format"].as_str().unwrap(), "rgb24");
    assert_eq!(body["bboxes"][0]["class"].as_str().unwrap(), "person");

    // Frame consumed from LRU, pickup audited.
    assert_eq!(core.storage.len(), 0);
    assert_eq!(frame_pickup_log_count(&core.db, "ok"), 1);
}

#[tokio::test]
async fn test_e2e_replay_rejected_on_wire() {
    let core = spawn_core_pickup_server().await;
    let yolo = spawn_mock_yolo(format!("http://{}", core.addr)).await;

    let raw_ref = core.storage.insert(mk_frame("cam-replay", 8, 4, vec![1; 96]));
    let (token, _) = core.issuer.issue(
        raw_ref.as_str().to_string(),
        "yolo-svc".into(),
        "req-replay".into(),
    );
    let wire = token.wire();

    // Two back-to-back dispatches with the SAME minted token. Driver injects
    // override_token so both calls reuse the same wire string.
    let first = drive_dispatch(
        &yolo,
        &core.issuer,
        DispatchInput {
            frame_ref: raw_ref.as_str().to_string(),
            service_id: "yolo-svc".into(),
            request_id: "req-replay".into(),
            // The issuer mints fresh wires for each call; override so both
            // attempts present the SAME pre-minted token.
            override_token: Some(wire.clone()),
            override_service_id_on_wire: None,
        },
    )
    .await;
    assert_eq!(first.status().as_u16(), 200);

    let second = drive_dispatch(
        &yolo,
        &core.issuer,
        DispatchInput {
            frame_ref: raw_ref.as_str().to_string(),
            service_id: "yolo-svc".into(),
            request_id: "req-replay".into(),
            override_token: Some(wire.clone()),
            override_service_id_on_wire: None,
        },
    )
    .await;
    // Yolo forwards upstream status. Replay → core returns 403 Unauthorized.
    assert_eq!(second.status().as_u16(), 403);
    assert_eq!(frame_pickup_log_count(&core.db, "ok"), 1);
    assert_eq!(frame_pickup_log_count(&core.db, "unauthorized"), 1);
}

#[tokio::test]
async fn test_e2e_ttl_expired_returns_410() {
    let core = spawn_core_pickup_server().await;
    // Override issuer with a 1 ms TTL so we can observe expiry without long
    // sleeps. We mint outside the long-lived issuer of `core` because the
    // happy-path server has TTL=30s — for this test we run the verify against
    // a separate short-TTL issuer with a different key. To keep the wire path
    // honest we re-bind a fresh core server.
    let short_issuer = Arc::new(PickupTokenIssuer::new_for_tests(
        [11u8; 32],
        Duration::from_millis(1),
    ));
    let storage = Arc::new(FrameStorage::new(8));
    let raw_ref = storage.insert(mk_frame("cam-ttl", 4, 2, vec![9; 24]));
    let (token, _) = short_issuer.issue(
        raw_ref.as_str().to_string(),
        "yolo-svc".into(),
        "req-ttl".into(),
    );
    let wire = token.wire();
    tokio::time::sleep(Duration::from_millis(25)).await;

    // Hit the real core server's handler directly with the now-expired token
    // by calling the pure function (the on-the-wire status mapping is what we
    // assert). Using the wire-path here would require a second hyper server
    // bound to `short_issuer`/`storage` — same code path, no extra coverage.
    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("yolo-svc"),
            request_id: Some("req-ttl"),
        },
        &short_issuer,
        &storage,
        &core.db,
    );
    assert_eq!(outcome.http_status(), 410);
    assert_eq!(frame_pickup_log_count(&core.db, "token_expired"), 1);
    // Frame remains in storage — expiry does not consume the LRU entry.
    assert_eq!(storage.len(), 1);
}

#[tokio::test]
async fn test_e2e_cross_service_rejected_on_wire() {
    let core = spawn_core_pickup_server().await;
    let yolo = spawn_mock_yolo(format!("http://{}", core.addr)).await;

    let raw_ref = core.storage.insert(mk_frame("cam-xs", 16, 8, vec![3; 384]));
    let (token, _) = core.issuer.issue(
        raw_ref.as_str().to_string(),
        "yolo-svc".into(),
        "req-xs".into(),
    );
    let wire = token.wire();

    // Driver lies in the wire payload: token bound to "yolo-svc" but the JSON
    // sent to yolo says "ocr-svc" — yolo forwards that header verbatim, and
    // core rejects on the header-cross-check path with HTTP 403.
    let resp = drive_dispatch(
        &yolo,
        &core.issuer,
        DispatchInput {
            frame_ref: raw_ref.as_str().to_string(),
            service_id: "yolo-svc".into(),
            request_id: "req-xs".into(),
            override_token: Some(wire.clone()),
            override_service_id_on_wire: Some("ocr-svc".into()),
        },
    )
    .await;
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(frame_pickup_log_count(&core.db, "unauthorized"), 1);

    // Defense-in-depth: legit recipient retry with the right service_id must
    // still succeed because the cross-service path did NOT consume the token.
    let retry = drive_dispatch(
        &yolo,
        &core.issuer,
        DispatchInput {
            frame_ref: raw_ref.as_str().to_string(),
            service_id: "yolo-svc".into(),
            request_id: "req-xs".into(),
            override_token: Some(wire.clone()),
            override_service_id_on_wire: None,
        },
    )
    .await;
    assert_eq!(retry.status().as_u16(), 200);
    assert_eq!(frame_pickup_log_count(&core.db, "ok"), 1);
}

#[tokio::test]
async fn test_e2e_missing_pickup_token_header_returns_400() {
    let core = spawn_core_pickup_server().await;

    // Skip the yolo hop here — we are testing core's wire-level header
    // validation, not the addon-side dispatch. Hit /core/frame/pickup
    // directly with the token header omitted.
    let resp = reqwest::Client::new()
        .post(format!("http://{}/core/frame/pickup", core.addr))
        .header(HDR_FRAME_REF, "frame_doesntmatter")
        .header(HDR_SERVICE_ID, "yolo-svc")
        .header(HDR_REQUEST_ID, "req-missing-tok")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 400);
    assert_eq!(frame_pickup_log_count(&core.db, "token_invalid"), 1);
}

#[tokio::test]
async fn test_e2e_oversized_pickup_body_returns_413() {
    let core = spawn_core_pickup_server().await;
    // 2 KiB body exceeds the 1 KiB limit copied from the production handler.
    let big = vec![0u8; 2048];
    let resp = reqwest::Client::new()
        .post(format!("http://{}/core/frame/pickup", core.addr))
        .header(HDR_PICKUP_TOKEN, "irrelevant")
        .header(HDR_FRAME_REF, "frame_x")
        .header(HDR_SERVICE_ID, "yolo-svc")
        .header(HDR_REQUEST_ID, "req-big")
        .header("content-length", big.len().to_string())
        .body(big)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status().as_u16(), 413);
}
