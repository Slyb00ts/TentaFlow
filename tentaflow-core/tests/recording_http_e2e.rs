// =============================================================================
// File: tests/recording_http_e2e.rs — M1.W8 Chunk D HTTP signed-URL e2e
// =============================================================================
//
// Full HTTP roundtrip over loopback for the addon-facing signed-URL endpoints:
//
//   * GET /recordings/<ref>?token=&exp=&ref=   (snapshot PNG / segment MP4)
//   * GET /frames/<ref>?token=&exp=&ref=       (raw RGB24 multi-use)
//
// We spin up a thin hyper http1 server backed by the **real** pure handlers
// (`api::recording::handle_recording_url` + `api::frames::handle_frame_url`),
// drive it with `reqwest`, and assert wire status + headers + body + the
// `audit_log` chain. Token tampering / expiry / multi-fetch / purged-row /
// missing-query / evicted-frame cases all flow through the real hyper wire,
// not the in-process pure function — these are e2e, not unit.

#![cfg(feature = "camera")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use tentaflow_core::api::frames::{
    handle_frame_url, parse_query as parse_frame_query, FrameOutcome, HDR_FRAME_HEIGHT,
    HDR_FRAME_PIXEL_FORMAT, HDR_FRAME_TS_MS, HDR_FRAME_WIDTH,
};
use tentaflow_core::api::recording::{
    handle_recording_url, parse_query as parse_rec_query, read_recording_file,
    RecordingFileOutcome, RecordingOutcome,
};
use tentaflow_core::db::repository::insert_recording;
use tentaflow_core::db::DbPool;
use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::recording::save_snapshot_rgb24;
use tentaflow_core::services::signed_urls::{SignedUrlIssuer, UrlScope};

// -----------------------------------------------------------------------------
// Test harness
// -----------------------------------------------------------------------------

struct Env {
    addr: SocketAddr,
    db: DbPool,
    rec_issuer: Arc<SignedUrlIssuer>,
    frame_issuer: Arc<SignedUrlIssuer>,
    storage: Arc<FrameStorage>,
}

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("db init")
}

fn audit_log_count(db: &DbPool, action: &str, result: &str) -> i64 {
    let conn = db.lock().expect("db lock");
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE action = ?1 AND result = ?2",
        rusqlite::params![action, result],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

async fn spawn_server() -> Env {
    let db = make_db();
    let rec_issuer = Arc::new(SignedUrlIssuer::new_for_tests(UrlScope::Recording, [77u8; 32]));
    let frame_issuer = Arc::new(SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [88u8; 32]));
    let storage = Arc::new(FrameStorage::new(64));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let db_loop = db.clone();
    let rec_loop = rec_issuer.clone();
    let frame_loop = frame_issuer.clone();
    let storage_loop = storage.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let db = db_loop.clone();
            let rec = rec_loop.clone();
            let frame = frame_loop.clone();
            let storage = storage_loop.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let db = db.clone();
                    let rec = rec.clone();
                    let frame = frame.clone();
                    let storage = storage.clone();
                    async move {
                        Ok::<_, Infallible>(router(req, &db, &rec, &frame, &storage).await)
                    }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(sock), svc)
                    .await;
            });
        }
    });

    Env {
        addr,
        db,
        rec_issuer,
        frame_issuer,
        storage,
    }
}

async fn router(
    req: Request<hyper::body::Incoming>,
    db: &DbPool,
    rec_issuer: &SignedUrlIssuer,
    frame_issuer: &SignedUrlIssuer,
    storage: &FrameStorage,
) -> Response<Full<Bytes>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query_string = uri.query().unwrap_or("").to_string();
    let headers = req.headers().clone();
    drop(req);

    // Mirror production: any non-empty body on these GETs is a 413.
    if (path.starts_with("/recordings/") || path.starts_with("/frames/"))
        && method == Method::GET
    {
        if headers.contains_key(hyper::header::TRANSFER_ENCODING) {
            return Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from_static(b"{\"error\":\"body_not_allowed\"}")))
                .unwrap();
        }
        let cl = headers
            .get(hyper::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if cl > 0 {
            return Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from_static(b"{\"error\":\"body_not_allowed\"}")))
                .unwrap();
        }
    }

    if method == Method::GET && path.starts_with("/recordings/") {
        let path_ref = path.strip_prefix("/recordings/").unwrap_or("");
        let q = match parse_rec_query(&query_string) {
            Ok(q) => q,
            Err(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
            }
        };
        let outcome = handle_recording_url(path_ref, &q, rec_issuer, db);
        let auth_status = outcome.http_status();
        return match outcome {
            RecordingOutcome::Ok {
                content_type,
                hash_sha256,
                created_at,
                file_size_bytes,
                file_path,
                retention_class,
                owner_addon_id,
            } => {
                let file_outcome = read_recording_file(
                    db,
                    path_ref,
                    &file_path,
                    &retention_class,
                    &owner_addon_id,
                    file_size_bytes,
                )
                .await;
                let status = file_outcome.http_status();
                match file_outcome {
                    RecordingFileOutcome::Ok { bytes } => Response::builder()
                        .status(status)
                        .header("Content-Type", content_type)
                        .header("X-Recording-Hash", hash_sha256)
                        .header("X-Recording-Created-At", created_at.to_string())
                        .body(Full::new(Bytes::from(bytes)))
                        .unwrap(),
                    _ => Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from_static(b"{\"error\":\"unavailable\"}")))
                        .unwrap(),
                }
            }
            _ => Response::builder()
                .status(auth_status)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from_static(b"{\"error\":\"denied\"}")))
                .unwrap(),
        };
    }

    if method == Method::GET && path.starts_with("/frames/") {
        let path_ref = path.strip_prefix("/frames/").unwrap_or("");
        let q = match parse_frame_query(&query_string) {
            Ok(q) => q,
            Err(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap();
            }
        };
        let outcome = handle_frame_url(path_ref, &q, frame_issuer, storage, db);
        let status = outcome.http_status();
        return match outcome {
            FrameOutcome::Ok {
                bytes,
                width,
                height,
                pixel_format,
                timestamp_unix_ms,
                pts: _,
            } => Response::builder()
                .status(status)
                .header("Content-Type", "application/octet-stream")
                .header(HDR_FRAME_WIDTH, width.to_string())
                .header(HDR_FRAME_HEIGHT, height.to_string())
                .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string())
                .body(Full::new(Bytes::copy_from_slice(&bytes)))
                .unwrap(),
            _ => Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from_static(b"{\"error\":\"denied\"}")))
                .unwrap(),
        };
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn rgb_buf(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push((x % 256) as u8);
            v.push((y % 256) as u8);
            v.push(((x + y) % 256) as u8);
        }
    }
    v
}

/// Save a snapshot via the real `save_snapshot_rgb24` and insert into DB.
/// HOME is mutated to a TempDir so the recording lands somewhere
/// per-test-process; the returned TempDir must outlive the test body.
async fn save_and_register(env: &Env, addon_id: &str, camera_id: &str) -> (String, tempfile::TempDir, Vec<u8>) {
    let tmp_home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("HOME", tmp_home.path());

    let rgb = rgb_buf(64, 48);
    let saved = save_snapshot_rgb24(camera_id, &rgb, 64, 48).await.expect("save");
    let path_str = saved.file_path.to_string_lossy().to_string();
    let png_bytes = tokio::fs::read(&saved.file_path).await.expect("read png");

    insert_recording(
        &env.db,
        saved.recording_ref.as_str(),
        "snapshot",
        addon_id,
        camera_id,
        &path_str,
        saved.file_size_bytes as i64,
        None,
        saved.width.map(|v| v as i64),
        saved.height.map(|v| v as i64),
        saved.pixel_format.as_deref(),
        &saved.hash_sha256,
        "B",
    )
    .expect("insert");

    (saved.recording_ref.as_str().to_string(), tmp_home, png_bytes)
}

fn frame_storage_insert(storage: &FrameStorage, camera_id: &str, payload: Vec<u8>) -> String {
    let metadata = FrameMetadata {
        camera_id: camera_id.into(),
        width: 8,
        height: 4,
        pixel_format: FramePixelFormat::Rgb24,
        timestamp_unix_ms: 1_715_500_000_000,
        pts: Some(123),
        frame_size_bytes: payload.len(),
    };
    let stored = StoredFrame {
        metadata,
        data: Arc::from(payload.into_boxed_slice()),
        created_at: std::time::Instant::now(),
    };
    storage.insert(stored).as_str().to_string()
}

// -----------------------------------------------------------------------------
// /recordings/<ref> tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_recording_url_returns_png() {
    let env = spawn_server().await;
    let (rec_ref, _home, png_bytes) = save_and_register(&env, "addon-a", "cam_e2e_ok").await;

    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?{}",
        env.addr, rec_ref, signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("Content-Type").and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    let body = resp.bytes().await.expect("body");
    assert_eq!(body.as_ref(), png_bytes.as_slice());
    assert_eq!(audit_log_count(&env.db, "recording_url_access", "ok"), 1);
}

#[tokio::test]
async fn test_e2e_recording_url_token_tampered_returns_403() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-tamper", "cam_e2e_tamper").await;

    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    // Flip the last byte of the token b64. base64 padding chars are common at
    // tail — guarantee an actual character flip rather than no-op replace.
    let mut tampered = signed.token_b64.clone();
    let last = tampered.pop().unwrap_or('A');
    tampered.push(if last == 'A' { 'B' } else { 'A' });

    let url = format!(
        "http://{}/recordings/{}?token={}&exp={}&ref={}",
        env.addr,
        rec_ref,
        urlencoding::encode(&tampered),
        signed.expiry_unix_ms,
        rec_ref
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(audit_log_count(&env.db, "recording_url_access", "denied"), 1);
}

#[tokio::test]
async fn test_e2e_recording_url_multi_fetch_in_ttl() {
    let env = spawn_server().await;
    let (rec_ref, _home, png_bytes) = save_and_register(&env, "addon-multi", "cam_e2e_multi").await;

    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?{}",
        env.addr, rec_ref, signed.query_string()
    );
    for _ in 0..3 {
        let resp = reqwest::get(&url).await.expect("get");
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.bytes().await.expect("body");
        assert_eq!(body.as_ref(), png_bytes.as_slice());
    }
    assert_eq!(audit_log_count(&env.db, "recording_url_access", "ok"), 3);
}

#[tokio::test]
async fn test_e2e_recording_url_purged_returns_404() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-purge", "cam_e2e_purge").await;

    // Soft-delete directly via SQL — bypassing the host-fn purge path so the
    // test stays focused on the HTTP layer's NotFound branch.
    {
        let conn = env.db.lock().expect("db lock");
        conn.execute(
            "UPDATE recordings SET purged_at = strftime('%s','now') WHERE ref = ?1",
            rusqlite::params![rec_ref],
        )
        .expect("soft delete");
    }
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?{}",
        env.addr, rec_ref, signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(audit_log_count(&env.db, "recording_url_access", "not_found"), 1);
}

#[tokio::test]
async fn test_e2e_recording_url_missing_query_params_returns_400() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-q", "cam_e2e_q").await;

    // No query at all.
    let url = format!("http://{}/recordings/{}", env.addr, rec_ref);
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 400);

    // Only token, no exp/ref.
    let url2 = format!("http://{}/recordings/{}?token=x", env.addr, rec_ref);
    let resp2 = reqwest::get(&url2).await.expect("get");
    assert_eq!(resp2.status().as_u16(), 400);

    assert_eq!(audit_log_count(&env.db, "recording_url_access", "bad_request"), 2);
}

#[tokio::test]
async fn test_e2e_recording_url_ref_mismatch_returns_400() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-mismatch", "cam_e2e_mm").await;

    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    // Path ref differs from query ref — must reject before signature verify.
    let url = format!(
        "http://{}/recordings/snap_00000000-0000-0000-0000-000000000000?token={}&exp={}&ref={}",
        env.addr,
        urlencoding::encode(&signed.token_b64),
        signed.expiry_unix_ms,
        rec_ref
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 400);
}

// -----------------------------------------------------------------------------
// /frames/<ref> tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_frame_url_returns_rgb24_bytes() {
    let env = spawn_server().await;
    let payload = vec![0x42u8; 8 * 4 * 3];
    let frame_ref = frame_storage_insert(&env.storage, "cam_e2e_frame", payload.clone());

    let signed = env.frame_issuer.issue(frame_ref.clone(), 120).expect("issue");
    let url = format!(
        "http://{}/frames/{}?{}",
        env.addr, frame_ref, signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get(HDR_FRAME_WIDTH).and_then(|v| v.to_str().ok()),
        Some("8")
    );
    assert_eq!(
        resp.headers().get(HDR_FRAME_HEIGHT).and_then(|v| v.to_str().ok()),
        Some("4")
    );
    assert_eq!(
        resp.headers().get(HDR_FRAME_PIXEL_FORMAT).and_then(|v| v.to_str().ok()),
        Some("rgb24")
    );
    let body = resp.bytes().await.expect("body");
    assert_eq!(body.as_ref(), payload.as_slice());
    assert_eq!(audit_log_count(&env.db, "frame_url_access", "ok"), 1);
}

#[tokio::test]
async fn test_e2e_frame_url_evicted_returns_404() {
    let env = spawn_server().await;
    // Mint a URL for a frame_ref that never enters the LRU.
    let phantom = "frame_00000000-0000-0000-0000-000000000000".to_string();
    let signed = env.frame_issuer.issue(phantom.clone(), 120).expect("issue");
    let url = format!(
        "http://{}/frames/{}?{}",
        env.addr, phantom, signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(audit_log_count(&env.db, "frame_url_access", "not_found"), 1);
}

#[tokio::test]
async fn test_e2e_frame_url_multi_fetch_in_ttl_ok() {
    let env = spawn_server().await;
    let payload = vec![0x77u8; 8 * 4 * 3];
    let frame_ref = frame_storage_insert(&env.storage, "cam_multi", payload.clone());
    let signed = env.frame_issuer.issue(frame_ref.clone(), 120).expect("issue");
    let url = format!(
        "http://{}/frames/{}?{}",
        env.addr, frame_ref, signed.query_string()
    );
    for _ in 0..3 {
        let resp = reqwest::get(&url).await.expect("get");
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.bytes().await.expect("body");
        assert_eq!(body.as_ref(), payload.as_slice());
    }
    assert_eq!(audit_log_count(&env.db, "frame_url_access", "ok"), 3);
}

// -----------------------------------------------------------------------------
// New regression tests — body DoS / size cap / strict query parsing
// -----------------------------------------------------------------------------

#[tokio::test]
async fn test_e2e_recording_url_duplicate_token_returns_400() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-dup", "cam_e2e_dup").await;
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?token={}&token=XX&exp={}&ref={}",
        env.addr,
        rec_ref,
        urlencoding::encode(&signed.token_b64),
        signed.expiry_unix_ms,
        rec_ref,
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 400);
}

#[tokio::test]
async fn test_e2e_recording_url_unknown_key_returns_400() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-uk", "cam_e2e_uk").await;
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?token={}&exp={}&ref={}&extra=foo",
        env.addr,
        rec_ref,
        urlencoding::encode(&signed.token_b64),
        signed.expiry_unix_ms,
        rec_ref,
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 400);
}

#[tokio::test]
async fn test_e2e_recording_url_chunked_body_rejected_413() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-chunk", "cam_e2e_chunk").await;
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    // Hand-roll the request because reqwest collapses GET-with-body in odd
    // ways. We use a raw TCP write of an HTTP/1.1 request with
    // `Transfer-Encoding: chunked` so the server's pre-collect gate fires.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect(env.addr).await.expect("connect");
    let request = format!(
        "GET /recordings/{rec_ref}?{query} HTTP/1.1\r\nHost: {host}\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        rec_ref = rec_ref,
        query = signed.query_string(),
        host = env.addr,
    );
    sock.write_all(request.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), sock.read_to_end(&mut buf)).await;
    let head = String::from_utf8_lossy(&buf);
    assert!(
        head.starts_with("HTTP/1.1 413"),
        "expected 413, got: {}",
        head.lines().next().unwrap_or("")
    );
}

#[tokio::test]
async fn test_e2e_recording_url_content_length_nonzero_rejected_413() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-cl", "cam_e2e_cl").await;
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect(env.addr).await.expect("connect");
    let body = "x";
    let request = format!(
        "GET /recordings/{rec_ref}?{query} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {clen}\r\n\r\n{body}",
        rec_ref = rec_ref,
        query = signed.query_string(),
        host = env.addr,
        clen = body.len(),
        body = body,
    );
    sock.write_all(request.as_bytes()).await.expect("write");
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), sock.read_to_end(&mut buf)).await;
    let head = String::from_utf8_lossy(&buf);
    assert!(
        head.starts_with("HTTP/1.1 413"),
        "expected 413, got: {}",
        head.lines().next().unwrap_or("")
    );
}

#[tokio::test]
async fn test_e2e_recording_url_missing_file_returns_404() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-miss", "cam_e2e_miss").await;
    // Delete the file on disk; DB row is still there.
    {
        let conn = env.db.lock().expect("db lock");
        let file_path: String = conn
            .query_row(
                "SELECT file_path FROM recordings WHERE ref = ?1",
                rusqlite::params![rec_ref],
                |row| row.get(0),
            )
            .expect("file_path");
        drop(conn);
        let _ = std::fs::remove_file(&file_path);
    }
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?{}",
        env.addr,
        rec_ref,
        signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn test_e2e_recording_url_file_size_mismatch_returns_500() {
    let env = spawn_server().await;
    let (rec_ref, _home, _) = save_and_register(&env, "addon-int", "cam_e2e_int").await;
    // Bump the DB-recorded size so it disagrees with on-disk reality.
    {
        let conn = env.db.lock().expect("db lock");
        conn.execute(
            "UPDATE recordings SET file_size_bytes = file_size_bytes + 999 WHERE ref = ?1",
            rusqlite::params![rec_ref],
        )
        .expect("bump size");
    }
    let signed = env.rec_issuer.issue(rec_ref.clone(), 300).expect("issue");
    let url = format!(
        "http://{}/recordings/{}?{}",
        env.addr,
        rec_ref,
        signed.query_string()
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 500);
}

#[tokio::test]
async fn test_e2e_frame_url_duplicate_key_returns_400() {
    let env = spawn_server().await;
    let payload = vec![0x11u8; 8 * 4 * 3];
    let frame_ref = frame_storage_insert(&env.storage, "cam_dup_f", payload);
    let signed = env.frame_issuer.issue(frame_ref.clone(), 120).expect("issue");
    let url = format!(
        "http://{}/frames/{}?token={}&token=XX&exp={}&ref={}",
        env.addr,
        frame_ref,
        urlencoding::encode(&signed.token_b64),
        signed.expiry_unix_ms,
        frame_ref,
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 400);
}

#[tokio::test]
async fn test_e2e_frame_url_token_tampered_returns_403() {
    let env = spawn_server().await;
    let payload = vec![0x33u8; 8 * 4 * 3];
    let frame_ref = frame_storage_insert(&env.storage, "cam_tamper_frame", payload);
    let signed = env.frame_issuer.issue(frame_ref.clone(), 120).expect("issue");
    let mut tampered = signed.token_b64.clone();
    let last = tampered.pop().unwrap_or('A');
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let url = format!(
        "http://{}/frames/{}?token={}&exp={}&ref={}",
        env.addr,
        frame_ref,
        urlencoding::encode(&tampered),
        signed.expiry_unix_ms,
        frame_ref
    );
    let resp = reqwest::get(&url).await.expect("get");
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(audit_log_count(&env.db, "frame_url_access", "denied"), 1);
}
