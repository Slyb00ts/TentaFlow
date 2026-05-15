// =============================================================================
// File: tests/streaming_pickup.rs — PickupToken + Service-to-Core integration
// =============================================================================
//
// Drives the M1.W7 Chunk C surface without standing up a wasmtime caller or a
// hyper server: the `PickupTokenIssuer` is exercised directly, and the
// `api::frame_pickup::handle_pickup` pure function is called against an
// in-memory FrameStorage + in-memory SQLite. The integration tests cover the
// six security promises from §6.4 of `tentavision-plan.md`:
//   1. happy path (issue → verify → bytes returned)
//   2. replay rejected (one-shot consume)
//   3. expired token rejected (TTL)
//   4. forged signature rejected (HMAC mismatch)
//   5. cross-service header replay rejected (defense-in-depth)
//   6. unknown-but-valid signature rejected (server restart / table miss)

use std::sync::Arc;
use std::time::Duration;

use tentaflow_core::api::frame_pickup::{handle_pickup, PickupOutcome, PickupRequest};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::frame_storage::{
    FrameMetadata, FramePixelFormat, FrameStorage, StoredFrame,
};
use tentaflow_core::services::pickup_tokens::{PickupTokenIssuer, PickupVerifyError};

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("db init")
}

fn issuer(ttl: Duration) -> PickupTokenIssuer {
    PickupTokenIssuer::new_for_tests([42u8; 32], ttl)
}

fn mk_frame(camera_id: &str, payload: &[u8]) -> StoredFrame {
    StoredFrame {
        metadata: FrameMetadata {
            camera_id: camera_id.into(),
            width: 4,
            height: 2,
            pixel_format: FramePixelFormat::Rgb24,
            timestamp_unix_ms: 1_715_500_000_000,
            pts: Some(1234),
            frame_size_bytes: payload.len(),
        },
        data: Arc::from(payload.to_vec().into_boxed_slice()),
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
// 1. Happy path
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_issue_and_verify_basic() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam-1", &[1, 2, 3, 4]));
    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(
        raw_ref.as_str().to_string(),
        "yolo-svc".into(),
        "req-1".into(),
    );
    let wire = tok.wire();
    let db = make_db();

    let pr = PickupRequest {
        pickup_token: Some(&wire),
        frame_ref: Some(raw_ref.as_str()),
        service_id: Some("yolo-svc"),
        request_id: Some("req-1"),
    };
    let outcome = handle_pickup(pr, &iss, &storage, &db);
    match outcome {
        PickupOutcome::Ok { bytes, width, height, pixel_format, .. } => {
            assert_eq!(&*bytes, &[1, 2, 3, 4]);
            assert_eq!(width, 4);
            assert_eq!(height, 2);
            assert_eq!(pixel_format, "rgb24");
        }
        other => panic!("expected Ok, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "ok"), 1);
    // Frame was consumed from LRU — second lookup must miss.
    assert_eq!(storage.len(), 0, "remove must drain the LRU entry");
}

// -----------------------------------------------------------------------------
// 2. Replay
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_replay_rejected() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam", &[9; 16]));
    // Two distinct entries so that the token verify path is the rejector
    // (not the LRU miss path).
    let _other = storage.insert(mk_frame("cam", &[0; 4]));
    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(raw_ref.as_str().to_string(), "svc".into(), "req".into());
    let wire = tok.wire();
    let db = make_db();

    let mk_req = || PickupRequest {
        pickup_token: Some(&wire),
        frame_ref: Some(raw_ref.as_str()),
        service_id: Some("svc"),
        request_id: Some("req"),
    };
    let first = handle_pickup(mk_req(), &iss, &storage, &db);
    assert!(matches!(first, PickupOutcome::Ok { .. }));
    let second = handle_pickup(mk_req(), &iss, &storage, &db);
    match second {
        PickupOutcome::Unauthorized(PickupVerifyError::AlreadyConsumed) => {}
        other => panic!("expected AlreadyConsumed, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "unauthorized"), 1);
}

// -----------------------------------------------------------------------------
// 3. Expired
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_expired_rejected() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam", &[7]));
    let iss = issuer(Duration::from_millis(1));
    let (tok, _) = iss.issue(raw_ref.as_str().to_string(), "svc".into(), "req".into());
    let wire = tok.wire();
    std::thread::sleep(Duration::from_millis(20));
    let db = make_db();

    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss,
        &storage,
        &db,
    );
    match outcome {
        PickupOutcome::Unauthorized(PickupVerifyError::Expired) => {}
        other => panic!("expected Expired, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "token_expired"), 1);
    // Frame remains in storage — expiry must not consume the LRU entry.
    assert_eq!(storage.len(), 1);
}

// -----------------------------------------------------------------------------
// 4. Forged signature
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_forge_rejected() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam", &[5]));
    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(raw_ref.as_str().to_string(), "svc".into(), "req".into());
    let mut wire = tok.wire();
    let last = wire.pop().unwrap();
    wire.push(if last == 'A' { 'B' } else { 'A' });
    let db = make_db();

    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss,
        &storage,
        &db,
    );
    match outcome {
        PickupOutcome::Unauthorized(PickupVerifyError::InvalidSignature) => {}
        other => panic!("expected InvalidSignature, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "token_invalid"), 1);
}

// -----------------------------------------------------------------------------
// 5. Cross-service header replay
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_cross_service_rejected() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam", &[1, 2]));
    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(
        raw_ref.as_str().to_string(),
        "yolo-svc".into(),
        "req-1".into(),
    );
    let wire = tok.wire();
    let db = make_db();

    // Real token but X-Service-Id lies → header mismatch path.
    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("ocr-svc"),
            request_id: Some("req-1"),
        },
        &iss,
        &storage,
        &db,
    );
    match outcome {
        PickupOutcome::HeaderMismatch(why) => assert_eq!(why, "service_id_mismatch"),
        other => panic!("expected HeaderMismatch, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "unauthorized"), 1);
    // Note: the token IS consumed even though headers mismatched — that is
    // intentional. A peer trying to abuse the token with bogus headers burns
    // it immediately, so the real recipient cannot use it either, which is
    // the safer failure mode (avoid double-spend at the cost of one denied
    // legitimate retry).
}

// -----------------------------------------------------------------------------
// 6. Unknown but signature-valid (server restart)
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_token_unknown_rejected() {
    let storage = FrameStorage::new(8);
    let raw_ref = storage.insert(mk_frame("cam", &[8, 8]));
    // Issuer A signs the token; issuer B has the SAME key (so the HMAC
    // checks out) but never inserted the entry into its inflight table.
    let key = [99u8; 32];
    let iss_a = PickupTokenIssuer::new_for_tests(key, Duration::from_secs(30));
    let iss_b = PickupTokenIssuer::new_for_tests(key, Duration::from_secs(30));
    let (tok, _) = iss_a.issue(
        raw_ref.as_str().to_string(),
        "svc".into(),
        "req".into(),
    );
    let wire = tok.wire();
    let db = make_db();

    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss_b,
        &storage,
        &db,
    );
    match outcome {
        PickupOutcome::Unauthorized(PickupVerifyError::InvalidToken) => {}
        other => panic!("expected InvalidToken, got {:?}", other),
    }
    assert_eq!(frame_pickup_log_count(&db, "token_invalid"), 1);
}

// -----------------------------------------------------------------------------
// Bad-headers paths
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_missing_headers_rejected() {
    let storage = FrameStorage::new(2);
    let iss = issuer(Duration::from_secs(30));
    let db = make_db();
    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: None,
            frame_ref: Some("frame_x"),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss,
        &storage,
        &db,
    );
    assert!(matches!(outcome, PickupOutcome::BadHeaders(_)));
    assert_eq!(outcome.http_status(), 400);
    assert_eq!(frame_pickup_log_count(&db, "token_invalid"), 1);
}

#[test]
fn test_pickup_frame_purged_after_lru_eviction() {
    let storage = FrameStorage::new(2);
    let r1 = storage.insert(mk_frame("c", &[1]));
    let _r2 = storage.insert(mk_frame("c", &[2]));
    let _r3 = storage.insert(mk_frame("c", &[3])); // evicts r1
    assert!(storage.get(&r1).is_none(), "r1 must be evicted");

    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(
        r1.as_str().to_string(),
        "svc".into(),
        "req".into(),
    );
    let wire = tok.wire();
    let db = make_db();
    let outcome = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(r1.as_str()),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss,
        &storage,
        &db,
    );
    assert!(matches!(outcome, PickupOutcome::FramePurged));
    assert_eq!(outcome.http_status(), 404);
    assert_eq!(frame_pickup_log_count(&db, "frame_purged"), 1);
}

// -----------------------------------------------------------------------------
// HTTP status mapping spot-check
// -----------------------------------------------------------------------------

#[test]
fn test_pickup_outcome_status_codes() {
    // Direct assertions on the enum to catch a refactor that forgets to
    // update one branch.
    let storage = FrameStorage::new(2);
    let raw_ref = storage.insert(mk_frame("c", &[1]));
    let iss = issuer(Duration::from_secs(30));
    let (tok, _) = iss.issue(raw_ref.as_str().to_string(), "svc".into(), "req".into());
    let wire = tok.wire();
    let db = make_db();
    let ok = handle_pickup(
        PickupRequest {
            pickup_token: Some(&wire),
            frame_ref: Some(raw_ref.as_str()),
            service_id: Some("svc"),
            request_id: Some("req"),
        },
        &iss,
        &storage,
        &db,
    );
    assert_eq!(ok.http_status(), 200);
}
