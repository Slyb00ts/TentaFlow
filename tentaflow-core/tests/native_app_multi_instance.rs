// =============================================================================
// Plik: tests/native_app_multi_instance.rs
// Opis: Testy integracyjne platformy dla natywnych aplikacji NON-SINGLETON
//       (plan `SUM/tentabus/PLAN-APP-PLATFORM.md` §2, W1.8). Uzywa generycznego
//       fixture pakietu (`native_apps::test_support`, `singleton = false`)
//       zamiast dotykac `bundled.rs` — katalog pakietow rejestrujemy wprost
//       przez `db::repository::upsert_addon_package` + zapis manifest.toml,
//       dokladnie to co `bundled::install_single_native_package` robi dla
//       pakietow wbudowanych. Uruchomienie:
//       cargo test --features test-support --test native_app_multi_instance
// =============================================================================

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use tentaflow_core::addon::native_apps::test_support as fixture;
use tentaflow_core::addon::{bundled, lifecycle};
use tentaflow_core::db;

/// One shared temp home per test process (same convention as
/// `tests/native_app_lifecycle.rs`): TENTAFLOW_HOME and the package store base
/// are process-global (OnceLock).
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

/// Registers one catalog entry for the generic test-fixture package
/// (`native_apps::test_support::PACKAGE_ID`) WITHOUT touching `bundled.rs`:
/// writes `manifest.toml` under the package store the same way
/// `bundled::install_single_native_package` does, then upserts the catalog
/// row directly. `version` lets independent tests register distinct rows
/// (e.g. one singleton, one not) without colliding with each other.
fn register_fixture_package(db: &db::DbPool, version: &str, singleton: bool) {
    let manifest_toml = fixture::fixture_manifest_toml(singleton);
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

/// Minimal content-db schema for the isolation test below — the fixture
/// manifest's `[native] db_file` gives every instance its own file; this is
/// the schema `app_db::open` runs on first open of either instance's file.
fn migrate_fixture_content(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rows (id INTEGER PRIMARY KEY, val TEXT NOT NULL);",
    )?;
    Ok(())
}

/// The platform's central multi-instance claim, proven at the content-
/// database level rather than only at the file-path level: two instances of
/// the SAME non-singleton package, each opened through `app_db::open`
/// (`native.db_file`), write into physically separate SQLite files. A row
/// written in A's database must be invisible from B's — not merely "a
/// different path exists", but "the data itself does not cross".
#[test]
fn two_instances_content_databases_are_isolated() {
    let _home = test_home();
    let db = create_test_db();
    register_fixture_package(&db, "1.0.0-isolation", false);

    let a = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-isolation",
        "Instance A",
        &BTreeMap::new(),
    )
    .expect("install instance A");
    let b = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-isolation",
        "Instance B",
        &BTreeMap::new(),
    )
    .expect("install instance B");

    let org = tentaflow_core::services::org::DEFAULT_ORG_ID;
    let pool_a = tentaflow_core::addon::app_db::open(&db, org, &a, migrate_fixture_content)
        .expect("open instance A content db");
    let pool_b = tentaflow_core::addon::app_db::open(&db, org, &b, migrate_fixture_content)
        .expect("open instance B content db");

    {
        let conn = pool_a.write().expect("write lock A");
        conn.execute("INSERT INTO rows (val) VALUES ('only-in-a')", [])
            .expect("insert into A");
    }

    let count_a: i64 = pool_a
        .read()
        .expect("read lock A")
        .query_row("SELECT COUNT(*) FROM rows", [], |r| r.get(0))
        .expect("count A");
    let count_b: i64 = pool_b
        .read()
        .expect("read lock B")
        .query_row("SELECT COUNT(*) FROM rows", [], |r| r.get(0))
        .expect("count B");
    assert_eq!(
        count_a, 1,
        "the write must land in instance A's own database"
    );
    assert_eq!(
        count_b, 0,
        "instance B's database must not see instance A's row — \
         `native.db_file` opens a separate SQLite file per instance"
    );

    tentaflow_core::addon::app_db::close(&a);
    tentaflow_core::addon::app_db::close(&b);
    lifecycle::uninstall_instance(&a, &db).expect("cleanup a");
    lifecycle::uninstall_instance(&b, &db).expect("cleanup b");
}

#[test]
fn non_singleton_package_installs_two_instances() {
    let _home = test_home();
    let db = create_test_db();
    register_fixture_package(&db, "1.0.0-multi", false);

    let a = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-multi",
        "Instance A",
        &BTreeMap::new(),
    )
    .expect("install instance A");
    let b = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-multi",
        "Instance B",
        &BTreeMap::new(),
    )
    .expect("install instance B");
    assert_ne!(
        a, b,
        "two instances of a non-singleton package must get distinct ids"
    );
    assert!(a.starts_with(fixture::PACKAGE_ID));
    assert!(b.starts_with(fixture::PACKAGE_ID));

    let row_a = db::repository::get_addon(&db, &a)
        .expect("addons query")
        .expect("row a present");
    let row_b = db::repository::get_addon(&db, &b)
        .expect("addons query")
        .expect("row b present");
    assert_eq!(row_a.package_id, fixture::PACKAGE_ID);
    assert_eq!(row_b.package_id, fixture::PACKAGE_ID);

    // Distinct, isolated data dirs — the platform's per-instance containment
    // must hold even though both instances share one package.
    let dir_a = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &a,
    )
    .expect("data dir a");
    let dir_b = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &b,
    )
    .expect("data dir b");
    assert_ne!(dir_a, dir_b);
    assert!(dir_a.exists());
    assert!(dir_b.exists());

    let instances =
        db::repository::list_package_instances(&db, fixture::PACKAGE_ID).expect("list instances");
    let ids: HashSet<_> = instances.iter().map(|(id, _, _)| id.clone()).collect();
    assert!(ids.contains(&a));
    assert!(ids.contains(&b));
    assert_eq!(
        instances.len(),
        2,
        "unrelated fixture rows must not leak in"
    );

    lifecycle::uninstall_instance(&a, &db).expect("cleanup a");
    lifecycle::uninstall_instance(&b, &db).expect("cleanup b");
}

#[test]
fn singleton_package_still_refuses_a_second_instance() {
    let _home = test_home();
    let db = create_test_db();
    register_fixture_package(&db, "1.0.0-singleton", true);

    let first = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-singleton",
        "First",
        &BTreeMap::new(),
    )
    .expect("first install");
    let err = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-singleton",
        "Second",
        &BTreeMap::new(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("singleton"), "unexpected error: {err}");

    lifecycle::uninstall_instance(&first, &db).expect("cleanup");
}

#[test]
fn get_instance_of_package_rejects_a_foreign_instance_id() {
    let _home = test_home();
    let db = create_test_db();
    register_fixture_package(&db, "1.0.0-cross", false);
    bundled::install_native_packages(&db).expect("native package reconcile");

    let own = lifecycle::install_instance(
        &db,
        fixture::PACKAGE_ID,
        "1.0.0-cross",
        "Own",
        &BTreeMap::new(),
    )
    .expect("install own instance");
    let foreign = lifecycle::install_instance(
        &db,
        "benchmark-studio",
        "1.0.0",
        "Foreign",
        &BTreeMap::new(),
    )
    .expect("install foreign instance");

    assert_eq!(
        db::repository::get_instance_of_package(&db, fixture::PACKAGE_ID, &foreign).expect("query"),
        None,
        "an instance of another package must not resolve"
    );
    let (found, enabled) = db::repository::get_instance_of_package(&db, fixture::PACKAGE_ID, &own)
        .expect("query")
        .expect("own instance found");
    assert_eq!(found, own);
    assert!(!enabled, "freshly installed instance starts disabled");

    lifecycle::uninstall_instance(&own, &db).expect("cleanup own");
    lifecycle::uninstall_instance(&foreign, &db).expect("cleanup foreign");
}
