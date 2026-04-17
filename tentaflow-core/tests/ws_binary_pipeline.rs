// =============================================================================
// Plik: tests/ws_binary_pipeline.rs
// Opis: Pipeline test dla WSS binary dispatch. Symuluje pelny flow:
//         1. Klient buduje Envelope + MessageBody (rkyv encode)
//         2. Serwer decoduje Envelope z bytecheck
//         3. Serwer decoduje MessageBody z envelope.body
//         4. Dispatch przez registry (dispatch::dispatch)
//         5. Response body + envelope encode
//         6. Klient decoduje envelope + body
//       Nie odpala realnego TCP/WS — testuje tylko protocol layer.
//       Pelny 3-nodowy e2e test (Task #34) wymaga spawned cluster.
// =============================================================================

use tentaflow_core::dispatch::{self, HandlerContext};
use tentaflow_protocol::{
    envelope::{message_kind, Envelope, EnvelopeFlags, Routing},
    MessageBody, SessionAuth,
};

/// Helper: encode klient -> serwer frame.
fn encode_request(correlation_id: u64, body: MessageBody) -> Vec<u8> {
    let body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&body).unwrap().to_vec();
    let env = Envelope::new_direct(correlation_id, 1, message_kind::META_HEARTBEAT, body_bytes);
    rkyv::to_bytes::<rkyv::rancor::Error>(&env).unwrap().to_vec()
}

/// Helper: serwer-side flow — decode envelope + body, dispatch, encode response.
fn server_handle(request_bytes: &[u8], session: SessionAuth) -> Vec<u8> {
    let env = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(request_bytes)
        .expect("decode envelope");
    assert!(matches!(env.routing, Routing::Direct));
    assert_eq!(env.schema_version, tentaflow_protocol::SCHEMA_VERSION);

    let body = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&env.body)
        .expect("decode body");

    let ctx = HandlerContext {
        session,
        correlation_id: env.correlation_id,
    };
    let (resp_body, is_error) = dispatch::dispatch(&body, &ctx);

    let resp_body_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&resp_body)
        .unwrap()
        .to_vec();
    let mut resp_env = Envelope::new_direct(
        env.correlation_id,
        1,
        env.message_kind,
        resp_body_bytes,
    );
    if is_error {
        resp_env.flags = EnvelopeFlags::IS_ERROR;
    }
    rkyv::to_bytes::<rkyv::rancor::Error>(&resp_env)
        .unwrap()
        .to_vec()
}

/// Helper: klient decoduje response, wyciaga body.
fn decode_response(bytes: &[u8]) -> (Envelope, MessageBody) {
    let env = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(bytes).expect("decode env");
    let body =
        rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(&env.body).expect("decode body");
    (env, body)
}

// =============================================================================
// Testy pipeline
// =============================================================================

#[test]
fn node_list_request_full_pipeline() {
    let req = encode_request(42, MessageBody::NodeListRequest);
    let resp = server_handle(
        &req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (env, body) = decode_response(&resp);
    assert_eq!(env.correlation_id, 42);
    assert!(!env.flags.contains(EnvelopeFlags::IS_ERROR));
    match body {
        MessageBody::NodeListResponse { nodes } => assert!(!nodes.is_empty()),
        other => panic!("expected NodeListResponse, got {:?}", other),
    }
}

#[test]
fn model_list_request_allows_anonymous() {
    let req = encode_request(1, MessageBody::ModelListRequest);
    let resp = server_handle(&req, SessionAuth::Anonymous);
    let (_env, body) = decode_response(&resp);
    match body {
        MessageBody::ModelListResponse { models } => assert!(!models.is_empty()),
        other => panic!("expected ModelListResponse, got {:?}", other),
    }
}

#[test]
fn anonymous_session_denied_for_admin_handler() {
    let req = encode_request(7, MessageBody::NodeListRequest);
    let resp = server_handle(&req, SessionAuth::Anonymous);
    let (env, body) = decode_response(&resp);
    assert!(env.flags.contains(EnvelopeFlags::IS_ERROR));
    match body {
        MessageBody::Error(e) => assert_eq!(
            e.code,
            tentaflow_protocol::ProtocolErrorCode::PolicyDenied
        ),
        other => panic!("expected Error(PolicyDenied), got {:?}", other),
    }
}

#[test]
fn auth_login_then_auth_me_flow() {
    use tentaflow_protocol::{AuthLoginRequest, AuthMeResponse};

    // Login step — Anonymous OK
    let login_req = encode_request(
        1,
        MessageBody::AuthLoginRequestBody(AuthLoginRequest {
            username: "admin".to_string(),
            password: "s3cret".to_string(),
        }),
    );
    let login_resp = server_handle(&login_req, SessionAuth::Anonymous);
    let (_, login_body) = decode_response(&login_resp);
    let _jwt = match login_body {
        MessageBody::AuthLoginResponseBody(resp) => resp.jwt,
        other => panic!("expected AuthLoginResponse, got {:?}", other),
    };

    // AuthMe — wymaga UserSession (po login bedzie nowa sesja z user_id z JWT)
    let me_req = encode_request(2, MessageBody::AuthMeRequest);
    let me_resp = server_handle(
        &me_req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (_, me_body) = decode_response(&me_resp);
    let _: AuthMeResponse = match me_body {
        MessageBody::AuthMeResponseBody(r) => r,
        other => panic!("expected AuthMeResponse, got {:?}", other),
    };
}

#[test]
fn dashboard_metrics_request_returns_snapshot() {
    let req = encode_request(99, MessageBody::DashboardMetricsRequest);
    let resp = server_handle(
        &req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (_, body) = decode_response(&resp);
    assert!(matches!(body, MessageBody::DashboardMetricsResponse(_)));
}

#[test]
fn mesh_peers_list_response_contains_self() {
    let req = encode_request(100, MessageBody::MeshPeersListRequest);
    let resp = server_handle(
        &req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (_, body) = decode_response(&resp);
    match body {
        MessageBody::MeshPeersListResponse { peers } => assert!(!peers.is_empty()),
        other => panic!("expected MeshPeersListResponse, got {:?}", other),
    }
}

#[test]
fn settings_update_returns_applied_count() {
    use tentaflow_protocol::{SettingEntry, SettingsUpdateRequest};
    let req = encode_request(
        200,
        MessageBody::SettingsUpdateRequestBody(SettingsUpdateRequest {
            entries: vec![
                SettingEntry {
                    key: "theme".into(),
                    value: "dark".into(),
                    is_secret: false,
                },
                SettingEntry {
                    key: "api_key".into(),
                    value: "s3cret".into(),
                    is_secret: true,
                },
            ],
        }),
    );
    let resp = server_handle(
        &req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (_, body) = decode_response(&resp);
    match body {
        MessageBody::SettingsUpdateResponse { applied } => assert_eq!(applied, 2),
        other => panic!("expected SettingsUpdateResponse, got {:?}", other),
    }
}

#[test]
fn correlation_id_preserved_across_pipeline() {
    for correlation_id in [1u64, 42, 1_000_000, u64::MAX] {
        let req = encode_request(correlation_id, MessageBody::NodeListRequest);
        let resp = server_handle(
            &req,
            SessionAuth::UserSession {
                user_id: [0u8; 16],
            },
        );
        let (env, _) = decode_response(&resp);
        assert_eq!(
            env.correlation_id, correlation_id,
            "correlation_id {} lost w pipeline",
            correlation_id
        );
    }
}

#[test]
fn unknown_variant_returns_not_implemented() {
    // MessageBody::Error nie ma handlera — dispatch zwraca NotImplemented.
    let req = encode_request(
        1,
        MessageBody::Error(tentaflow_protocol::ProtocolError {
            code: tentaflow_protocol::ProtocolErrorCode::Internal,
            message: "synthetic".to_string(),
            trace_id: None,
        }),
    );
    let resp = server_handle(
        &req,
        SessionAuth::UserSession {
            user_id: [0u8; 16],
        },
    );
    let (env, body) = decode_response(&resp);
    assert!(env.flags.contains(EnvelopeFlags::IS_ERROR));
    match body {
        MessageBody::Error(e) => assert_eq!(
            e.code,
            tentaflow_protocol::ProtocolErrorCode::NotImplemented
        ),
        other => panic!("expected Error(NotImplemented), got {:?}", other),
    }
}
