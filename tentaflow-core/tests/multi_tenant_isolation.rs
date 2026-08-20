// ============ tests/multi_tenant_isolation.rs — F2 P1.b integration tests ============
//
// Validates the org-scoped invariants introduced in P1.b:
//
//   1. Per-org filesystem sandbox: addon_data_dir(org_a, X) and
//      addon_data_dir(org_b, X) produce disjoint paths and disjoint SQLite
//      files. Two tenants installing the same addon_id keep separate data.
//   2. Audit row carries org_id: a synthetic AddonState pinned to org B
//      emits an audit row that reads back with org_id = org B.
//   3. RBAC isolation: a viewer membership in org A grants no permissions
//      in org B; PermissionMatrix surfaces NoMembership instead of a
//      silent allow.
//
// The wider org-scoped sweep (camera_list_v1 filtering, vector namespace
// per-org files, policy_claims by org) is wider than P1.b — those host fns
// land their per-org filters during P1.c CLI / dashboard work. The
// `audit_log_carries_org_id` test is the only end-to-end audit assertion
// here; the host-fn fan-out is covered by per-fn unit tests added in P1.c.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::fs_sandbox::addon_data_dir;
use tentaflow_core::addon::host_functions::audit_log_with_risk;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::storage_sql::{close_addon_db, open_addon_db};
use tentaflow_core::addon::{AddonCallProvenance, AddonManifest, AddonState};
use tentaflow_core::audit::RiskClass;
use tentaflow_core::db::DbPool;
use tentaflow_core::services::org::{repo as org_repo, DEFAULT_ORG_ID};
use tentaflow_core::services::rbac::{PermissionError, PermissionMatrix};

fn home_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

fn with_tmp_home<F: FnOnce()>(f: F) {
    let _guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());
    f();
    if let Some(p) = prev {
        std::env::set_var("HOME", p);
    } else {
        std::env::remove_var("HOME");
    }
}

fn open_pool() -> (tempfile::TempDir, DbPool) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("mt_iso.db");
    let pool = tentaflow_core::db::init(&path).expect("db init");
    (dir, pool)
}

fn create_two_orgs(pool: &DbPool) -> (String, String) {
    let a = org_repo::create_organization(pool, "Org A", "org-a", None, None, None, None).unwrap();
    let b = org_repo::create_organization(pool, "Org B", "org-b", None, None, None, None).unwrap();
    (a.org_id, b.org_id)
}

fn make_state(db: DbPool, addon_id: &str, org_id: Option<&str>) -> AddonState {
    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "i-1".to_string(),
        user_id: Some("00000000-0000-0000-0000-000000000042".to_string()),
        org_id: org_id.map(String::from),
        db: db.clone(),
        permissions: vec!["sql".to_string()],
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db.clone())),
        fuel_consumed: 0,
        is_system_call: false,
        call_provenance: AddonCallProvenance::addon(),
        rate_limiter: None,
        net_manager: Arc::new(PlMutex::new(NetworkConnectionManager::new())),
        settings_cipher: Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 64 * 1024 * 1024,
        router: None,
        oauth_refresh_guard: Arc::new(
            tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
        ),
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

// =============================================================================

#[test]
fn per_org_addon_data_dir_is_isolated() {
    with_tmp_home(|| {
        let dir_a = addon_data_dir("org-a", "shared-addon").expect("org-a path");
        let dir_b = addon_data_dir("org-b", "shared-addon").expect("org-b path");
        assert_ne!(
            dir_a, dir_b,
            "two tenants installing the same addon_id must not collide"
        );
        // Each path lives under its own org subtree.
        assert!(dir_a.to_string_lossy().contains("orgs/org-a/"));
        assert!(dir_b.to_string_lossy().contains("orgs/org-b/"));
    });
}

#[test]
fn per_org_sqlite_pools_back_distinct_files() {
    with_tmp_home(|| {
        let pool_a = open_addon_db("org-a", "shared-addon").expect("org-a pool");
        let pool_b = open_addon_db("org-b", "shared-addon").expect("org-b pool");

        // Write a sentinel through pool A.
        {
            let conn = pool_a.get().expect("a conn");
            conn.execute("CREATE TABLE marker (v TEXT)", []).unwrap();
            conn.execute("INSERT INTO marker(v) VALUES ('A')", [])
                .unwrap();
        }
        // Pool B is owned by a different org — same addon_id, different file.
        let b_has_table: i64 = {
            let conn = pool_b.get().expect("b conn");
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='marker'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            b_has_table, 0,
            "org B must not observe org A's per-addon table for the same addon_id"
        );

        close_addon_db("org-a", "shared-addon");
        close_addon_db("org-b", "shared-addon");
    });
}

#[test]
fn audit_log_carries_org_id_from_addon_state() {
    let (_d, pool) = open_pool();
    let (org_a, org_b) = create_two_orgs(&pool);

    let state_a = make_state(pool.clone(), "addon-x", Some(&org_a));
    let state_b = make_state(pool.clone(), "addon-x", Some(&org_b));

    audit_log_with_risk(
        &state_a,
        "test.action",
        Some("res"),
        Some("id-1"),
        RiskClass::C,
        None,
        None,
        "ok",
        None,
    );
    audit_log_with_risk(
        &state_b,
        "test.action",
        Some("res"),
        Some("id-2"),
        RiskClass::C,
        None,
        None,
        "ok",
        None,
    );

    let conn = pool.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT resource_id, org_id FROM audit_log WHERE action = 'test.action' ORDER BY id ASC")
        .unwrap();
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "expected two audit rows");
    let a_row = rows.iter().find(|(rid, _)| rid == "id-1").expect("row A");
    let b_row = rows.iter().find(|(rid, _)| rid == "id-2").expect("row B");
    assert_eq!(a_row.1.as_deref(), Some(org_a.as_str()));
    assert_eq!(b_row.1.as_deref(), Some(org_b.as_str()));
}

#[test]
fn audit_log_defaults_org_to_default_when_state_unset() {
    let (_d, pool) = open_pool();
    let state = make_state(pool.clone(), "addon-y", None);
    audit_log_with_risk(
        &state,
        "system.boot",
        None,
        Some("boot-1"),
        RiskClass::C,
        None,
        None,
        "ok",
        None,
    );
    let conn = pool.read().unwrap();
    let org_id: Option<String> = conn
        .query_row(
            "SELECT org_id FROM audit_log WHERE resource_id = 'boot-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(org_id.as_deref(), Some(DEFAULT_ORG_ID));
}

#[test]
fn rbac_membership_in_org_a_is_invisible_in_org_b() {
    let (_d, pool) = open_pool();
    let (org_a, org_b) = create_two_orgs(&pool);
    let admin_role = org_repo::list_roles(&pool)
        .unwrap()
        .into_iter()
        .find(|r| r.name == "org_admin")
        .unwrap();
    org_repo::add_membership(&pool, &org_a, "user-1", &admin_role.role_id, "boot").unwrap();

    let m = PermissionMatrix::new();
    // org A: admin grants org.admin.
    assert!(m
        .has_permission(&pool, "user-1", &org_a, "org.admin")
        .unwrap());
    // org B: no membership → NoMembership error, not a silent deny / allow.
    let err = m
        .has_permission(&pool, "user-1", &org_b, "org.admin")
        .unwrap_err();
    assert!(matches!(err, PermissionError::NoMembership(_, _)));
}

#[test]
fn rbac_invalidate_after_membership_remove() {
    let (_d, pool) = open_pool();
    let (org_a, _org_b) = create_two_orgs(&pool);
    let viewer = org_repo::list_roles(&pool)
        .unwrap()
        .into_iter()
        .find(|r| r.name == "org_viewer")
        .unwrap();
    org_repo::add_membership(&pool, &org_a, "user-2", &viewer.role_id, "boot").unwrap();

    let m = PermissionMatrix::global();
    assert!(m
        .has_permission(&pool, "user-2", &org_a, "org.read")
        .unwrap());

    // remove_membership invalidates the global cache so the next read sees
    // the new state. PermissionMatrix is process-wide so we use the same
    // singleton the repo writes against.
    org_repo::remove_membership(&pool, &org_a, "user-2").unwrap();
    let err = m
        .has_permission(&pool, "user-2", &org_a, "org.read")
        .unwrap_err();
    assert!(matches!(err, PermissionError::NoMembership(_, _)));
}

// Silence unused-import warning when wasi target excludes the wasmtime stack.
#[allow(dead_code)]
fn _phantom_path(p: &Path) -> &Path {
    p
}
