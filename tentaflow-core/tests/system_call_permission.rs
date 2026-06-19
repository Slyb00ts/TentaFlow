// =============================================================================
// File: tests/system_call_permission.rs — CR-006 system-call permission path
// =============================================================================
//
// Regression coverage for the robot-advertisement break: a core-internal
// trusted read (e.g. the `<pkg>.status` refresh loop) runs as a GENUINE system
// call — the worker's `AddonState` carries `user_id = None` +
// `is_system_call = true`, so `check_permission` grants the addon's DECLARED
// permissions WITHOUT a per-user grant (CR-006 bypass).
//
// The negative cases prove the bypass is tightly scoped:
//   - a system call still cannot use a permission the addon did NOT declare;
//   - a non-system call with an ungranted user is rejected even when the addon
//     declares the permission (the per-user `permission_checker` still gates it).

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;

use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::check_permission;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::db::{init as db_init, DbPool};

fn fresh_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("test db")
}

/// Builds an `AddonState` mirroring how a pooled worker is configured for a
/// given call: `user_id` / `is_system_call` are the per-call identity, and
/// `permissions` is the addon's DECLARED set (from its manifest).
fn make_state(
    db: DbPool,
    addon_id: &str,
    declared: Vec<String>,
    user_id: Option<String>,
    is_system_call: bool,
) -> AddonState {
    let event_bus = Arc::new(EventBus::new());
    let permission_checker = Arc::new(PermissionChecker::new(db.clone()));
    let settings_cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32]));

    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "system-call-permission-instance".to_string(),
        user_id,
        org_id: None,
        db,
        permissions: declared,
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

#[test]
fn system_call_grants_declared_permission_without_user_grant() {
    let db = fresh_db();
    // Mirror the go2 addon: it DECLARES `sql.read` but no per-user grant exists.
    let state = make_state(
        db,
        "robot-go2",
        vec!["sql.read".to_string()],
        None,
        true,
    );
    assert!(
        check_permission(&state, "sql.read", None),
        "system call must grant a DECLARED permission via CR-006 (no user grant needed)"
    );
}

#[test]
fn system_call_does_not_grant_undeclared_permission() {
    let db = fresh_db();
    // Addon declares only `sql.read`; a system call must NOT reach `sql.write`.
    let state = make_state(
        db,
        "robot-go2",
        vec!["sql.read".to_string()],
        None,
        true,
    );
    assert!(
        !check_permission(&state, "sql.write", None),
        "system call must NOT grant a permission the addon never declared"
    );
}

#[test]
fn non_system_call_with_ungranted_user_is_rejected() {
    let db = fresh_db();
    // Same declared permission, but a real principal with NO per-user grant —
    // exactly the worker state that broke robot advertisement when the status
    // refresh was acquired as a fake "system" user instead of a system call.
    let state = make_state(
        db,
        "robot-go2",
        vec!["sql.read".to_string()],
        Some("user-without-grant".to_string()),
        false,
    );
    assert!(
        !check_permission(&state, "sql.read", None),
        "non-system call must still require a per-user grant (no blanket bypass)"
    );
}

#[test]
fn user_id_none_without_system_flag_is_rejected() {
    let db = fresh_db();
    // Defense-in-depth: missing principal alone must NOT grant — only the
    // explicit `is_system_call = true` flag opens the CR-006 path.
    let state = make_state(
        db,
        "robot-go2",
        vec!["sql.read".to_string()],
        None,
        false,
    );
    assert!(
        !check_permission(&state, "sql.read", None),
        "user_id=None without is_system_call must NOT grant (CR-006 invariant)"
    );
}
