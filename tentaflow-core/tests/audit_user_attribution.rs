// =============================================================================
// File: tests/audit_user_attribution.rs — F1c-P7 per-user audit attribution
// =============================================================================
//
// Validates that an authenticated operator's user_id propagates from
// `AddonState.user_id` to:
//
//   1. `flow_invocations.actor_user_id` when an addon calls `flow_invoke_v1`
//      (migration v31 column).
//   2. `audit_log.user_id` when an addon emits an audit row via
//      `audit_log_with_risk` (the shared write path used by every F1c host fn).
//
// Companion negative test asserts that system-originated invocations
// (state.user_id = None, is_system_call = true) record NULL in both columns,
// so DoD-9 / DoD-10 reports can distinguish "operator action" from
// "background / boot / mesh task" purely from the row.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;

use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::audit_log_with_risk;
use tentaflow_core::addon::host_functions::flow::test_api as flow_api;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::audit::RiskClass;
use tentaflow_core::db::{init as db_init, DbPool};
use tentaflow_core::flow_runtime::scheduler::FlowScheduler;

// -----------------------------------------------------------------------------
// Fixture helpers
// -----------------------------------------------------------------------------

fn fresh_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("test db")
}

fn make_state(
    db: DbPool,
    addon_id: &str,
    user_id: Option<String>,
    is_system_call: bool,
) -> AddonState {
    let event_bus = Arc::new(EventBus::new());
    let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));

    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "audit-attribution-instance".to_string(),
        user_id,
        org_id: None,
        db,
        permissions: vec![flow_api::PERM_FLOW_INVOKE.to_string()],
        event_bus,
        permission_checker,
        fuel_consumed: 0,
        is_system_call,
        rate_limiter: None,
        net_manager: Arc::new(PlMutex::new(
            tentaflow_core::addon::host_functions::network::NetworkConnectionManager::new(),
        )),
        settings_cipher,
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 64 * 1024 * 1024,
        oauth_refresh_guard: Arc::new(
            tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
        ),
        router: None,
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

/// Insert a `user_accounts` row with the given UUID so the `actor_user_id` FK
/// on `flow_invocations` is satisfiable. Production callers receive their id
/// from the auth layer after a real user exists; this fixture mirrors that
/// invariant inside the in-memory test DB.
fn ensure_user(db: &DbPool, id: &str, username: &str) {
    let conn = db.write().expect("db write");
    conn.execute(
        "INSERT INTO user_accounts (id, username, password_hash, display_name) \
         VALUES (?1, ?2, 'x', ?2)",
        rusqlite::params![id, username],
    )
    .expect("insert user");
}

/// Plant an `allow` default for `flow.invoke` on the given addon so the
/// permission check passes when the test runs as a real authenticated user
/// (rather than is_system_call=true). Without this the dispatcher rejects
/// with AbiError::Permission because no per-user grant exists.
fn grant_flow_invoke_default(db: &DbPool, addon_id: &str) {
    let conn = db.write().expect("db write");
    conn.execute(
        "INSERT INTO addon_permission_defaults (addon_id, permission_id, grant_mode) \
         VALUES (?1, ?2, 'allow')",
        rusqlite::params![addon_id, flow_api::PERM_FLOW_INVOKE],
    )
    .expect("insert default allow");
}

/// Register a single-source DAG in the global flow registry under the given
/// addon_id. Returns the flow_id the invocation should reference. The exact
/// shape doesn't matter for this suite — the assertions target row attribution,
/// not the DAG result.
fn register_minimal_flow(addon_id: &str) -> String {
    use tentaflow_core::flow_runtime::parser::{compile, parse_flow_definition};
    use tentaflow_core::flow_runtime::registry;

    let flow_id = format!("attr-flow-{}", addon_id);
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{}} }},
                {{ "id": "snk", "type": "Sink",   "params": {{}} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    let def = parse_flow_definition(&json).expect("parse");
    let compiled = Arc::new(compile(def).expect("compile"));
    registry::global().register(addon_id, compiled);
    flow_id
}

// -----------------------------------------------------------------------------
// 1. flow_invoke records the addon caller's user_id on the invocation row.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_records_actor_user_id() {
    let db = fresh_db();
    let addon_id = "audit-attr-user";
    ensure_user(&db, "00000000-0000-0000-0000-000000000042", "alice");
    let flow_id = register_minimal_flow(addon_id);
    grant_flow_invoke_default(&db, addon_id);
    let sched = Arc::new(FlowScheduler::new(db.clone()));

    // Authenticated operator user_id = 42.
    let state = make_state(
        db.clone(),
        addon_id,
        Some("00000000-0000-0000-0000-000000000042".to_string()),
        false,
    );
    state.permission_checker.refresh_all();
    let payload = flow_api::FlowInvokeInput {
        flow_id: flow_id.clone(),
        input_toml: Some("value = 1\n".to_string()),
        wait_ms: 5000,
    };

    let outcome = {
        let sched = sched.clone();
        tokio::task::spawn_blocking(move || flow_api::dispatch_invoke(&state, &sched, &payload))
            .await
            .expect("join")
    };
    let out = match outcome {
        flow_api::DispatchOutcome::Ok(o) => o,
        flow_api::DispatchOutcome::Err(e) => panic!("dispatch must succeed, got Err({:?})", e),
    };

    // SELECT actor_user_id FROM flow_invocations WHERE id = ?
    let conn = db.read().expect("db read");
    let recorded: Option<String> = conn
        .query_row(
            "SELECT actor_user_id FROM flow_invocations WHERE id = ?1",
            rusqlite::params![out.invocation_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .expect("invocation row present");
    assert_eq!(
        recorded,
        Some("00000000-0000-0000-0000-000000000042".to_string()),
        "flow_invocations.actor_user_id must mirror AddonState.user_id"
    );
}

// -----------------------------------------------------------------------------
// 2. System-originated invocations (no user) record NULL — provides the
//    contrast row DoD-9 / DoD-10 reports need to filter on.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_system_call_records_null_user_id() {
    let db = fresh_db();
    let addon_id = "audit-attr-system";
    let flow_id = register_minimal_flow(addon_id);
    let sched = Arc::new(FlowScheduler::new(db.clone()));

    // Background / boot path — no authenticated user, system-trusted call.
    let state = make_state(db.clone(), addon_id, None, true);
    let payload = flow_api::FlowInvokeInput {
        flow_id: flow_id.clone(),
        input_toml: Some("value = 1\n".to_string()),
        wait_ms: 5000,
    };

    let outcome = {
        let sched = sched.clone();
        tokio::task::spawn_blocking(move || flow_api::dispatch_invoke(&state, &sched, &payload))
            .await
            .expect("join")
    };
    let out = match outcome {
        flow_api::DispatchOutcome::Ok(o) => o,
        flow_api::DispatchOutcome::Err(e) => panic!("dispatch must succeed, got Err({:?})", e),
    };

    let conn = db.read().expect("db read");
    let recorded: Option<String> = conn
        .query_row(
            "SELECT actor_user_id FROM flow_invocations WHERE id = ?1",
            rusqlite::params![out.invocation_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .expect("invocation row present");
    assert_eq!(
        recorded, None,
        "system-call invocations must leave actor_user_id NULL"
    );
}

// -----------------------------------------------------------------------------
// 3. audit_log_with_risk carries AddonState.user_id — this is the shared write
//    path for every F1c host function (camera credentials rotate, vector,
//    sql, alias, gate, streaming, ...), so verifying it once at the helper
//    layer covers every caller without forcing the test to spin up real
//    camera state.
// -----------------------------------------------------------------------------

#[tokio::test]
async fn audit_log_with_risk_carries_actor_user_id() {
    let db = fresh_db();
    let addon_id = "audit-attr-hostfn";
    let state = make_state(
        db.clone(),
        addon_id,
        Some("00000000-0000-0000-0000-000000000007".to_string()),
        false,
    );

    audit_log_with_risk(
        &state,
        "camera.credentials_rotate",
        Some("camera"),
        Some("cam-1"),
        RiskClass::B,
        None,
        None,
        "ok",
        None,
    );

    let conn = db.read().expect("db read");
    let recorded: Option<String> = conn
        .query_row(
            "SELECT user_id FROM audit_log \
             WHERE addon_id = ?1 AND action = 'camera.credentials_rotate'",
            rusqlite::params![addon_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .expect("audit row present");
    assert_eq!(
        recorded,
        Some("00000000-0000-0000-0000-000000000007".to_string()),
        "audit_log.user_id must mirror AddonState.user_id"
    );
}
