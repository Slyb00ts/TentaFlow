// =============================================================================
// Plik: tests/native_app_lifecycle.rs
// Opis: Testy integracyjne cyklu życia aplikacji NATYWNYCH (app-platform):
//       rejestracja pakietu do katalogu, instalacja instancji (singleton,
//       seed addon_permission_defaults, hook init), odmowa update, uninstall
//       z hookiem teardown. Wymaga `native_apps::test_support` (tylko pod
//       `test-support` dla testow integracyjnych, patrz Cargo.toml
//       `required-features`). Uruchomienie:
//       cargo test --features test-support --test native_app_lifecycle
// =============================================================================

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use tentaflow_core::addon::{bundled, lifecycle};
use tentaflow_core::db;

/// One shared temp home per test process: TENTAFLOW_HOME and the package
/// store base are process-global (OnceLock), so every test funnels through
/// the same sandboxed directory tree.
fn test_home() -> &'static tempfile::TempDir {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        bundled::set_packages_base(tmp.path().join("data"));
        tmp
    })
}

fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;",
    )
    .expect("pragmas");
    db::migrations::run(&conn).expect("migrations");
    Arc::new(db::Db::from_connection(conn))
}

fn install_benchmark_instance(db: &db::DbPool, name: &str) -> anyhow::Result<String> {
    lifecycle::install_instance(db, "benchmark-studio", "1.0.0", name, &BTreeMap::new())
}

#[test]
fn native_package_registers_installs_and_uninstalls() {
    let _home = test_home();
    let db = create_test_db();

    // Boot reconcile puts the native packages into the catalog. A manifest
    // that fails to parse is skipped with an error log, so EVERY bundled
    // native app is asserted here — a broken manifest fails the test, not
    // just the boot log.
    bundled::install_native_packages(&db).expect("native package reconcile");
    for package_id in [
        "benchmark-studio",
        "ml-studio",
        "projekty",
        "code-studio",
        "meeting-bot",
        "tentabus",
    ] {
        let pkg = db::repository::get_addon_package(&db, package_id, "1.0.0")
            .expect("catalog query")
            .unwrap_or_else(|| panic!("{package_id} in catalog"));
        assert_eq!(pkg.source, "native");
    }

    // Install an instance from the catalog.
    let instance_id = install_benchmark_instance(&db, "Benchmark").expect("install instance");
    assert!(
        instance_id.starts_with("benchmark-studio-"),
        "instance id '{instance_id}' must be package-prefixed"
    );

    let row = db::repository::get_addon(&db, &instance_id)
        .expect("addons query")
        .expect("instance row");
    assert_eq!(row.package_id, "benchmark-studio");
    assert_eq!(row.runtime, "native");
    assert_eq!(row.wasm_size_bytes, 0);

    // Manifest `default = "allow"` seeded into addon_permission_defaults,
    // keyed by the INSTANCE id (the permission matrix is per instance).
    let read_default =
        db::repository::get_permission_default_grant_mode(&db, &instance_id, "benchmark.read")
            .expect("defaults query");
    assert_eq!(read_default.as_deref(), Some("allow"));
    let write_default =
        db::repository::get_permission_default_grant_mode(&db, &instance_id, "benchmark.write")
            .expect("defaults query");
    assert_eq!(write_default, None, "deny stays implicit — no row");

    // The local node recorded its reconcile outcome in the synced per-node
    // registry (no sync runtime in tests → the "local" fallback id).
    let statuses = db::repository::list_addon_config_prefixed(
        &db,
        &instance_id,
        tentaflow_core::addon::native_apps::NODE_STATUS_KEY_PREFIX,
    )
    .expect("node status query");
    assert_eq!(statuses.len(), 1, "one node, one status row");
    assert_eq!(statuses[0].0, "local");
    assert!(
        statuses[0].1.contains("\"status\":\"ready\""),
        "unexpected status payload: {}",
        statuses[0].1
    );

    // The init hook ran: the instance data dir exists.
    let data_dir = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &instance_id,
    )
    .expect("data dir");
    assert!(data_dir.exists());

    // Singleton: a second instance is refused.
    let err = install_benchmark_instance(&db, "Drugi").unwrap_err().to_string();
    assert!(err.contains("singleton"), "unexpected error: {err}");

    // Native apps have no separate update path — version follows core.
    let err = lifecycle::update_instance(&db, &instance_id, "1.0.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("core"), "unexpected error: {err}");

    // Uninstall runs the teardown hook and removes row + data dir.
    lifecycle::uninstall_instance(&instance_id, &db).expect("uninstall");
    assert!(db::repository::get_addon(&db, &instance_id)
        .expect("addons query")
        .is_none());
    assert!(!data_dir.exists(), "data dir must be gone after uninstall");

    // Singleton slot is free again after uninstall.
    let second = install_benchmark_instance(&db, "Benchmark 2").expect("reinstall after uninstall");
    lifecycle::uninstall_instance(&second, &db).expect("cleanup");
}

#[test]
fn seeded_defaults_grant_read_through_permission_checker() {
    let _home = test_home();
    let db = create_test_db();
    bundled::install_native_packages(&db).expect("native package reconcile");
    let instance_id = install_benchmark_instance(&db, "Benchmark").expect("install");

    let checker = tentaflow_core::addon::permissions::PermissionChecker::new(db.clone());
    checker.refresh_addon(&instance_id);

    // A plain user (no explicit grants, not admin): the seeded default
    // `benchmark.read = allow` grants, `benchmark.write` stays deny-by-default.
    assert!(checker
        .check(&instance_id, "user-plain", "benchmark.read", None)
        .is_granted());
    assert!(!checker
        .check(&instance_id, "user-plain", "benchmark.write", None)
        .is_granted());

    // Platform semantics: a freshly installed instance starts DISABLED (same
    // as WASM addons) — the admin enables it with the toggle. The gate must
    // see both states.
    let (found, enabled) = db::repository::get_package_instance(&db, "benchmark-studio")
        .expect("gate lookup")
        .expect("instance present");
    assert_eq!(found, instance_id);
    assert!(!enabled, "installed instance starts disabled until the admin toggle");
    db::repository::set_addon_enabled(&db, &instance_id, true).expect("enable");
    let (_, enabled) = db::repository::get_package_instance(&db, "benchmark-studio")
        .expect("gate lookup")
        .expect("instance present");
    assert!(enabled, "gate must see the enabled state after the toggle");

    lifecycle::uninstall_instance(&instance_id, &db).expect("cleanup");
    assert!(db::repository::get_package_instance(&db, "benchmark-studio")
        .expect("gate lookup")
        .is_none());
}

/// Registers the generic fixture package in the same catalog/version-store
/// shape `bundled::install_single_native_package` uses for real apps —
/// needed by the reconcile half of the test below, which (like
/// `AddonManager::reconcile_synced_addon` does for every native instance)
/// reads the package's `manifest.toml` off disk before touching `hooks_for`.
fn register_fixture_package(db: &db::DbPool, version: &str) {
    use tentaflow_core::addon::native_apps::test_support as fixture;

    let manifest_toml = fixture::fixture_manifest_toml(true);
    let dir = bundled::package_dir(fixture::PACKAGE_ID, version);
    std::fs::create_dir_all(&dir).expect("fixture package dir");
    std::fs::write(dir.join("manifest.toml"), &manifest_toml).expect("fixture manifest.toml");
    db::repository::upsert_addon_package(
        db,
        fixture::PACKAGE_ID,
        version,
        "Test Fixture App",
        &manifest_toml,
        "fixture-hash",
        "native",
    )
    .expect("fixture catalog upsert");
}

/// H1/H2 regression: drives the TWO real code paths that must reach a native
/// instance's `on_enable`/`on_disable` hooks, instead of calling
/// `notify_enabled` directly (which cannot catch either bug — it is the unit
/// under test, not the caller).
///
/// 1. The dashboard toggle handler (`handlers_addon_lifecycle::addon_toggle`)
///    — the local-node path, exercised on a freshly installed (disabled)
///    instance exactly as an admin would use it: enable, no-op re-enable,
///    disable.
/// 2. `AddonManager::reconcile_addon` (the `AddonSyncReconciler` entry point
///    `sync::runtime` calls after a replicated `addons` row commits) — the
///    fleet path, with the row's `is_enabled` flipped directly in the DB (as
///    a mesh-sync materializer would) rather than through this node's own
///    toggle. Before H1, the reconcile's `!addon.is_enabled` branch returned
///    before ever calling `notify_enabled(.., false)`, so a disable that
///    reached a node through sync never stopped the instance there.
// `AddonManager::new` spawns its permission-cache background refresh via
// `tokio::spawn` (`addon/permissions.rs::start_background_refresh`), so this
// test needs a live Tokio runtime even though every call in its body is sync.
#[tokio::test]
async fn addon_toggle_and_sync_reconcile_run_the_native_hooks_everywhere() {
    use std::sync::atomic::Ordering;
    use tentaflow_core::addon::native_apps::test_support as fixture;
    use tentaflow_core::addon::AddonManager;
    use tentaflow_core::api::dashboard::handlers_addon_lifecycle::addon_toggle;
    use tentaflow_core::dispatch::state::AppState;
    use tentaflow_core::dispatch::HandlerContext;
    use tentaflow_core::sync::runtime::AddonSyncReconciler;
    use tentaflow_protocol::{AddonToggleRequest, MessageBody, SessionAuth};

    let _home = test_home();
    let state = AppState::for_test();
    let db = state.db.clone();
    register_fixture_package(&db, "1.0.0-hooks");

    let addon_id = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-hooks",
        "Hooks Fixture",
        &BTreeMap::new(),
    )
    .expect("install fixture instance");

    let ctx = HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        org_context: None,
    };

    // --- 1. Dashboard toggle handler (local-node path) ---
    let enable_before = fixture::ENABLE_CALLS.load(Ordering::SeqCst);
    let disable_before = fixture::DISABLE_CALLS.load(Ordering::SeqCst);

    addon_toggle(
        &MessageBody::AddonToggleRequestBody(AddonToggleRequest {
            addon_id: addon_id.clone(),
            enabled: true,
        }),
        &ctx,
    )
    .expect("toggle on");
    assert_eq!(
        fixture::ENABLE_CALLS.load(Ordering::SeqCst),
        enable_before + 1,
        "addon_toggle must run on_enable when the flag actually flips"
    );

    // Toggling to the SAME value again must not re-run the hook.
    addon_toggle(
        &MessageBody::AddonToggleRequestBody(AddonToggleRequest {
            addon_id: addon_id.clone(),
            enabled: true,
        }),
        &ctx,
    )
    .expect("no-op toggle");
    assert_eq!(
        fixture::ENABLE_CALLS.load(Ordering::SeqCst),
        enable_before + 1,
        "a no-op toggle must not re-run on_enable"
    );

    addon_toggle(
        &MessageBody::AddonToggleRequestBody(AddonToggleRequest {
            addon_id: addon_id.clone(),
            enabled: false,
        }),
        &ctx,
    )
    .expect("toggle off");
    assert_eq!(
        fixture::DISABLE_CALLS.load(Ordering::SeqCst),
        disable_before + 1,
        "addon_toggle must run on_disable when the flag actually flips"
    );

    // --- 2. Sync reconcile (H1/H2): the row flips WITHOUT going through this
    //        node's own `addon_toggle` at all, exactly as a replicated
    //        enable/disable from another node would arrive. ---
    let mgr =
        AddonManager::new(db.clone(), state.settings_cipher.clone()).expect("AddonManager::new");

    db::repository::set_addon_enabled(&db, &addon_id, true).expect("enable row directly");
    let enable_before_reconcile = fixture::ENABLE_CALLS.load(Ordering::SeqCst);
    mgr.reconcile_addon(&addon_id);
    assert_eq!(
        fixture::ENABLE_CALLS.load(Ordering::SeqCst),
        enable_before_reconcile + 1,
        "H2: reconcile of a replicated enable must run on_enable on this node too"
    );

    db::repository::set_addon_enabled(&db, &addon_id, false).expect("disable row directly");
    let disable_before_reconcile = fixture::DISABLE_CALLS.load(Ordering::SeqCst);
    mgr.reconcile_addon(&addon_id);
    assert_eq!(
        fixture::DISABLE_CALLS.load(Ordering::SeqCst),
        disable_before_reconcile + 1,
        "H1: a disable that reached this node through sync must stop the \
         instance here too, not only where the admin clicked"
    );
}

/// W2: TentaBus is `[native] singleton = false` — the first shipped package
/// where a second install is not merely tolerated but the whole point.
/// Two installs must land as fully separate instances: distinct addon ids,
/// distinct on-disk data dirs, and — because `native_init` opens the content
/// database on install — each with its OWN `tentabus.db` FILE on disk, not
/// merely two `addons` rows.
#[test]
fn two_tentabus_instances_install_side_by_side() {
    let _home = test_home();
    let db = create_test_db();
    bundled::install_native_packages(&db).expect("native package reconcile");

    let addon_a = lifecycle::install_instance(&db, "tentabus", "1.0.0", "Bus A", &BTreeMap::new())
        .expect("install instance A");
    let addon_b = lifecycle::install_instance(&db, "tentabus", "1.0.0", "Bus B", &BTreeMap::new())
        .expect("install instance B");

    assert_ne!(addon_a, addon_b, "each install mints its own instance id");
    assert!(addon_a.starts_with("tentabus-"));
    assert!(addon_b.starts_with("tentabus-"));

    let row_a = db::repository::get_addon(&db, &addon_a)
        .expect("addons query")
        .expect("instance A row");
    assert_eq!(row_a.package_id, "tentabus");
    let row_b = db::repository::get_addon(&db, &addon_b)
        .expect("addons query")
        .expect("instance B row");
    assert_eq!(row_b.package_id, "tentabus");

    let dir_a = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &addon_a,
    )
    .expect("data dir A");
    let dir_b = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &addon_b,
    )
    .expect("data dir B");
    assert_ne!(dir_a, dir_b, "instances are isolated on disk");

    // native_init ran on install: each instance's own tentabus.db exists,
    // not just a shared row in the catalog.
    assert!(
        dir_a.join("tentabus.db").exists(),
        "instance A's own tentabus.db must exist"
    );
    assert!(
        dir_b.join("tentabus.db").exists(),
        "instance B's own tentabus.db must exist"
    );
    assert_ne!(
        dir_a.join("tentabus.db"),
        dir_b.join("tentabus.db"),
        "each instance owns a distinct db file path"
    );

    // Two files existing side by side does not prove two independent SQLite
    // POOLS — `app_db::open` caches by addon_id, so a bug that resolved both
    // instances to the same cached pool would still pass every assertion
    // above. Write through A's pool and confirm B's pool (opened separately,
    // same process-global registry) sees nothing.
    let pool_a = tentaflow_core::bus::native::open_db(
        &db,
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &addon_a,
    )
    .expect("open A's content db");
    let pool_b = tentaflow_core::bus::native::open_db(
        &db,
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &addon_b,
    )
    .expect("open B's content db");
    pool_a
        .write()
        .expect("A's writer")
        .execute(
            "INSERT INTO bus_groups \
             (org_id, group_id, topic, commit_mode, paused, created_at_ms, updated_at_ms) \
             VALUES ('default', 'g1', 'orders', 'auto', 0, 1, 1)",
            [],
        )
        .expect("insert bus_groups row through A's pool");
    let rows_in_a: i64 = pool_a
        .write()
        .expect("A's writer")
        .query_row("SELECT COUNT(*) FROM bus_groups", [], |r| r.get(0))
        .expect("count in A");
    assert_eq!(rows_in_a, 1, "A's own row must be visible through A's pool");
    let rows_in_b: i64 = pool_b
        .write()
        .expect("B's writer")
        .query_row("SELECT COUNT(*) FROM bus_groups", [], |r| r.get(0))
        .expect("count in B");
    assert_eq!(
        rows_in_b, 0,
        "A's bus_groups row must not leak into B's separate tentabus.db"
    );

    // Uninstall must be genuinely instance-scoped: tearing down A must not
    // touch B's data dir or B's content database.
    lifecycle::uninstall_instance(&addon_a, &db).expect("uninstall A");
    assert!(
        !dir_a.exists(),
        "A's data dir must be gone after uninstalling A"
    );
    assert!(
        dir_b.join("tentabus.db").exists(),
        "B's own tentabus.db must survive A's uninstall untouched"
    );

    lifecycle::uninstall_instance(&addon_b, &db).expect("uninstall B");
    assert!(
        !dir_b.exists(),
        "B's data dir must be gone after uninstalling B"
    );
}
