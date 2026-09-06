// ============ File: tests/flow_factory_restore_test.rs ============
//
// `FlowFactoryRestoreRequest` resets a factory flow (`FACTORY_FLOW_IDS`) to
// its canonical graph: the edited graph must land in `flow_versions` first
// (so the restore is undoable), the row must come back `active` with the
// factory JSON, Default Chat must regain `is_default=1`/`service_type='chat'`,
// and a non-factory id must be refused. Runs the real handler on
// `AppState::for_test()`.

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::db::seed::{factory_flow_json, DEFAULT_CHAT_FLOW_ID, MEETING_BOT_FLOW_ID};
use tentaflow_core::dispatch::handlers::{flow_delete, flow_factory_restore, flow_list};
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{FlowFactoryRestoreRequest, MessageBody, ProtocolErrorCode, SessionAuth};

const EDITED_JSON: &str = "{\"nodes\":[],\"edges\":[]}";
const USER_FLOW_ID: &str = "user-flow-under-test";

fn admin_ctx(state: Arc<AppState>) -> HandlerContext {
    let mut user_id_bytes = [0u8; 16];
    user_id_bytes[0] = 0xFF;
    user_id_bytes[8..].copy_from_slice(&7i64.to_le_bytes());
    // `flow_versions.created_by` references `user_accounts`, so the acting
    // admin must exist before the restore snapshots the current graph.
    {
        let conn = state.db.write().expect("db write lock");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts (id, username, password_hash, display_name) \
             VALUES (?1, 'factory-restore-admin', 'x', 'factory-restore-admin')",
            rusqlite::params![uuid::Uuid::from_bytes(user_id_bytes).to_string()],
        )
        .expect("insert admin user");
    }
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

/// Puts a flow row into an EDITED, non-factory state (draft, not default, no
/// service_type), as a user could leave it through `flow_update`. `db::init`
/// already seeds the factory rows, so this is an upsert.
fn seed_edited_flow(state: &AppState, id: &str) {
    let conn = state.db.write().expect("db write lock");
    conn.execute(
        "INSERT INTO flows (id, name, flow_json, status, is_default, is_system) \
         VALUES (?1, 'Edited Flow', ?2, 'draft', 0, 0) \
         ON CONFLICT(id) DO UPDATE SET flow_json = excluded.flow_json, \
         status = 'draft', is_default = 0, service_type = NULL, is_system = 0",
        rusqlite::params![id, EDITED_JSON],
    )
    .expect("seed flow");
    conn.execute(
        "DELETE FROM flow_versions WHERE flow_id = ?1",
        rusqlite::params![id],
    )
    .expect("clear versions");
}

fn row_state(state: &AppState, id: &str) -> (String, String, i64, Option<String>) {
    let conn = state.db.write().expect("db write lock");
    conn.query_row(
        "SELECT flow_json, status, is_default, service_type FROM flows WHERE id = ?1",
        rusqlite::params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .expect("flow row")
}

fn version_jsons(state: &AppState, id: &str) -> Vec<String> {
    let conn = state.db.write().expect("db write lock");
    let mut stmt = conn
        .prepare("SELECT flow_json FROM flow_versions WHERE flow_id = ?1 ORDER BY version_num")
        .expect("prepare");
    stmt.query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

fn restore(
    ctx: &HandlerContext,
    id: &str,
) -> Result<MessageBody, tentaflow_protocol::ProtocolError> {
    flow_factory_restore(
        &MessageBody::FlowFactoryRestoreRequestBody(FlowFactoryRestoreRequest {
            flow_id: id.to_string(),
        }),
        ctx,
    )
}

#[test]
fn default_chat_restore_resets_graph_and_routing_identity() {
    let state = AppState::for_test();
    seed_edited_flow(&state, DEFAULT_CHAT_FLOW_ID);
    let ctx = admin_ctx(state.clone());

    let resp = restore(&ctx, DEFAULT_CHAT_FLOW_ID).expect("factory restore");
    let MessageBody::FlowDetailResponse(detail) = resp else {
        panic!("expected FlowDetailResponse, got {resp:?}");
    };
    let factory = factory_flow_json(DEFAULT_CHAT_FLOW_ID).expect("factory json");
    assert_eq!(detail.graph_json, factory);
    assert!(detail.is_factory);
    assert!(detail.enabled);

    let (flow_json, status, is_default, service_type) = row_state(&state, DEFAULT_CHAT_FLOW_ID);
    assert_eq!(flow_json, factory);
    assert_eq!(status, "active");
    assert_eq!(is_default, 1);
    assert_eq!(service_type.as_deref(), Some("chat"));

    // The pre-restore graph is kept as a version, so the restore is undoable.
    assert_eq!(
        version_jsons(&state, DEFAULT_CHAT_FLOW_ID),
        vec![EDITED_JSON.to_string()]
    );
}

#[test]
fn meeting_bot_restore_keeps_routing_untouched() {
    let state = AppState::for_test();
    seed_edited_flow(&state, MEETING_BOT_FLOW_ID);
    let ctx = admin_ctx(state.clone());

    restore(&ctx, MEETING_BOT_FLOW_ID).expect("factory restore");

    let (flow_json, status, is_default, service_type) = row_state(&state, MEETING_BOT_FLOW_ID);
    assert_eq!(
        flow_json,
        factory_flow_json(MEETING_BOT_FLOW_ID).expect("factory json")
    );
    assert_eq!(status, "active");
    assert_eq!(is_default, 0);
    assert_eq!(service_type, None);
    assert_eq!(version_jsons(&state, MEETING_BOT_FLOW_ID).len(), 1);
}

#[test]
fn non_factory_flow_is_refused_and_untouched() {
    let state = AppState::for_test();
    seed_edited_flow(&state, USER_FLOW_ID);
    let ctx = admin_ctx(state.clone());

    let err = restore(&ctx, USER_FLOW_ID).expect_err("non-factory id must be refused");
    assert_eq!(err.code, ProtocolErrorCode::BadRequest, "{}", err.message);
    assert!(
        err.message.contains("not a factory flow"),
        "{}",
        err.message
    );

    let (flow_json, status, _, _) = row_state(&state, USER_FLOW_ID);
    assert_eq!(flow_json, EDITED_JSON);
    assert_eq!(status, "draft");
    assert!(version_jsons(&state, USER_FLOW_ID).is_empty());
}

#[test]
fn factory_flow_cannot_be_deleted_and_is_flagged_in_list() {
    let state = AppState::for_test();
    seed_edited_flow(&state, DEFAULT_CHAT_FLOW_ID);
    seed_edited_flow(&state, USER_FLOW_ID);
    let ctx = admin_ctx(state.clone());

    let err = flow_delete(
        &MessageBody::FlowDeleteRequest {
            flow_id: DEFAULT_CHAT_FLOW_ID.to_string(),
        },
        &ctx,
    )
    .expect_err("factory flow delete must be refused");
    assert_eq!(err.code, ProtocolErrorCode::BadRequest, "{}", err.message);

    let MessageBody::FlowListResponse { flows } =
        flow_list(&MessageBody::FlowListRequest, &ctx).expect("flow list")
    else {
        panic!("expected FlowListResponse");
    };
    let by_id = |id: &str| flows.iter().find(|f| f.id == id).expect("flow in list");
    assert!(by_id(DEFAULT_CHAT_FLOW_ID).is_factory);
    assert!(!by_id(USER_FLOW_ID).is_factory);
}
