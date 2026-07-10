// ============ tests/multi_tenant_org_isolation_full.rs — F2 P1.c host-fn sweep ============
//
// Validates the host-fn org filters added in P1.c. P1.b proved per-org install
// paths + audit row stamping; this file proves the SQL WHERE clauses landed
// on every read path that an addon can drive:
//
//   1. `list_cameras_for_addon(_, _, Some(org_a))` returns only org-A rows
//      even when org B holds rows for the same `addon_id` (cross-tenant
//      collision is impossible via the normal install path, but the SQL
//      filter is defense-in-depth and must hold regardless).
//   2. `get_camera_for_addon` for `(addon_id, camera_id)` returns None when
//      the row belongs to a different org — the addon caller sees NotFound
//      rather than a leaked camera row.
//   3. `NamespaceManager::get` keyed by `(org, addon, namespace)` returns
//      NamespaceNotFound when the namespace was created in another org
//      under the same `(addon_id, namespace)` pair.
//   4. `insert_camera` stamps the row with the supplied `org_id` (Some) or
//      falls back to `org-default` (None) — production stays compatible
//      with backfilled rows that pre-date P1.b.

use tentaflow_core::db::repository as repo;
use tentaflow_core::db::DbPool;
use tentaflow_core::services::org::DEFAULT_ORG_ID;
use tentaflow_core::services::vector::backend::Metric;
use tentaflow_core::services::vector::namespace::NamespaceManager;

fn open_pool() -> (tempfile::TempDir, DbPool) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("mt_full.db");
    let pool = tentaflow_core::db::init(&path).expect("db init");
    (dir, pool)
}

fn insert_camera_in_org(pool: &DbPool, addon_id: &str, camera_id: &str, org_id: &str) -> i64 {
    repo::insert_camera(
        pool,
        camera_id,
        addon_id,
        "Test camera",
        "rtsp",
        "rtsp://127.0.0.1/stream",
        30,
        10,
        Some(640),
        Some(480),
        "A",
        "h264",
        None,
        None,
        None,
        Some(org_id),
    )
    .expect("insert_camera ok")
}

#[test]
fn camera_list_returns_only_org_a_cameras_for_addon_in_org_a() {
    let (_d, pool) = open_pool();
    // Insert two rows for the same addon_id but in different orgs. This is
    // the worst-case scenario the SQL filter has to defend against.
    insert_camera_in_org(&pool, "vision-adr", "cam-a-1", "org-a");
    insert_camera_in_org(&pool, "vision-adr", "cam-a-2", "org-a");
    insert_camera_in_org(&pool, "vision-adr", "cam-b-1", "org-b");

    let rows_a =
        repo::list_cameras_for_addon(&pool, "vision-adr", Some("org-a")).expect("list rows org A");
    let ids_a: Vec<_> = rows_a.iter().map(|r| r.camera_id.as_str()).collect();
    assert!(ids_a.contains(&"cam-a-1"));
    assert!(ids_a.contains(&"cam-a-2"));
    assert!(
        !ids_a.contains(&"cam-b-1"),
        "org A list leaked org B row: {ids_a:?}"
    );

    let rows_b =
        repo::list_cameras_for_addon(&pool, "vision-adr", Some("org-b")).expect("list rows org B");
    let ids_b: Vec<_> = rows_b.iter().map(|r| r.camera_id.as_str()).collect();
    assert_eq!(ids_b, vec!["cam-b-1"]);
}

#[test]
fn camera_get_returns_none_for_cross_org_lookup() {
    let (_d, pool) = open_pool();
    insert_camera_in_org(&pool, "vision-adr", "cam-1", "org-a");

    // Caller in org B asking for the same (addon_id, camera_id) must get
    // None — the host fn maps None to AbiError::NotFound, NEVER to a leaked
    // CameraRow.
    let row_b =
        repo::get_camera_for_addon(&pool, "vision-adr", "cam-1", Some("org-b")).expect("get ok");
    assert!(row_b.is_none(), "cross-org get leaked the org A row");

    // Sanity: same call under org A returns the row.
    let row_a = repo::get_camera_for_addon(&pool, "vision-adr", "cam-1", Some("org-a"))
        .expect("get ok")
        .expect("row present");
    assert_eq!(row_a.camera_id, "cam-1");
}

#[test]
fn camera_insert_with_none_org_falls_back_to_default() {
    let (_d, pool) = open_pool();
    let rowid = repo::insert_camera(
        &pool,
        "cam-boot",
        "boot-addon",
        "Boot camera",
        "rtsp",
        "rtsp://127.0.0.1/x",
        15,
        10,
        None,
        None,
        "A",
        "h264",
        None,
        None,
        None,
        None,
    )
    .expect("insert ok");
    assert!(rowid > 0);

    let row = repo::get_camera_for_addon(&pool, "boot-addon", "cam-boot", Some(DEFAULT_ORG_ID))
        .expect("get ok")
        .expect("row present");
    assert_eq!(row.camera_id, "cam-boot");
}

#[test]
fn vector_namespace_get_returns_not_found_for_cross_org_lookup() {
    let (_d, pool) = open_pool();
    let dir = tempfile::TempDir::new().unwrap();
    let mgr = NamespaceManager::with_root(pool, dir.path().to_path_buf());

    mgr.get_or_create("org-a", "addon-rag", "docs", 8, Metric::Cosine, &[], false)
        .expect("create");
    // Org B asking for the same (addon, namespace) tuple must NOT see it —
    // the SQL filter on `addon_vector_namespaces.org_id` blocks the read.
    let res = mgr.get("org-b", "addon-rag", "docs");
    let is_not_found = matches!(
        res,
        Err(tentaflow_core::services::vector::error::VectorError::NamespaceNotFound { .. })
    );
    assert!(
        is_not_found,
        "cross-org vector ns lookup must surface NamespaceNotFound, got Ok or other error"
    );
}

// Helper: inserts one recording row for `(addon_id, ref, org_id)`. Keeps the
// recording tests below short and self-documenting.
fn insert_recording_in_org(
    pool: &DbPool,
    addon_id: &str,
    recording_ref: &str,
    camera_id: &str,
    org_id: &str,
) -> i64 {
    repo::insert_recording(
        pool,
        recording_ref,
        "snapshot",
        addon_id,
        camera_id,
        "/tmp/r.png",
        128,
        None,
        Some(16),
        Some(12),
        Some("rgb24"),
        "deadbeef",
        "B",
        Some(org_id),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("insert_recording ok")
}

#[test]
fn recording_get_returns_none_for_cross_org_lookup() {
    let (_d, pool) = open_pool();
    // Same addon_id present in both orgs (worst-case for the SQL filter).
    insert_recording_in_org(&pool, "vision-adr", "snap_a", "cam-1", "org-a");
    insert_recording_in_org(&pool, "vision-adr", "snap_b", "cam-1", "org-b");

    // Caller in org B asking for the org-A ref must get None — not the row.
    let row_b = repo::get_recording_for_addon(&pool, "vision-adr", "snap_a", Some("org-b"))
        .expect("get ok");
    assert!(row_b.is_none(), "cross-org get leaked the org-A recording");

    // Sanity: org-A caller sees its own row.
    let row_a = repo::get_recording_for_addon(&pool, "vision-adr", "snap_a", Some("org-a"))
        .expect("get ok")
        .expect("row present");
    assert_eq!(row_a.recording_ref, "snap_a");
}

#[test]
fn recording_purge_does_not_affect_other_org() {
    let (_d, pool) = open_pool();
    insert_recording_in_org(&pool, "vision-adr", "snap_a", "cam-1", "org-a");
    insert_recording_in_org(&pool, "vision-adr", "snap_b", "cam-1", "org-b");

    // Org-B tries to soft-delete a ref that only exists in org-A. Must be a
    // no-op (0 rows touched) — the WHERE clause filters on org_id.
    let touched = repo::soft_delete_recording(&pool, "vision-adr", "snap_a", Some("org-b"))
        .expect("delete ok");
    assert!(!touched, "cross-org purge must not touch the org-A row");

    // The org-A row is still live.
    let still = repo::get_recording_for_addon(&pool, "vision-adr", "snap_a", Some("org-a"))
        .expect("get ok");
    assert!(
        still.is_some(),
        "org-A row must survive the cross-org purge"
    );

    // The org-B row is also untouched (sanity for the test setup).
    let other = repo::get_recording_for_addon(&pool, "vision-adr", "snap_b", Some("org-b"))
        .expect("get ok");
    assert!(other.is_some());
}

#[test]
fn recording_stats_for_addon_are_org_scoped() {
    let (_d, pool) = open_pool();
    insert_recording_in_org(&pool, "vision-adr", "snap_a1", "cam-1", "org-a");
    insert_recording_in_org(&pool, "vision-adr", "snap_a2", "cam-1", "org-a");
    insert_recording_in_org(&pool, "vision-adr", "snap_b1", "cam-1", "org-b");

    let agg_a = repo::recording_stats_for_addon(&pool, "vision-adr", None, Some("org-a"))
        .expect("stats ok");
    assert_eq!(agg_a.total_snapshots, 2, "org-A must see only its own rows");

    let agg_b = repo::recording_stats_for_addon(&pool, "vision-adr", None, Some("org-b"))
        .expect("stats ok");
    assert_eq!(agg_b.total_snapshots, 1, "org-B must see only its own rows");
}

#[test]
fn camera_update_under_wrong_org_is_no_op() {
    let (_d, pool) = open_pool();
    // Row owned by org-A. An org-B caller addressing the same (addon, camera_id)
    // must NOT match — defends against any TOCTOU between a host-fn pre-check
    // and the UPDATE. (The active `camera_id` unique index prevents inserting
    // a parallel org-B row, so we assert the no-op shape on a single row.)
    insert_camera_in_org(&pool, "vision-adr", "cam-only-a", "org-a");

    let patch = repo::CameraPatch {
        display_name: Some("hijacked".into()),
        ..Default::default()
    };
    let touched_b = repo::update_camera(&pool, "vision-adr", "cam-only-a", &patch, Some("org-b"))
        .expect("update ok");
    assert!(!touched_b, "org-B must NOT be able to mutate an org-A row");

    let row_a = repo::get_camera_for_addon(&pool, "vision-adr", "cam-only-a", Some("org-a"))
        .expect("get ok")
        .expect("row present");
    assert_eq!(row_a.display_name, "Test camera");

    // Sanity: org-A still owns the row and CAN update it.
    let touched_a = repo::update_camera(&pool, "vision-adr", "cam-only-a", &patch, Some("org-a"))
        .expect("update ok");
    assert!(touched_a);
}

#[test]
fn camera_soft_delete_under_wrong_org_is_no_op() {
    let (_d, pool) = open_pool();
    insert_camera_in_org(&pool, "vision-adr", "cam-only-a-del", "org-a");

    let removed_b = repo::soft_delete_camera(&pool, "vision-adr", "cam-only-a-del", Some("org-b"))
        .expect("delete ok");
    assert!(
        !removed_b,
        "org-B must NOT be able to soft-delete an org-A row"
    );

    let row_a = repo::get_camera_for_addon(&pool, "vision-adr", "cam-only-a-del", Some("org-a"))
        .expect("get ok");
    assert!(
        row_a.is_some(),
        "org-A row survives the cross-org delete attempt"
    );
}

#[test]
fn audit_log_for_addon_in_org_a_carries_org_id_a() {
    // Already covered by tests/multi_tenant_isolation.rs in P1.b. We
    // re-assert the simpler shape here so a regression in the audit insert
    // is caught alongside the camera / vector regressions — a P1.c sweep
    // failure should not require running two test binaries to triage.
    use parking_lot::Mutex as PlMutex;
    use std::sync::Arc;
    use tentaflow_core::addon::event_bus::EventBus;
    use tentaflow_core::addon::host_functions::audit_log_with_risk;
    use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
    use tentaflow_core::addon::permissions::PermissionChecker;
    use tentaflow_core::addon::{AddonManifest, AddonState};
    use tentaflow_core::audit::RiskClass;

    let (_d, pool) = open_pool();
    let state = AddonState {
        addon_id: "audit-test".to_string(),
        instance_id: "i-9".to_string(),
        user_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
        org_id: Some("org-a".to_string()),
        db: pool.clone(),
        permissions: vec![],
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(pool.clone())),
        fuel_consumed: 0,
        is_system_call: false,
        rate_limiter: None,
        net_manager: Arc::new(PlMutex::new(NetworkConnectionManager::new())),
        settings_cipher: Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 16 * 1024 * 1024,
        router: None,
        oauth_refresh_guard: Arc::new(
            tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
        ),
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    };
    audit_log_with_risk(
        &state,
        "p1c.sweep",
        Some("res"),
        Some("audit-cross"),
        RiskClass::C,
        None,
        None,
        "ok",
        None,
    );
    let conn = pool.read().unwrap();
    let org_id: Option<String> = conn
        .query_row(
            "SELECT org_id FROM audit_log WHERE resource_id = 'audit-cross'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(org_id.as_deref(), Some("org-a"));
}
