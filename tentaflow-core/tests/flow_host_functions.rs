// =============================================================================
// File: tests/flow_host_functions.rs
// Purpose: F1c P5 chunk D — integration tests for the flow_invoke_v1 /
//          flow_status_v1 / flow_cancel_v1 host functions. The wasmtime ABI
//          shells are thin pass-throughs to `dispatch_*` + `run_*` helpers;
//          this file exercises both the engine-level error mapping (no
//          AddonState required) and the permission-gated dispatch path
//          (minimal AddonState fixture, mirrors `network.rs` test pattern).
// =============================================================================

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as PlMutex;

use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::flow::test_api as flow_api;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonCallProvenance, AddonManifest, AddonState};
use tentaflow_core::flow_runtime::parser::{compile, parse_flow_definition};
use tentaflow_core::flow_runtime::registry;
use tentaflow_core::flow_runtime::scheduler::FlowScheduler;
use tentaflow_core::flow_runtime::types::CompiledFlow;

fn fresh_db() -> tentaflow_core::db::DbPool {
    tentaflow_core::db::init(Path::new(":memory:")).expect("test db")
}

fn unique_addon(tag: &str) -> String {
    format!("flow-host-{tag}-{}", uuid::Uuid::new_v4())
}

fn make_flow(id: &str) -> Arc<CompiledFlow> {
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{}} }},
                {{ "id": "snk", "type": "Sink",   "params": {{}} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    let def = parse_flow_definition(&json).expect("parse");
    Arc::new(compile(def).expect("compile"))
}

/// Minimal AddonState fixture mirroring `network.rs::make_state`. The
/// dispatch_* layer only touches `addon_id`, `db`, `permissions`,
/// `permission_checker`, `user_id`, and `is_system_call`; the heavy WASI
/// context and net_manager are still constructed because `AddonState` does
/// not derive Default.
fn make_state(
    db: tentaflow_core::db::DbPool,
    addon_id: &str,
    permissions: Vec<String>,
) -> AddonState {
    let event_bus = Arc::new(EventBus::new());
    let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));

    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "flow-host-test-instance".to_string(),
        user_id: None,
        org_id: None,
        db,
        permissions,
        event_bus,
        permission_checker,
        fuel_consumed: 0,
        // `is_system_call=true` lets `check_permission` accept a permission
        // declared by the addon even with `user_id == None`; the
        // alternative would be a full user-acl seed which is orthogonal to
        // what these tests cover.
        is_system_call: true,
        call_provenance: AddonCallProvenance::addon(),
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
// Permission gating (dispatch layer)
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_denied_without_permission() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("perm-deny");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    // No permissions declared → dispatch must return Permission.
    let state = make_state(db, &addon, vec![]);
    let payload = flow_api::FlowInvokeInput {
        flow_id: flow_id.clone(),
        input_toml: None,
        wait_ms: 100,
    };
    let out =
        tokio::task::spawn_blocking(move || flow_api::dispatch_invoke(&state, &sched, &payload))
            .await
            .expect("join");
    match out {
        flow_api::DispatchOutcome::Err(AbiError::Permission) => {}
        other => panic!("expected Permission, got {:?}", abi_label(&other)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_permission_check_precedes_payload_parse() {
    // A caller without `flow.invoke` must be rejected with Permission even when
    // the operator payload is malformed. If the capability check did not run
    // first, dispatch_invoke would parse `input_toml`, hit the bad-input branch
    // and return Operation instead — proving the permission boundary sits ahead
    // of attacker-controlled payload parsing.
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("perm-precedes");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    let state = make_state(db, &addon, vec![]);
    let payload = flow_api::FlowInvokeInput {
        flow_id: flow_id.clone(),
        input_toml: Some("this = is = not = valid = toml".to_string()),
        wait_ms: 100,
    };
    let out =
        tokio::task::spawn_blocking(move || flow_api::dispatch_invoke(&state, &sched, &payload))
            .await
            .expect("join");
    match out {
        flow_api::DispatchOutcome::Err(AbiError::Permission) => {}
        other => panic!(
            "expected Permission (capability before payload parse), got {:?}",
            abi_label(&other)
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_ok_with_permission_returns_completed() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("perm-ok");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    let state = make_state(db, &addon, vec![flow_api::PERM_FLOW_INVOKE.to_string()]);
    let payload = flow_api::FlowInvokeInput {
        flow_id: flow_id.clone(),
        input_toml: None,
        wait_ms: 5000,
    };
    let out =
        tokio::task::spawn_blocking(move || flow_api::dispatch_invoke(&state, &sched, &payload))
            .await
            .expect("join");
    match out {
        flow_api::DispatchOutcome::Ok(o) => {
            assert_eq!(o.status, "completed", "got {:?}", o);
            assert!(o.finished_at.is_some());
            assert_eq!(o.operators_total, 2);
            assert!(o.result_toml.is_some());
        }
        flow_api::DispatchOutcome::Err(e) => panic!("expected Ok, got Err({:?})", e),
    }
}

// -----------------------------------------------------------------------------
// Engine-level mapping (pure run_* layer)
// -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_not_found_for_unknown_flow_id() {
    let sched = Arc::new(FlowScheduler::new(fresh_db()));
    let addon = unique_addon("notfound");
    let err = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let addon = addon.clone();
        move || {
            flow_api::run_invoke(
                &sched,
                &addon,
                "ghost-flow",
                toml::Value::Table(Default::default()),
                0,
                None,
                None,
            )
        }
    })
    .await
    .expect("join")
    .expect_err("must err");
    assert_eq!(err.0, AbiError::NotFound);
    assert_eq!(err.1, "flow_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_sync_returns_completed_within_wait_ms() {
    let sched = Arc::new(FlowScheduler::new(fresh_db()));
    let addon = unique_addon("sync");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    let out = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let addon = addon.clone();
        let flow_id = flow_id.clone();
        move || {
            flow_api::run_invoke(
                &sched,
                &addon,
                &flow_id,
                toml::Value::String("payload".into()),
                5_000,
                None,
                None,
            )
        }
    })
    .await
    .expect("join")
    .expect("run_invoke ok");
    assert_eq!(out.status, "completed");
    assert_eq!(out.operators_total, 2);
    assert!(out.finished_at.is_some());
    assert!(out.result_toml.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_invoke_async_returns_running_when_wait_ms_zero() {
    let sched = Arc::new(FlowScheduler::new(fresh_db()));
    let addon = unique_addon("async");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    let out = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let addon = addon.clone();
        let flow_id = flow_id.clone();
        move || {
            flow_api::run_invoke(
                &sched,
                &addon,
                &flow_id,
                toml::Value::Table(Default::default()),
                0,
                None,
                None,
            )
        }
    })
    .await
    .expect("join")
    .expect("run_invoke ok");
    assert_eq!(out.status, "running");
    assert!(out.finished_at.is_none());
    // result_toml is only populated on terminal Completed, never on running.
    assert!(out.result_toml.is_none());
    // Let the background task drain so the test runtime shuts down cleanly.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_status_cross_addon_returns_not_found() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db));
    let addon_a = unique_addon("a");
    let addon_b = unique_addon("b");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon_a, make_flow(&flow_id));

    // A starts an invocation; B tries to read it → NotFound (scheduler
    // filters by addon_id on the SELECT).
    let started = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let addon_a = addon_a.clone();
        let flow_id = flow_id.clone();
        move || {
            flow_api::run_invoke(
                &sched,
                &addon_a,
                &flow_id,
                toml::Value::Table(Default::default()),
                5_000,
                None,
                None,
            )
        }
    })
    .await
    .expect("join")
    .expect("invoke");

    let err = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let inv = started.invocation_id.clone();
        let addon_b = addon_b.clone();
        move || flow_api::run_status(&sched, &inv, &addon_b)
    })
    .await
    .expect("join")
    .expect_err("must err");
    assert_eq!(err.0, AbiError::NotFound);
    assert_eq!(err.1, "invocation_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow_cancel_marks_invocation_cancelled() {
    let sched = Arc::new(FlowScheduler::new(fresh_db()));
    let addon = unique_addon("cancel");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    let started = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let addon = addon.clone();
        let flow_id = flow_id.clone();
        move || {
            flow_api::run_invoke(
                &sched,
                &addon,
                &flow_id,
                toml::Value::Table(Default::default()),
                0,
                None,
                None,
            )
        }
    })
    .await
    .expect("join")
    .expect("invoke ok");

    // Cancel is idempotent regardless of whether the invocation has
    // already drained — the scheduler returns Ok for both in-flight and
    // terminal invocations as long as the addon owns the row.
    let cancel_out = tokio::task::spawn_blocking({
        let sched = sched.clone();
        let inv = started.invocation_id.clone();
        let addon = addon.clone();
        move || flow_api::run_cancel(&sched, &inv, &addon)
    })
    .await
    .expect("join")
    .expect("cancel ok");
    assert!(cancel_out.cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_invoke_concurrency_cap_quota_exceeded() {
    use tentaflow_core::flow_runtime::scheduler::DEFAULT_CONCURRENCY_CAP;
    let sched = Arc::new(FlowScheduler::new(fresh_db()));
    let addon = unique_addon("cap");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    registry::global().register(&addon, make_flow(&flow_id));

    // Oversubscribe 3x — the scheduler caps concurrent invocations at
    // DEFAULT_CONCURRENCY_CAP per addon. The blocking call must surface
    // QuotaExceeded for at least one of the racers.
    let attempts = (DEFAULT_CONCURRENCY_CAP * 3) as u32;
    let mut handles = Vec::new();
    for i in 0..attempts {
        let sched = sched.clone();
        let addon = addon.clone();
        let flow_id = flow_id.clone();
        handles.push(tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                flow_api::run_invoke(
                    &sched,
                    &addon,
                    &flow_id,
                    toml::Value::Integer(i as i64),
                    10_000,
                    None,
                    None,
                )
            })
            .await
            .expect("join")
        }));
    }

    let mut quota_hits = 0;
    for h in handles {
        if let Err(e) = h.await.unwrap() {
            if e.0 == AbiError::QuotaExceeded && e.1 == "concurrency_cap" {
                quota_hits += 1;
            }
        }
    }
    assert!(
        quota_hits >= 1,
        "expected ≥1 QuotaExceeded across {attempts} attempts"
    );
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn abi_label<T: std::fmt::Debug>(o: &flow_api::DispatchOutcome<T>) -> String {
    match o {
        flow_api::DispatchOutcome::Ok(v) => format!("Ok({:?})", v),
        flow_api::DispatchOutcome::Err(e) => format!("Err({:?})", e),
    }
}
