// =============================================================================
// File: tests/tentavm_lifecycle.rs
// Purpose: Lifecycle of TentaVM as a MULTI-INSTANCE app (plan §15, "multi-instance
//          lifecycle test"): two environments install side by side, each gets its
//          own `tentavm.db`, and the host row is one per node, shared by both.
//          This file runs in its own test process because it initializes the
//          global sync runtime — that is what gives the node an identity, and
//          without one `native_init` deliberately publishes no host.
//          Run: cargo test --test tentavm_lifecycle
// =============================================================================

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use tentaflow_core::addon::{bundled, lifecycle, native_apps};
use tentaflow_core::db;

const PACKAGE: &str = tentaflow_core::tentavm::PACKAGE_ID;
const VERSION: &str = "1.0.0";

/// One shared temp home per test process: TENTAFLOW_HOME, the package store
/// base and the sync ledger directory are process-global.
fn test_home() -> &'static tempfile::TempDir {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        bundled::set_packages_base(tmp.path().join("data"));
        tmp
    })
}

/// The database, the mesh identity and the sync runtime, in that order — the
/// same order `tentaflow/src/main.rs` boots them in. The runtime is what makes
/// `sync::runtime::local_node_id()` answer, so the node id below is a real
/// Ed25519 key, not a placeholder.
fn booted_node() -> (db::DbPool, String) {
    static NODE: OnceLock<(db::DbPool, String)> = OnceLock::new();
    NODE.get_or_init(|| {
        test_home();
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .expect("pragmas");
        db::migrations::run(&conn).expect("migrations");
        let pool: db::DbPool = Arc::new(db::Db::from_connection(conn));

        let cipher = Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[7u8; 32]));
        let security = Arc::new(
            tentaflow_core::mesh::security::MeshSecurity::new(pool.clone(), cipher.clone())
                .expect("mesh identity"),
        );
        let node_id = security.ed25519_public_key_hex();
        tentaflow_core::sync::runtime::init(pool.clone(), security, cipher).expect("sync runtime");
        (pool, node_id)
    })
    .clone()
}

/// TentaVM is deliberately NOT in `NATIVE_APP_PACKAGES` yet (the tile needs the
/// UI shell), so the catalog row is written here the way `install_native_packages`
/// writes it: manifest on disk plus the `addon_packages` row.
fn register_package(db: &db::DbPool) {
    let manifest = tentaflow_core::tentavm::APP_MANIFEST;
    let dir = bundled::package_dir(PACKAGE, VERSION);
    std::fs::create_dir_all(&dir).expect("package dir");
    std::fs::write(dir.join("manifest.toml"), manifest).expect("manifest.toml");
    db::repository::upsert_addon_package(
        db,
        PACKAGE,
        VERSION,
        "TentaVM",
        manifest,
        "test-bundle-hash",
        "native",
    )
    .expect("catalog row");
}

fn instance_db_path(addon_id: &str) -> std::path::PathBuf {
    tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        addon_id,
    )
    .expect("data dir")
    .join("tentavm.db")
}

fn host_rows(db: &db::DbPool) -> Vec<(String, String, String, String)> {
    let conn = db.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT id, node_id, status, owner_node_id FROM vm_hosts ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

/// Rows of the core capture journal for the replicated host resource, newest
/// last: `(status, resource_id, operation_id)`.
///
/// The journal is the FIRST half of "the row leaves this node". The second half
/// is the drain, which mints the ledger operation and puts it in the outbox —
/// see `drain_host_captures`.
fn host_captures(db: &db::DbPool) -> Vec<(String, String, Option<String>)> {
    let conn = db.read().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT status, resource_id, operation_id FROM __tentaflow_core_sync_captures \
             WHERE resource_type = 'core.vm_host' ORDER BY created_at_ms ASC, capture_id ASC",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

/// Drains the pending core captures the way the sync scheduler tick does, and
/// reports how many outbox targets each minted operation reached.
///
/// `drain_pending_core_captures_with` IS the production drain — the online
/// wrapper `sync::runtime::drain_pending_core_captures_online` is one line over
/// it, and both record through `sync::runtime::record_core_capture`. It is used
/// here in the explicit form only because the wrapper throws away the number of
/// targets, which is the thing this test is about.
fn drain_host_captures(db: &db::DbPool) -> Vec<usize> {
    let mut queued = Vec::new();
    tentaflow_core::sync::core_capture::drain_pending_core_captures_with(db, usize::MAX, |capture| {
        let is_host = capture.resource_type == "core.vm_host";
        let record = tentaflow_core::sync::runtime::record_core_capture(capture)?;
        Ok(record.map(|record| {
            if is_host {
                queued.push(record.queued_targets);
            }
            record.op_id
        }))
    })
    .expect("drain");
    queued
}

/// The whole point of `singleton = false`: two environments coexist. Each gets
/// its own instance row, its own data dir and its own `tentavm.db` (the init
/// hook creates and migrates it), while the host row is org-scoped and shared —
/// plan §2, "Hosty są wspólne dla organizacji".
#[test]
fn two_environments_install_side_by_side_with_separate_databases() {
    let (db, node_id) = booted_node();
    register_package(&db);

    let first = lifecycle::install_instance(&db, PACKAGE, VERSION, "Domyślne", &BTreeMap::new())
        .expect("first environment");
    let second = lifecycle::install_instance(&db, PACKAGE, VERSION, "Laboratorium", &BTreeMap::new())
        .expect("second environment — singleton = false");
    assert_ne!(first, second);
    assert!(first.starts_with("tentavm-") && second.starts_with("tentavm-"));

    // `native_init` ran for both: two files, two schemas, no sharing.
    for instance in [&first, &second] {
        let path = instance_db_path(instance);
        assert!(path.exists(), "{instance}: tentavm.db must exist after init");
        let conn = rusqlite::Connection::open(&path).expect("open instance db");
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'vm_%'",
                [],
                |r| r.get(0),
            )
            .expect("schema query");
        assert_eq!(tables, 9, "{instance}: the content schema must be applied");
    }
    assert_ne!(instance_db_path(&first), instance_db_path(&second));

    // One node, one host row, carrying the REAL mesh identity — no placeholder
    // id ever reaches the replicated registry.
    let hosts = host_rows(&db);
    assert_eq!(hosts.len(), 1, "hosts are shared by every environment");
    assert_eq!(hosts[0].0, node_id);
    assert_eq!(hosts[0].1, node_id);
    assert_eq!(hosts[0].2, "needs_install");
    assert_eq!(hosts[0].3, node_id);
    assert_eq!(node_id.len(), 64, "the host id is the Ed25519 node id");

    // The registry is REPLICATED, so writing the row locally is only half of
    // what `init` owes the fleet. This is the other half, and it goes the whole
    // production way: a real `install_instance` ran the real `native_init`,
    // which ran the real `ensure_local_host`, and what is checked here is what
    // that write left behind — the capture journal, then the drain that mints
    // the signed operation and puts it in the outbox of a real peer.
    //
    // Calling the capture function directly would have proved nothing. That is
    // the trap steps 3, 8 and 14 each paid for: a mechanism that exists,
    // answers every test that calls it, and is called by nothing on the path
    // the product actually takes.
    {
        let captures = host_captures(&db);
        assert_eq!(
            captures.len(),
            1,
            "one host row written by init, one capture — and the second \
             environment's init changed nothing, so it added none: {captures:?}"
        );
        assert_eq!(captures[0].0, "pending");
        assert_eq!(captures[0].1, node_id, "the capture is keyed by the host id");

        // A peer to send it to. Without a trusted node and the default core
        // policies the operation is minted and then resolves to zero targets,
        // which is exactly the silent half-failure this asserts against.
        db::repository::ensure_default_core_sync_policies(&db).expect("core policies");
        let peer = "b".repeat(64);
        db::repository::upsert_sync_node_identity(
            &db, &peer, "pk", "ed25519", "Peer", "server", "trusted", None, "standard",
        )
        .expect("trusted peer");

        let queued = drain_host_captures(&db);
        assert_eq!(queued, vec![1], "the host row must reach the peer's outbox");
        let captures = host_captures(&db);
        assert_eq!(captures[0].0, "ledgered", "the capture must become an operation");
        assert!(
            captures[0].2.as_deref().is_some_and(|op| !op.is_empty()),
            "a ledgered capture names the operation it became: {captures:?}"
        );
    }

    // A boot that changes nothing must not write to the registry.
    //
    // This became a real invariant with step 7: `vm_hosts` is a replicated
    // resource now, so an unguarded upsert would mint a row change on every
    // reconcile of every environment on every node, and the mesh would carry a
    // stream of "updates" nobody made. The only thing standing between that and
    // a quiet boot is the WHERE clause of the upsert in `ensure_local_host`.
    //
    // A sentinel is the pin, not a timestamp comparison: `updated_at` has second
    // resolution, so a write inside the same second would be invisible.
    {
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE vm_hosts SET updated_at = 'boot-witness' WHERE id = ?1",
                rusqlite::params![node_id],
            )
            .expect("sentinel");
        }
        let hooks = native_apps::hooks_for(PACKAGE).expect("hooks");
        let data_dir = instance_db_path(&first)
            .parent()
            .expect("data dir")
            .to_path_buf();
        (hooks.init)(&native_apps::NativeAppContext {
            db: &db,
            addon_id: &first,
            org_id: tentaflow_core::services::org::DEFAULT_ORG_ID,
            data_dir,
        })
        .expect("a second init changes nothing");
        let witness: String = {
            let conn = db.read().unwrap();
            conn.query_row(
                "SELECT updated_at FROM vm_hosts WHERE id = ?1",
                rusqlite::params![node_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            witness, "boot-witness",
            "a boot that changes nothing must not touch the replicated host row"
        );
        assert_eq!(host_rows(&db).len(), 1, "and must not add a second one");
        // …and mints nothing for the mesh to carry. One condition guards both:
        // the capture is written only when the upsert reported a changed row.
        assert!(
            host_captures(&db).iter().all(|(status, _, _)| status == "ledgered"),
            "a no-op boot minted a capture: {:?}",
            host_captures(&db)
        );

        // Positive control: the sentinel has to be able to detect a write, or
        // the assertion above would hold for a guard that was never there. A
        // boot that DOES change something — the display name the card shows —
        // must overwrite it.
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE vm_hosts SET display_name = 'stale name' WHERE id = ?1",
                rusqlite::params![node_id],
            )
            .expect("make the boot a real change");
        }
        let hooks = native_apps::hooks_for(PACKAGE).expect("hooks");
        let data_dir = instance_db_path(&first)
            .parent()
            .expect("data dir")
            .to_path_buf();
        (hooks.init)(&native_apps::NativeAppContext {
            db: &db,
            addon_id: &first,
            org_id: tentaflow_core::services::org::DEFAULT_ORG_ID,
            data_dir,
        })
        .expect("a boot that changes the name writes");
        let after: String = {
            let conn = db.read().unwrap();
            conn.query_row(
                "SELECT updated_at FROM vm_hosts WHERE id = ?1",
                rusqlite::params![node_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_ne!(
            after, "boot-witness",
            "the sentinel cannot tell a write from a no-op, so the assertion above proves nothing"
        );
        // The control is double: a boot that DOES change something both writes
        // the row and mints the capture, and this one goes out through the
        // production wrapper the sync scheduler calls.
        let pending: Vec<_> = host_captures(&db)
            .into_iter()
            .filter(|(status, _, _)| status == "pending")
            .collect();
        assert_eq!(pending.len(), 1, "a real change mints exactly one capture");
        assert_eq!(
            tentaflow_core::sync::runtime::drain_pending_core_captures_online(usize::MAX)
                .expect("online drain"),
            Some(1),
            "the scheduler's own drain must carry it"
        );
        assert!(
            host_captures(&db).iter().all(|(status, _, _)| status == "ledgered"),
            "nothing may be left pending after the drain: {:?}",
            host_captures(&db)
        );
    }

    // Both environments recorded their own reconcile outcome for this node.
    for instance in [&first, &second] {
        let statuses = db::repository::list_addon_config_prefixed(
            &db,
            instance,
            native_apps::NODE_STATUS_KEY_PREFIX,
        )
        .expect("node status");
        assert_eq!(statuses.len(), 1, "{instance}: one node, one status row");
        assert_eq!(statuses[0].0, node_id);
        assert!(statuses[0].1.contains("\"status\":\"ready\""));
    }

    // …and the PLATFORM wrote it, not the hook. Counting rows cannot show that:
    // `record_node_status` upserts one key, so a hook writing it too leaves
    // exactly one row with the same value the platform then overwrites. Run the
    // hook alone, with no platform call around it, on a cleared key — anything
    // that appears was written by the hook. On a node with no mesh identity that
    // write is worse than redundant: `record_node_status` falls back to the
    // literal `local` and publishes `__node_status/local = ready` for an
    // environment with zero host rows.
    {
        {
            let conn = db.write().unwrap();
            conn.execute(
                "DELETE FROM addon_config WHERE addon_id = ?1 AND key LIKE ?2",
                rusqlite::params![&first, format!("{}%", native_apps::NODE_STATUS_KEY_PREFIX)],
            )
            .expect("clear the platform's status");
        }
        let hooks = native_apps::hooks_for(PACKAGE).expect("hooks");
        let data_dir = instance_db_path(&first)
            .parent()
            .expect("data dir")
            .to_path_buf();
        (hooks.init)(&native_apps::NativeAppContext {
            db: &db,
            addon_id: &first,
            org_id: tentaflow_core::services::org::DEFAULT_ORG_ID,
            data_dir,
        })
        .expect("init alone");
        let written =
            db::repository::list_addon_config_prefixed(&db, &first, native_apps::NODE_STATUS_KEY_PREFIX)
                .expect("node status");
        assert!(
            written.is_empty(),
            "the platform owns the node status; the init hook must not write one: {written:?}"
        );
    }

    // The teardown plan is honest about what a wipe leaves behind, and the
    // uninstall proves it: the registry rows of the environment stay.
    {
        let conn = db.write().unwrap();
        conn.execute(
            "INSERT INTO vm_instance_settings (instance_id, key, org_id, value, created_at, \
                 updated_at, updated_by_node) VALUES (?1, 'visibility', 'default', 'all', 'x', 'x', ?2)",
            rusqlite::params![first, node_id],
        )
        .expect("seed an environment setting");
    }
    let data_dir = tentaflow_core::addon::fs_sandbox::addon_data_dir(
        tentaflow_core::services::org::DEFAULT_ORG_ID,
        &first,
    )
    .expect("data dir");
    let plan = (native_apps::hooks_for(PACKAGE).expect("hooks").teardown_plan)(
        &native_apps::NativeAppContext {
            db: &db,
            addon_id: &first,
            org_id: tentaflow_core::services::org::DEFAULT_ORG_ID,
            data_dir: data_dir.clone(),
        },
    )
    .expect("teardown plan");
    let registry_row = plan
        .iter()
        .find(|e| e.kind == "tentavm_registry_rows")
        .expect("the plan must name the rows that stay");
    assert!(!registry_row.removed);

    lifecycle::uninstall_instance(&first, &db).expect("uninstall the first environment");
    assert!(!data_dir.exists(), "the data dir goes with the instance");
    let left_behind: i64 = {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM vm_instance_settings WHERE instance_id = ?1",
            rusqlite::params![first],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        left_behind, 1,
        "registry rows survive the uninstall — exactly what the plan promised"
    );

    // The second environment is untouched by the first one's uninstall.
    assert!(instance_db_path(&second).exists());
    assert_eq!(host_rows(&db).len(), 1, "the host row is not an instance row");
}
