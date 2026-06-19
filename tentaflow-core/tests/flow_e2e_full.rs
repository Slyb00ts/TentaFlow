// =============================================================================
// File: tests/flow_e2e_full.rs — F1c-P5 chunk E end-to-end integration
// =============================================================================
//
// Full production-shaped lifecycle for a flow-bearing addon:
//
//   1. Copy fixtures/test_flow_addon/* into a fresh tempdir.
//   2. `addon::lifecycle::install(...)` — parses manifest.toml, compiles
//      `flows/simple.flow.json`, lands the addon row, registers the
//      CompiledFlow with the global registry.
//   3. Build a minimal `AddonState` that carries the `flow.invoke` permission
//      and dispatch `flow_invoke_v1` via the same path the WASM ABI takes
//      (`flow::test_api::dispatch_invoke`).
//   4. Assert on the dispatch result + audit_log rows + registry mutations
//      observed by other addons.
//
// What this exercises that the per-layer tests do not:
//   * lifecycle::install → registry::register handoff is real (no manual
//     register_addon helper).
//   * Permission catalog read at dispatch time reflects the manifest's
//     [[permission]] declaration.
//   * Cross-addon isolation survives an end-to-end install for both sides.
//   * audit_log accumulates the full action sequence
//     (flow.invoke + flow.op.source.* + flow.op.threshold.* + flow.op.sink.*).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;

use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::flow::test_api as flow_api;
use tentaflow_core::addon::lifecycle;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::db::{init as db_init, DbPool};
use tentaflow_core::flow_runtime::registry;
use tentaflow_core::flow_runtime::scheduler::FlowScheduler;

const FIXTURE_ADDON_ID: &str = "test-flow-runner";
const FIXTURE_FLOW_ID: &str = "tv-test-simple";

// -----------------------------------------------------------------------------
// Fixture handling
// -----------------------------------------------------------------------------

fn fixture_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/test_flow_addon");
    p
}

/// Mirror the on-disk fixture into a fresh tempdir. The lifecycle::install
/// path canonicalizes the addon dir and writes per-addon SQL state, so giving
/// each test its own tempdir avoids parallel-run interference.
fn stage_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = fixture_root();
    copy_tree(&src, tmp.path());
    tmp
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("dirent");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            std::fs::create_dir_all(&target).expect("mkdir target");
            copy_tree(&path, &target);
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::copy(&path, &target).expect("copy file");
        }
    }
}

fn fresh_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("test db")
}

/// Install the fixture addon under a per-test alias. Each test rewrites
/// `manifest.toml` with a unique addon_id so the global flow registry and
/// shared in-memory DB do not collide across tests in the same binary.
fn install_with_unique_id(db: &DbPool, suffix: &str) -> (String, tempfile::TempDir) {
    let tmp = stage_fixture();
    let addon_id = format!("{FIXTURE_ADDON_ID}-{suffix}");
    rewrite_addon_id(tmp.path(), &addon_id);
    // Pre-clean — protect against leftover registry state from a prior run.
    registry::global().unregister_addon(&addon_id);
    lifecycle::install(tmp.path(), db).expect("install must succeed");
    (addon_id, tmp)
}

fn rewrite_addon_id(addon_dir: &Path, new_id: &str) {
    let manifest_path = addon_dir.join("manifest.toml");
    let contents = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let patched = contents.replace(
        &format!("id = \"{FIXTURE_ADDON_ID}\""),
        &format!("id = \"{new_id}\""),
    );
    assert_ne!(
        patched, contents,
        "fixture manifest must contain the placeholder addon id"
    );
    std::fs::write(&manifest_path, patched).expect("write manifest");
}

// -----------------------------------------------------------------------------
// AddonState fixture — mirrors tests/flow_host_functions.rs::make_state.
// -----------------------------------------------------------------------------

fn make_state(db: DbPool, addon_id: &str, permissions: Vec<String>) -> AddonState {
    let event_bus = Arc::new(EventBus::new());
    let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));

    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "flow-e2e-instance".to_string(),
        user_id: None,
        org_id: None,
        db,
        permissions,
        event_bus,
        permission_checker,
        fuel_consumed: 0,
        is_system_call: true,
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

// -----------------------------------------------------------------------------
// audit_log helpers
// -----------------------------------------------------------------------------

fn count_audit_action(db: &DbPool, addon_id: &str, action_like: &str) -> i64 {
    let conn = db.read().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE addon_id = ?1 AND action LIKE ?2",
        rusqlite::params![addon_id, action_like],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

fn count_audit_exact(
    db: &DbPool,
    addon_id: &str,
    action: &str,
    result: &str,
    risk_class: &str,
) -> i64 {
    let conn = db.read().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log \
         WHERE addon_id = ?1 AND action = ?2 AND result = ?3 AND risk_class = ?4",
        rusqlite::params![addon_id, action, result, risk_class],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// 1. Install → invoke (passing threshold) → 3 records survive.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_e2e_install_then_invoke_returns_filtered_records() {
    let db = fresh_db();
    let (addon_id, _tmp) = install_with_unique_id(&db, "pass");

    // Registry must show the compiled flow after install.
    let listed = registry::global().list_for_addon(&addon_id);
    assert_eq!(
        listed,
        vec![FIXTURE_FLOW_ID.to_string()],
        "install must register exactly one flow"
    );
    let compiled = registry::global()
        .get(&addon_id, FIXTURE_FLOW_ID)
        .expect("flow registered after install");
    assert_eq!(compiled.def.operators.len(), 3);

    // Dispatch through the same path the WASM ABI uses.
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let state = make_state(
        db.clone(),
        &addon_id,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let payload = flow_api::FlowInvokeInput {
        flow_id: FIXTURE_FLOW_ID.to_string(),
        input_toml: Some("value = 10.0\n".to_string()),
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

    assert_eq!(out.status, "completed", "expected completed, got {:?}", out);
    assert_eq!(out.operators_total, 3, "DAG has 3 operators");
    assert_eq!(
        out.operators_completed, 3,
        "every operator must report completed"
    );
    assert!(
        out.finished_at.is_some(),
        "completed must carry finished_at"
    );

    // Sink emits result_toml = `[[records]]` table array. value=10.0 passes
    // threshold (min=5.0) so all 3 source records survive.
    let result_toml = out
        .result_toml
        .as_deref()
        .expect("result_toml present on completion");
    let parsed: toml::Value = toml::from_str(result_toml).expect("decode result_toml");
    let records = parsed
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(records.len(), 3, "expected 3 records, got {records:?}");
}

// -----------------------------------------------------------------------------
// 2. Threshold drops below-floor values.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_e2e_threshold_filters_low_values() {
    let db = fresh_db();
    let (addon_id, _tmp) = install_with_unique_id(&db, "drop");

    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let state = make_state(
        db.clone(),
        &addon_id,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let payload = flow_api::FlowInvokeInput {
        flow_id: FIXTURE_FLOW_ID.to_string(),
        input_toml: Some("value = 2.0\n".to_string()),
        wait_ms: 5000,
    };

    let outcome = tokio::task::spawn_blocking({
        let sched = sched.clone();
        move || flow_api::dispatch_invoke(&state, &sched, &payload)
    })
    .await
    .expect("join");
    let out = match outcome {
        flow_api::DispatchOutcome::Ok(o) => o,
        flow_api::DispatchOutcome::Err(e) => panic!("expected Ok, got Err({:?})", e),
    };
    assert_eq!(out.status, "completed");

    let result_toml = out
        .result_toml
        .as_deref()
        .expect("result_toml present on completion");
    let parsed: toml::Value = toml::from_str(result_toml).expect("decode result_toml");
    let records = parsed
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        records.is_empty(),
        "threshold must drop all records below floor, got {records:?}"
    );
}

// -----------------------------------------------------------------------------
// 3. Uninstall purges the flow from the registry.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_e2e_addon_uninstall_drops_flow() {
    let db = fresh_db();
    let (addon_id, _tmp) = install_with_unique_id(&db, "uninstall");

    assert!(
        registry::global().get(&addon_id, FIXTURE_FLOW_ID).is_some(),
        "flow registered after install"
    );

    lifecycle::uninstall(&addon_id, &db).expect("uninstall must succeed");

    assert!(
        registry::global().get(&addon_id, FIXTURE_FLOW_ID).is_none(),
        "uninstall must drop the flow from the registry"
    );

    // Dispatch through the ABI now surfaces NotFound rather than executing
    // a template owned by a removed addon.
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let state = make_state(
        db.clone(),
        &addon_id,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let payload = flow_api::FlowInvokeInput {
        flow_id: FIXTURE_FLOW_ID.to_string(),
        input_toml: Some("value = 10.0\n".to_string()),
        wait_ms: 1000,
    };
    let outcome = tokio::task::spawn_blocking({
        let sched = sched.clone();
        move || flow_api::dispatch_invoke(&state, &sched, &payload)
    })
    .await
    .expect("join");
    match outcome {
        flow_api::DispatchOutcome::Err(AbiError::NotFound) => {}
        other => panic!("expected NotFound after uninstall, got {:?}", debug(&other)),
    }
}

// -----------------------------------------------------------------------------
// 4. audit_log captures the full invocation lifecycle.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_e2e_audit_log_records_invocation_lifecycle() {
    let db = fresh_db();
    let (addon_id, _tmp) = install_with_unique_id(&db, "audit");

    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let state = make_state(
        db.clone(),
        &addon_id,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let payload = flow_api::FlowInvokeInput {
        flow_id: FIXTURE_FLOW_ID.to_string(),
        input_toml: Some("value = 10.0\n".to_string()),
        wait_ms: 5000,
    };
    let outcome = tokio::task::spawn_blocking({
        let sched = sched.clone();
        move || flow_api::dispatch_invoke(&state, &sched, &payload)
    })
    .await
    .expect("join");
    assert!(
        matches!(outcome, flow_api::DispatchOutcome::Ok(_)),
        "invoke must succeed before checking audit rows"
    );

    // flow.invoke: dispatch emits one row per call, result='ok', risk='B'.
    let invoke_ok = count_audit_exact(&db, &addon_id, "flow.invoke", "ok", "B");
    assert!(
        invoke_ok >= 1,
        "expected >=1 flow.invoke ok/B row, got {invoke_ok}"
    );

    // Per-operator audit trails — Source emits at least start+completed,
    // Threshold at least completed, Sink at least completed.
    let source_rows = count_audit_action(&db, &addon_id, "flow.op.source.%");
    assert!(
        source_rows >= 1,
        "expected >=1 flow.op.source.* row, got {source_rows}"
    );
    let threshold_rows = count_audit_action(&db, &addon_id, "flow.op.threshold.%");
    assert!(
        threshold_rows >= 1,
        "expected >=1 flow.op.threshold.* row, got {threshold_rows}"
    );
    let sink_rows = count_audit_action(&db, &addon_id, "flow.op.sink.%");
    assert!(
        sink_rows >= 1,
        "expected >=1 flow.op.sink.* row, got {sink_rows}"
    );
}

// -----------------------------------------------------------------------------
// 5. Cross-addon isolation: addon B cannot read addon A's invocation status.
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_e2e_cross_addon_cannot_read_status() {
    let db = fresh_db();
    let (addon_a, _tmp_a) = install_with_unique_id(&db, "xa-a");
    let (addon_b, _tmp_b) = install_with_unique_id(&db, "xa-b");

    let sched = Arc::new(FlowScheduler::new(db.clone()));

    // A invokes its own flow synchronously; the returned invocation_id is
    // owned by addon A.
    let state_a = make_state(
        db.clone(),
        &addon_a,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let payload = flow_api::FlowInvokeInput {
        flow_id: FIXTURE_FLOW_ID.to_string(),
        input_toml: Some("value = 10.0\n".to_string()),
        wait_ms: 5000,
    };
    let started = tokio::task::spawn_blocking({
        let sched = sched.clone();
        move || flow_api::dispatch_invoke(&state_a, &sched, &payload)
    })
    .await
    .expect("join");
    let invocation_id = match started {
        flow_api::DispatchOutcome::Ok(o) => o.invocation_id.clone(),
        flow_api::DispatchOutcome::Err(e) => panic!("addon A invoke must succeed: {:?}", e),
    };
    assert!(
        !invocation_id.is_empty(),
        "scheduler must return a non-empty invocation_id"
    );

    // B (different addon_id) holds the same permission but queries A's
    // invocation — scheduler.status() filters by addon_id so this must
    // surface NotFound at the dispatch layer.
    let state_b = make_state(
        db.clone(),
        &addon_b,
        vec![flow_api::PERM_FLOW_INVOKE.to_string()],
    );
    let status_payload = flow_api::FlowInvocationIdInput {
        invocation_id: invocation_id.clone(),
    };
    let outcome = tokio::task::spawn_blocking({
        let sched = sched.clone();
        move || flow_api::dispatch_status(&state_b, &sched, &status_payload)
    })
    .await
    .expect("join");
    match outcome {
        flow_api::DispatchOutcome::Err(AbiError::NotFound) => {}
        other => panic!(
            "expected NotFound for cross-addon status read, got {:?}",
            debug(&other)
        ),
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn debug<T: std::fmt::Debug>(o: &flow_api::DispatchOutcome<T>) -> String {
    match o {
        flow_api::DispatchOutcome::Ok(v) => format!("Ok({:?})", v),
        flow_api::DispatchOutcome::Err(e) => format!("Err({:?})", e),
    }
}
