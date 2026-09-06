// ============ File: tests/flow_system_guard_test.rs ============
//
// A flow seeded with `is_system = 1` is owned by the platform: every mutating
// flow handler must refuse it, so it can never lose `is_default`, gain a
// `published_model_name`, flip its status, be rolled back or deleted through
// the binary protocol. Runs the real handlers on `AppState::for_test()`.

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::dispatch::handlers::{flow_delete, flow_update, flow_version_restore};
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{
    FlowUpdateRequest, FlowVersionRestoreRequest, MessageBody, ProtocolErrorCode, SessionAuth,
};

const SYSTEM_FLOW_ID: &str = "system-flow-under-test";

fn admin_ctx(state: Arc<AppState>) -> HandlerContext {
    let mut user_id_bytes = [0u8; 16];
    user_id_bytes[0] = 0xFF;
    user_id_bytes[8..].copy_from_slice(&7i64.to_le_bytes());
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: user_id_bytes,
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: "user-dispatch-test".to_string(),
            org_id: "org-default".to_string(),
            role_id: "role-test".to_string(),
            permissions: HashSet::new(),
        }),
    }
}

fn seed_system_flow(state: &AppState) {
    let conn = state.db.write().expect("db write lock");
    conn.execute(
        "INSERT INTO flows (id, name, flow_json, status, is_default, is_system) \
         VALUES (?1, 'System Flow', '{\"nodes\":[],\"edges\":[]}', 'active', 1, 1)",
        rusqlite::params![SYSTEM_FLOW_ID],
    )
    .expect("seed system flow");
    conn.execute(
        "INSERT INTO flow_versions (id, flow_id, version_num, flow_json, name, status) \
         VALUES ('v-old', ?1, 1, '{\"nodes\":[],\"edges\":[]}', 'System Flow', 'draft')",
        rusqlite::params![SYSTEM_FLOW_ID],
    )
    .expect("seed flow version");
}

fn row_state(state: &AppState) -> (i64, Option<String>, String, i64) {
    let conn = state.db.write().expect("db write lock");
    conn.query_row(
        "SELECT is_default, published_model_name, status, is_system FROM flows WHERE id = ?1",
        rusqlite::params![SYSTEM_FLOW_ID],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .expect("system flow row")
}

fn assert_refused(result: Result<MessageBody, tentaflow_protocol::ProtocolError>) {
    let err = result.expect_err("system flow mutation must be refused");
    assert_eq!(err.code, ProtocolErrorCode::BadRequest, "{}", err.message);
    assert!(err.message.contains("system flow"), "{}", err.message);
}

fn update_request(status: Option<&str>, published_model_name: Option<Option<&str>>) -> MessageBody {
    MessageBody::FlowUpdateRequestBody(FlowUpdateRequest {
        flow_id: SYSTEM_FLOW_ID.to_string(),
        name: None,
        description: None,
        flow_json: None,
        status: status.map(str::to_string),
        published_model_name: published_model_name.map(|p| p.map(str::to_string)),
    })
}

#[test]
fn system_flow_cannot_change_status() {
    let state = AppState::for_test();
    seed_system_flow(&state);
    let ctx = admin_ctx(state.clone());

    assert_refused(flow_update(&update_request(Some("draft"), None), &ctx));

    let (is_default, published, status, is_system) = row_state(&state);
    assert_eq!(
        (is_default, published, status.as_str(), is_system),
        (1, None, "active", 1)
    );
}

#[test]
fn system_flow_cannot_be_published() {
    let state = AppState::for_test();
    seed_system_flow(&state);
    let ctx = admin_ctx(state.clone());

    assert_refused(flow_update(
        &update_request(None, Some(Some("my-published-model"))),
        &ctx,
    ));

    let (is_default, published, _, _) = row_state(&state);
    assert_eq!(is_default, 1);
    assert_eq!(published, None);
}

#[test]
fn system_flow_cannot_be_restored_to_older_version() {
    let state = AppState::for_test();
    seed_system_flow(&state);
    let ctx = admin_ctx(state.clone());

    assert_refused(flow_version_restore(
        &MessageBody::FlowVersionRestoreRequestBody(FlowVersionRestoreRequest {
            flow_id: SYSTEM_FLOW_ID.to_string(),
            version_id: "v-old".to_string(),
        }),
        &ctx,
    ));

    let (_, _, status, _) = row_state(&state);
    assert_eq!(status, "active");
}

#[test]
fn system_flow_cannot_be_deleted() {
    let state = AppState::for_test();
    seed_system_flow(&state);
    let ctx = admin_ctx(state.clone());

    assert_refused(flow_delete(
        &MessageBody::FlowDeleteRequest {
            flow_id: SYSTEM_FLOW_ID.to_string(),
        },
        &ctx,
    ));

    let (_, _, _, is_system) = row_state(&state);
    assert_eq!(is_system, 1);
}
