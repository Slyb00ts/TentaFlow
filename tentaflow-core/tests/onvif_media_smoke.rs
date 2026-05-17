// =============================================================================
// File: tests/onvif_media_smoke.rs
// Purpose: Verifies F1c P6 ONVIF Media SOAP client against canned responses
//          and a wiremock-served mock device. The unit-level cases in
//          `services::camera_ingest::onvif_media::tests` cover the digest
//          algorithm + XML escaping in isolation; this suite drives the
//          public `get_profiles` / `get_stream_uri` / `derive_rtsp_uri`
//          surface end-to-end over real HTTP so a regression in the
//          reqwest plumbing surfaces here.
// =============================================================================

#![cfg(feature = "camera")]

use tentaflow_core::services::camera_ingest::onvif_media::{
    derive_rtsp_uri, get_profiles, get_stream_uri, OnvifCredentials, OnvifError,
    StreamProtocol,
};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
            <tt:Width>2560</tt:Width>
            <tt:Height>1440</tt:Height>
          </tt:Resolution>
        </tt:VideoEncoderConfiguration>
      </trt:Profiles>
      <trt:Profiles token="SubProfile">
        <tt:Name>Sub</tt:Name>
        <tt:VideoEncoderConfiguration token="VEC2">
          <tt:Encoding>H265</tt:Encoding>
          <tt:Resolution>
            <tt:Width>640</tt:Width>
            <tt:Height>360</tt:Height>
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
        <tt:InvalidAfterConnect>false</tt:InvalidAfterConnect>
        <tt:InvalidAfterReboot>false</tt:InvalidAfterReboot>
        <tt:Timeout>PT60S</tt:Timeout>
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

fn creds() -> OnvifCredentials {
    OnvifCredentials {
        username: "admin".into(),
        password: "hunter2".into(),
    }
}

#[tokio::test]
async fn get_profiles_smoke_returns_two_profiles() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(header_exists("Content-Type"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_PROFILES_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let profiles = get_profiles(&url, &creds(), 5_000).await.expect("ok");
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].token, "MainProfile");
    assert_eq!(profiles[0].encoding.as_deref(), Some("H264"));
    assert_eq!(profiles[0].resolution, Some((2560, 1440)));
    assert_eq!(profiles[1].token, "SubProfile");
}

#[tokio::test]
async fn get_stream_uri_smoke_returns_rtsp_uri() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_STREAM_URI_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let stream = get_stream_uri(&url, &creds(), "MainProfile", StreamProtocol::Tcp, 5_000)
        .await
        .expect("ok");
    assert_eq!(stream.rtsp_uri, "rtsp://192.168.10.42:554/onvif/profile1/media.smp");
    assert_eq!(stream.profile_token, "MainProfile");
}

#[tokio::test]
async fn auth_fault_returns_auth_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(FAULT_NOT_AUTHORIZED),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let err = get_profiles(&url, &creds(), 5_000)
        .await
        .expect_err("must fail");
    assert!(matches!(err, OnvifError::AuthFailed), "got {err:?}");
}

#[tokio::test]
async fn http_401_returns_auth_failed_even_with_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let err = get_profiles(&url, &creds(), 5_000)
        .await
        .expect_err("must fail");
    assert!(matches!(err, OnvifError::AuthFailed), "got {err:?}");
}

#[tokio::test]
async fn derive_rtsp_uri_one_shot_picks_first_profile() {
    // Two SOAP calls: GetProfiles then GetStreamUri. wiremock matches on
    // body substring to disambiguate.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(wiremock::matchers::body_string_contains("GetProfiles"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_PROFILES_OK),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .and(wiremock::matchers::body_string_contains("GetStreamUri"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_STREAM_URI_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let stream = derive_rtsp_uri(&url, &creds(), None, 5_000).await.expect("ok");
    assert_eq!(stream.profile_token, "MainProfile");
    assert!(stream.rtsp_uri.starts_with("rtsp://"));
}

#[tokio::test]
async fn derive_rtsp_uri_with_unknown_profile_returns_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_PROFILES_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let err = derive_rtsp_uri(&url, &creds(), Some("NoSuchProfile"), 5_000)
        .await
        .expect_err("must fail");
    assert!(
        matches!(err, OnvifError::ProfileNotFound(ref t) if t == "NoSuchProfile"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn envelope_carries_password_digest_and_escapes_username() {
    // Capture the request body so we can assert the WS-Security headers
    // are present. wiremock's `then_capture` is not available, so we
    // mount a permissive mock and then read its received requests.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/device_service"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_PROFILES_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/device_service", server.uri());
    let nasty = OnvifCredentials {
        username: r#"<a>&"b'"#.into(),
        password: "p".into(),
    };
    let _ = get_profiles(&url, &nasty, 5_000).await.expect("ok");

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body = String::from_utf8_lossy(&received[0].body);
    // WS-Security envelope shape
    assert!(body.contains("UsernameToken"), "missing UsernameToken: {body}");
    assert!(body.contains("PasswordDigest"), "missing digest variant: {body}");
    assert!(body.contains("<Nonce"), "missing Nonce: {body}");
    assert!(body.contains("<Created"), "missing Created: {body}");
    // The injection must be entity-escaped — raw `<a>` must not appear
    // outside the entity form.
    assert!(
        body.contains("&lt;a&gt;&amp;&quot;b&apos;"),
        "username not escaped: {body}"
    );
    assert!(!body.contains("<Username><a>"), "raw injection present: {body}");
}
