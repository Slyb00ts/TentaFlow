// =============================================================================
// File: tests/camera_admin_rpcs.rs
// Purpose: Coverage for the F2 P7.a admin binary RPCs `CameraDiscoverRequest`
//          and `CameraAddOnvifRequest`. Discovery hits a stubbed UDP listener
//          path (the actual ws-discovery probe is allowed to time out — we
//          only assert permission + rate-limit semantics). The add path drives
//          a wiremock-served ONVIF device that replies with canned
//          GetProfiles + GetStreamUri envelopes.
// =============================================================================

#![cfg(feature = "camera")]

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::dispatch::camera_admin::{
    camera_admin_dispatch, reset_discover_rate_limiter_for_test,
    reset_frame_url_rate_limiter_for_test, set_discover_timeout_ms_for_test,
};
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{
    CameraAddOnvifRequest, CameraAdminPayload, CameraDiscoverRequest, CameraFrameUrlRequest,
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// =============================================================================
// Canned ONVIF SOAP envelopes
// =============================================================================

const GET_PROFILES_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
  <env:Body>
    <trt:GetProfilesResponse>
      <trt:Profiles token="MainProfile" fixed="true">
        <tt:Name>Main</tt:Name>
        <tt:VideoEncoderConfiguration token="VEC1">
          <tt:Encoding>H264</tt:Encoding>
          <tt:Resolution>
            <tt:Width>1920</tt:Width>
            <tt:Height>1080</tt:Height>
          </tt:Resolution>
        </tt:VideoEncoderConfiguration>
      </trt:Profiles>
    </trt:GetProfilesResponse>
  </env:Body>
</env:Envelope>"#;

const GET_STREAM_URI_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
  <env:Body>
    <trt:GetStreamUriResponse>
      <trt:MediaUri>
        <tt:Uri>rtsp://192.168.10.42:554/onvif/profile1/media.smp</tt:Uri>
      </trt:MediaUri>
    </trt:GetStreamUriResponse>
  </env:Body>
</env:Envelope>"#;

const FAULT_NOT_AUTHORIZED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:ter="http://www.onvif.org/ver10/error">
  <env:Body>
    <env:Fault>
      <env:Code>
        <env:Value>env:Sender</env:Value>
        <env:Subcode>
          <env:Value>ter:NotAuthorized</env:Value>
        </env:Subcode>
      </env:Code>
      <env:Reason>
        <env:Text>Sender not authorized</env:Text>
      </env:Reason>
    </env:Fault>
  </env:Body>
</env:Envelope>"#;

const GET_PROFILES_EMPTY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:trt="http://www.onvif.org/ver10/media/wsdl">
  <env:Body>
    <trt:GetProfilesResponse/>
  </env:Body>
</env:Envelope>"#;

// =============================================================================
// Fixtures
// =============================================================================

fn ensure_cameras_key_env() {
    // CredentialsCipher generates a 32-byte master key on first use. In tests
    // we redirect it to a per-process tempfile so tests don't trample the dev
    // node's real `cameras.key`. The override is a process-wide OnceLock and
    // every camera_admin test uses the same path, so there is no contention.
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let tmp = tempfile::Builder::new()
            .prefix("camera-admin-test-")
            .suffix(".key")
            .tempfile()
            .expect("tempfile for cameras.key");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Removing the empty file lets `load_or_generate_at` write a fresh
        // 32-byte CSPRNG key with the correct mode bits.
        let _ = std::fs::remove_file(&path);
        tentaflow_core::services::camera_ingest::credentials::set_key_path_override(path);
    });
}

fn ctx_with_perms(state: Arc<AppState>, perms: &[&str]) -> HandlerContext {
    let mut user_id_bytes = [0u8; 16];
    user_id_bytes[0] = 0xFF;
    let user_le = 42i64.to_le_bytes();
    user_id_bytes[8..].copy_from_slice(&user_le);
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: user_id_bytes,
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        org_context: Some(OrgContext {
            user_id: "user-42".to_string(),
            org_id: "org-test".to_string(),
            role_id: "role-test".to_string(),
            permissions: perms.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        }),
    }
}

fn count_cameras_for_org(state: &AppState, org_id: &str) -> i64 {
    let conn = state.db.read().expect("db mutex");
    conn.query_row(
        "SELECT COUNT(*) FROM cameras WHERE org_id = ?1",
        rusqlite::params![org_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

fn last_audit_for_action(state: &AppState, action: &str) -> Option<(Option<String>, String)> {
    let conn = state.db.read().expect("db mutex");
    conn.query_row(
        "SELECT COALESCE(resource, resource_id), \
                COALESCE(result, '') || ' ' || COALESCE(details, '') \
         FROM audit_log \
         WHERE action = ?1 ORDER BY id DESC LIMIT 1",
        rusqlite::params![action],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
    )
    .ok()
}

fn expect_error(res: Result<MessageBody, ProtocolError>) -> ProtocolError {
    match res {
        Ok(MessageBody::Error(e)) => e,
        Ok(_) => panic!("expected ProtocolError, got Ok"),
        Err(e) => e,
    }
}

// =============================================================================
// camera.discover
// =============================================================================

#[tokio::test]
async fn camera_discover_denied_without_permission() {
    ensure_cameras_key_env();
    reset_discover_rate_limiter_for_test();
    let state = AppState::for_test();
    // org_context has no "camera.discover" entry.
    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    let req = MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverRequest(
        CameraDiscoverRequest {},
    ));
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);

    let audit = last_audit_for_action(&state, "camera.discover").expect("audit row");
    assert!(audit.1.contains("denied"));
    assert!(audit.1.contains("missing_permission"));
}

#[tokio::test]
async fn camera_discover_rate_limited_after_burst() {
    ensure_cameras_key_env();
    reset_discover_rate_limiter_for_test();
    // Shrink the per-call ws-discovery window so the loop closes fast; the
    // limiter's refill (0.1 tok/s) cannot replenish a token within the
    // ~tens-of-ms total runtime, so the 7th call is reliably rate-limited.
    set_discover_timeout_ms_for_test(20);
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.discover"]);

    let req = MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverRequest(
        CameraDiscoverRequest {},
    ));

    // Burst capacity = 6. The first six calls consume the bucket (each call's
    // ws-discovery is short-circuited to an empty list because there is no LAN
    // device responding in the test window, but the limiter is debited before
    // the probe runs). The 7th must be rate-limited.
    for i in 0..6 {
        let out = camera_admin_dispatch(&req, &ctx).await.expect("ok");
        match out {
            MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverResponse(_)) => {}
            other => panic!("iter {i}: expected DiscoverResponse, got {other:?}"),
        }
    }
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::RateLimited);

    let audit = last_audit_for_action(&state, "camera.discover").expect("audit row");
    assert!(audit.1.contains("rate_limited"), "details={}", audit.1);
}

#[tokio::test]
async fn camera_discover_isolates_org_buckets() {
    ensure_cameras_key_env();
    reset_discover_rate_limiter_for_test();
    set_discover_timeout_ms_for_test(20);
    let state = AppState::for_test();

    // Org A burns its full burst.
    let mut ctx_a = ctx_with_perms(state.clone(), &["camera.discover"]);
    if let Some(oc) = ctx_a.org_context.as_mut() {
        oc.org_id = "org-a".to_string();
    }
    let req = MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverRequest(
        CameraDiscoverRequest {},
    ));
    for _ in 0..6 {
        assert!(camera_admin_dispatch(&req, &ctx_a).await.is_ok());
    }
    let err = expect_error(camera_admin_dispatch(&req, &ctx_a).await);
    assert_eq!(err.code, ProtocolErrorCode::RateLimited);

    // Org B starts with a fresh bucket — the limiter is keyed per org.
    let mut ctx_b = ctx_with_perms(state.clone(), &["camera.discover"]);
    if let Some(oc) = ctx_b.org_context.as_mut() {
        oc.org_id = "org-b".to_string();
    }
    let out = camera_admin_dispatch(&req, &ctx_b).await.expect("ok");
    assert!(matches!(
        out,
        MessageBody::CameraAdminBody(CameraAdminPayload::DiscoverResponse(_))
    ));
}

// =============================================================================
// camera.add_onvif
// =============================================================================

async fn mount_get_profiles(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(wiremock::matchers::body_string_contains("GetProfiles"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn mount_get_stream_uri(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(wiremock::matchers::body_string_contains("GetStreamUri"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn mount_get_profiles_fault(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(wiremock::matchers::body_string_contains("GetProfiles"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

fn add_onvif_request(device_url: &str) -> MessageBody {
    MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(CameraAddOnvifRequest {
        display_name: "Front Door".into(),
        device_service_url: device_url.into(),
        username: "admin".into(),
        password: "hunter2".into(),
        profile_token: None,
        target_fps: Some(15),
    }))
}

#[tokio::test]
async fn camera_add_onvif_denied_without_permission() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);
    let server = MockServer::start().await;
    let url = format!("{}/onvif/device_service", server.uri());

    let err = expect_error(camera_admin_dispatch(&add_onvif_request(&url), &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    assert_eq!(count_cameras_for_org(&state, "org-test"), 0);

    let audit = last_audit_for_action(&state, "camera.add_onvif").expect("audit row");
    assert!(audit.1.contains("denied"));
    assert!(audit.1.contains("missing_permission"));
}

#[tokio::test]
async fn camera_add_onvif_org_scoped() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let mut ctx = ctx_with_perms(state.clone(), &["camera.write"]);
    if let Some(oc) = ctx.org_context.as_mut() {
        oc.org_id = "org-tenant-A".to_string();
    }
    let server = MockServer::start().await;
    mount_get_profiles(&server, GET_PROFILES_OK).await;
    mount_get_stream_uri(&server, GET_STREAM_URI_OK).await;
    let url = format!("{}/onvif/device_service", server.uri());

    let out = camera_admin_dispatch(&add_onvif_request(&url), &ctx)
        .await
        .expect("ok");
    let resp = match out {
        MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifResponse(r)) => r,
        other => panic!("expected AddOnvifResponse, got {other:?}"),
    };
    assert!(resp.camera_id.starts_with("cam_"));
    assert_eq!(resp.profile_token, "MainProfile");
    assert!(resp.rtsp_url.starts_with("rtsp://"));

    // Row exists under the tenant org and not the default one.
    assert_eq!(count_cameras_for_org(&state, "org-tenant-A"), 1);
    assert_eq!(count_cameras_for_org(&state, "org-default"), 0);

    let audit = last_audit_for_action(&state, "camera.add_onvif").expect("audit row");
    assert_eq!(audit.0.as_deref(), Some(resp.camera_id.as_str()));
    assert!(audit.1.contains("ok"));
    // Sensitive fields must never appear in audit details.
    assert!(!audit.1.contains("hunter2"));
    assert!(!audit.1.contains("admin:hunter2"));
}

#[tokio::test]
async fn camera_add_onvif_auth_failed_maps_to_protocol_error() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.write"]);
    let server = MockServer::start().await;
    mount_get_profiles_fault(&server, FAULT_NOT_AUTHORIZED).await;
    let url = format!("{}/onvif/device_service", server.uri());

    let err = expect_error(camera_admin_dispatch(&add_onvif_request(&url), &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    assert_eq!(err.message, "onvif_auth_failed");
    assert_eq!(count_cameras_for_org(&state, "org-test"), 0);

    let audit = last_audit_for_action(&state, "camera.add_onvif").expect("audit row");
    assert!(audit.1.contains("onvif_auth_failed"));
    assert!(!audit.1.contains("hunter2"));
}

#[tokio::test]
async fn camera_add_onvif_no_profiles_maps_to_protocol_error() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.write"]);
    let server = MockServer::start().await;
    mount_get_profiles(&server, GET_PROFILES_EMPTY).await;
    let url = format!("{}/onvif/device_service", server.uri());

    let err = expect_error(camera_admin_dispatch(&add_onvif_request(&url), &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
    assert_eq!(err.message, "onvif_no_profiles");
    assert_eq!(count_cameras_for_org(&state, "org-test"), 0);

    let audit = last_audit_for_action(&state, "camera.add_onvif").expect("audit row");
    assert!(audit.1.contains("onvif_no_profiles"));
}

#[tokio::test]
async fn camera_add_onvif_emits_audit_row_with_org_and_ok_result() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.write"]);
    let server = MockServer::start().await;
    mount_get_profiles(&server, GET_PROFILES_OK).await;
    mount_get_stream_uri(&server, GET_STREAM_URI_OK).await;
    let url = format!("{}/onvif/device_service", server.uri());

    let out = camera_admin_dispatch(&add_onvif_request(&url), &ctx)
        .await
        .expect("ok");
    let resp = match out {
        MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifResponse(r)) => r,
        other => panic!("unexpected: {other:?}"),
    };

    let audit = last_audit_for_action(&state, "camera.add_onvif").expect("audit row");
    assert_eq!(audit.0.as_deref(), Some(resp.camera_id.as_str()));
    assert!(audit.1.contains("user_id=user-42"));
    assert!(audit.1.contains("vendor=onvif"));
}

#[tokio::test]
async fn camera_add_onvif_rejects_invalid_inputs_without_calling_device() {
    ensure_cameras_key_env();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.write"]);

    // Empty display_name.
    let bad =
        MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(CameraAddOnvifRequest {
            display_name: "".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(15),
        }));
    let err = expect_error(camera_admin_dispatch(&bad, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "display_name_empty");

    // FPS out of range.
    let bad_fps =
        MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(CameraAddOnvifRequest {
            display_name: "X".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(120),
        }));
    let err = expect_error(camera_admin_dispatch(&bad_fps, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "target_fps_out_of_range");

    // Username with unsafe chars (would let an attacker smuggle a `@` into
    // the rtsp:// userinfo).
    let bad_user =
        MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(CameraAddOnvifRequest {
            display_name: "X".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin@evil".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(15),
        }));
    let err = expect_error(camera_admin_dispatch(&bad_user, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "username_invalid_chars");

    // No camera rows must have been inserted by any of the rejected paths.
    assert_eq!(count_cameras_for_org(&state, "org-test"), 0);
}

// =============================================================================
// camera.frame_url
// =============================================================================

const VALID_UUID_A: &str = "cam_550e8400-e29b-41d4-a716-446655440000";
const VALID_UUID_B: &str = "cam_550e8400-e29b-41d4-a716-446655440001";

fn insert_camera_row(state: &AppState, camera_id: &str, org_id: &str) {
    tentaflow_core::db::repository::insert_camera(
        &state.db,
        camera_id,
        "addon-test",
        "Test Camera",
        "onvif",
        "rtsp://example/stream",
        15,
        10,
        None,
        None,
        "C",
        "default",
        None,
        None,
        None,
        Some(org_id),
    )
    .expect("insert camera row");
}

fn push_frame(camera_id: &str) {
    use tentaflow_core::services::frame_storage::{FrameMetadata, FramePixelFormat, StoredFrame};
    let frame = StoredFrame {
        metadata: FrameMetadata {
            camera_id: camera_id.to_string(),
            width: 16,
            height: 16,
            pixel_format: FramePixelFormat::Rgb24,
            timestamp_unix_ms: 0,
            pts: None,
            frame_size_bytes: 768,
        },
        data: vec![0u8; 768].into(),
        created_at: std::time::Instant::now(),
    };
    tentaflow_core::services::frame_storage().insert(frame);
}

#[tokio::test]
async fn frame_url_rejects_invalid_uuid() {
    ensure_cameras_key_env();
    reset_frame_url_rate_limiter_for_test();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    let req =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: "not-a-uuid".to_string(),
            ttl_secs: 30,
        }));
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "camera_id_invalid_format");

    let audit = last_audit_for_action(&state, "camera.frame_url").expect("audit row");
    assert!(audit.1.contains("denied"));
    assert!(audit.1.contains("camera_id_invalid_format"));
    // Static reason — must NEVER echo the raw input value.
    assert!(!audit.1.contains("not-a-uuid"));
}

#[tokio::test]
async fn frame_url_rejects_cross_org_camera() {
    ensure_cameras_key_env();
    reset_frame_url_rate_limiter_for_test();
    let state = AppState::for_test();
    // Camera lives in org-other; the caller's session is org-test.
    insert_camera_row(&state, VALID_UUID_A, "org-other");
    push_frame(VALID_UUID_A);

    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    let req =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: VALID_UUID_A.to_string(),
            ttl_secs: 30,
        }));
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::NotFound);
    assert_eq!(err.message, "camera_not_found");

    let audit = last_audit_for_action(&state, "camera.frame_url").expect("audit row");
    assert!(audit.1.contains("camera_not_found"), "details={}", audit.1);
    // No camera_id echo in audit details.
    assert!(!audit.1.contains(VALID_UUID_A));
}

#[tokio::test]
async fn frame_url_rejects_ttl_out_of_range() {
    ensure_cameras_key_env();
    reset_frame_url_rate_limiter_for_test();
    let state = AppState::for_test();
    insert_camera_row(&state, VALID_UUID_B, "org-test");
    push_frame(VALID_UUID_B);

    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    // Below the dispatch floor (5 secs).
    let too_low =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: VALID_UUID_B.to_string(),
            ttl_secs: 4,
        }));
    let err = expect_error(camera_admin_dispatch(&too_low, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "ttl_secs_out_of_range");

    // Above the dispatch ceiling (300 secs).
    let too_high =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: VALID_UUID_B.to_string(),
            ttl_secs: 301,
        }));
    let err = expect_error(camera_admin_dispatch(&too_high, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "ttl_secs_out_of_range");
}

#[tokio::test]
async fn frame_url_rate_limit_per_user() {
    ensure_cameras_key_env();
    // Use a test-unique org so the process-wide bucket cache cannot be
    // pre-drained by another test running in parallel inside the same proc.
    let unique_org = "org-frame-url-burst";
    let state = AppState::for_test();
    insert_camera_row(&state, VALID_UUID_A, unique_org);
    push_frame(VALID_UUID_A);

    let mut ctx = ctx_with_perms(state.clone(), &["camera.read"]);
    if let Some(oc) = ctx.org_context.as_mut() {
        oc.org_id = unique_org.to_string();
    }
    let req =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: VALID_UUID_A.to_string(),
            ttl_secs: 30,
        }));

    // Burst capacity = 30. The first 30 calls drain the bucket; refill at
    // 0.5 tok/s is too slow to replenish during a tight test loop.
    for i in 0..30 {
        let out = camera_admin_dispatch(&req, &ctx).await.expect("ok");
        match out {
            MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlResponse(_)) => {}
            other => panic!("iter {i}: expected FrameUrlResponse, got {other:?}"),
        }
    }
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::RateLimited);

    let audit = last_audit_for_action(&state, "camera.frame_url").expect("audit row");
    assert!(audit.1.contains("rate_limited"), "details={}", audit.1);
}

#[tokio::test]
async fn frame_url_rejects_unknown_prefix() {
    ensure_cameras_key_env();
    reset_frame_url_rate_limiter_for_test();
    let state = AppState::for_test();
    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    // Same length as `cam_<uuid v4>` (40 chars) but the wrong 4-byte prefix.
    let req =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: "foo_550e8400-e29b-41d4-a716-446655440000".to_string(),
            ttl_secs: 30,
        }));
    let err = expect_error(camera_admin_dispatch(&req, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "camera_id_invalid_format");
}

#[tokio::test]
async fn frame_url_accepts_cam_prefix_uuid() {
    ensure_cameras_key_env();
    reset_frame_url_rate_limiter_for_test();
    let state = AppState::for_test();
    insert_camera_row(&state, VALID_UUID_B, "org-test");
    push_frame(VALID_UUID_B);
    let ctx = ctx_with_perms(state.clone(), &["camera.read"]);

    let req =
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlRequest(CameraFrameUrlRequest {
            camera_id: VALID_UUID_B.to_string(),
            ttl_secs: 30,
        }));
    let out = camera_admin_dispatch(&req, &ctx).await.expect("ok");
    match out {
        MessageBody::CameraAdminBody(CameraAdminPayload::FrameUrlResponse(r)) => {
            assert!(r.signed_url.starts_with("/frames/"));
            assert!(r.signed_url.contains("token="));
        }
        other => panic!("expected FrameUrlResponse, got {other:?}"),
    }
}
