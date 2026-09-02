// =============================================================================
// Plik: tests/native_app_lifecycle.rs
// Opis: Testy integracyjne cyklu życia aplikacji NATYWNYCH (app-platform):
//       rejestracja pakietu do katalogu, instalacja instancji (singleton,
//       seed addon_permission_defaults, hook init), odmowa update, uninstall
//       z hookiem teardown. Uruchomienie: cargo test --test native_app_lifecycle
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
    for package_id in ["benchmark-studio", "ml-studio", "projekty"] {
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
