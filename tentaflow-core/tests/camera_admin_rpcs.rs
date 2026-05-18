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
    camera_admin_dispatch, reset_discover_rate_limiter_for_test, set_discover_timeout_ms_for_test,
};
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{
    CameraAddOnvifRequest, CameraAdminPayload, CameraDiscoverRequest, MessageBody, ProtocolError,
    ProtocolErrorCode, SessionAuth,
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
    // node's real `cameras.key`. set_var is racy but every camera_admin test
    // uses the same path so there is no contention.
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
        std::env::set_var("TENTAFLOW_CAMERAS_KEY", path);
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
    let conn = state.db.lock().expect("db mutex");
    conn.query_row(
        "SELECT COUNT(*) FROM cameras WHERE org_id = ?1",
        rusqlite::params![org_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

fn last_audit_for_action(state: &AppState, action: &str) -> Option<(Option<String>, String)> {
    let conn = state.db.lock().expect("db mutex");
    conn.query_row(
        "SELECT resource, COALESCE(details, '') FROM audit_log \
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
    let bad = MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(
        CameraAddOnvifRequest {
            display_name: "".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(15),
        },
    ));
    let err = expect_error(camera_admin_dispatch(&bad, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "display_name_empty");

    // FPS out of range.
    let bad_fps = MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(
        CameraAddOnvifRequest {
            display_name: "X".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(120),
        },
    ));
    let err = expect_error(camera_admin_dispatch(&bad_fps, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "target_fps_out_of_range");

    // Username with unsafe chars (would let an attacker smuggle a `@` into
    // the rtsp:// userinfo).
    let bad_user = MessageBody::CameraAdminBody(CameraAdminPayload::AddOnvifRequest(
        CameraAddOnvifRequest {
            display_name: "X".into(),
            device_service_url: "http://192.0.2.10/onvif/device_service".into(),
            username: "admin@evil".into(),
            password: "hunter2".into(),
            profile_token: None,
            target_fps: Some(15),
        },
    ));
    let err = expect_error(camera_admin_dispatch(&bad_user, &ctx).await);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert_eq!(err.message, "username_invalid_chars");

    // No camera rows must have been inserted by any of the rejected paths.
    assert_eq!(count_cameras_for_org(&state, "org-test"), 0);
}
