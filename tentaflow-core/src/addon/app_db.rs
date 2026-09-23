// =============================================================================
// File: addon/app_db.rs — per-INSTANCE content database of a native app
//       (plan-01 §6). The main `tentaflow.db` is the platform layer (packages,
//       instances, permissions, app registries the sync engine reads); an
//       app's local content (benchmark runs, ML artifacts, workspace state)
//       lives in `<instance data dir>/<native.db_file>` — never synced, no
//       foreign keys outside itself, wiped with the instance on uninstall.
//
//       One registry for every native app: the file name comes from the
//       instance manifest (single source of truth), the app supplies only its
//       schema migration. Handlers reach the pool through the instance id the
//       app gate already resolved, so there is no second lookup path to drift.
// =============================================================================

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use rusqlite::Connection;
use tracing::{info, warn};

use crate::db::DbPool;

/// Brings the app's schema up to date on a freshly opened connection. Must be
/// idempotent — it runs on every process-lifetime first open, not once per
/// install (`native_init` reconcile re-runs it too).
pub type Migrate = fn(&Connection) -> Result<()>;

fn registry() -> &'static DashMap<String, DbPool> {
    static REG: OnceLock<DashMap<String, DbPool>> = OnceLock::new();
    REG.get_or_init(DashMap::new)
}

/// Pool of the instance's content database, opening it on first use. The
/// file name is `[native] db_file` of the instance manifest stored in the
/// `addons` row; an instance whose manifest declares no `db_file` is an error
/// here — the app has no content database by its own declaration.
///
/// The directory is `data_dir_org` of the manifest and `org_id`: for a
/// `singleton = true` package it is the one every lifecycle hook uses, not
/// the requesting organisation's (see there).
pub fn open(main_db: &DbPool, org_id: &str, addon_id: &str, migrate: Migrate) -> Result<DbPool> {
    if let Some(pool) = registry().get(addon_id) {
        return Ok(pool.clone());
    }
    let row = crate::db::repository::get_addon(main_db, addon_id)?
        .ok_or_else(|| anyhow!("app instance '{addon_id}' is not installed"))?;
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&row.manifest_json)?;
    let native = manifest
        .native
        .as_ref()
        .ok_or_else(|| anyhow!("app instance '{addon_id}' declares no native.db_file"))?;
    let db_file = native
        .db_file
        .as_deref()
        .ok_or_else(|| anyhow!("app instance '{addon_id}' declares no native.db_file"))?;
    let dir = crate::addon::fs_sandbox::addon_data_dir(data_dir_org(native, org_id), addon_id)
        .map_err(|e| anyhow!("instance data dir for '{addon_id}': {e:?}"))?;
    open_at(addon_id, &dir.join(db_file), migrate)
}

/// The organisation whose data directory holds an instance's content
/// database.
///
/// A `singleton = true` package has ONE instance per node shared by every
/// organisation on it (TentaNas: one `tentanas.db` per node), and the pool is
/// registered under the instance id alone. Every lifecycle path — install,
/// `native_init` at boot and on reconcile, enable/disable, teardown and
/// uninstall — resolves its data dir under `DEFAULT_ORG_ID`. A request path
/// passes the ASKING organisation instead, so whenever the pool was not yet
/// registered (a failed `native_init`) the first tenant to send a request
/// decided which file became the node's database: a fresh, empty one under
/// its own org directory, which the uninstall would then never remove.
/// Resolving a singleton under `DEFAULT_ORG_ID` whoever asks makes the file
/// the one the lifecycle created (on rig11
/// `orgs/org-default/addons/tentanas-8dd19dc4/tentanas.db`, unchanged).
///
/// A multi-instance package keeps the caller's organisation: that is its
/// behaviour today, and nothing here has measured it being wrong for one.
fn data_dir_org<'a>(native: &crate::addon::AddonNativeSection, requesting_org: &'a str) -> &'a str {
    if native.singleton {
        crate::services::org::DEFAULT_ORG_ID
    } else {
        requesting_org
    }
}

/// Pool for the (single enabled) instance of `package_id`. For code paths
/// that did not go through `app_gate::require_app_permission` — background
/// jobs, lifecycle hooks of other apps — and therefore hold no instance id.
///
/// Routes through `app_gate::sole_enabled_instance`: on a `singleton = false`
/// package with zero or more than one enabled instance, this fails loudly
/// instead of silently picking one (the previous `get_package_instance`
/// LIMIT-1 behaviour, which also ignored `is_enabled` entirely). This DOES
/// change behaviour for every existing (singleton) caller: a disabled
/// instance used to still open its content database here; now it returns an
/// error naming the app as disabled instead. That tightening is intentional
/// — disabling an app means stopping it, and a background job quietly
/// reading its content database while it is "off" is exactly the kind of
/// access the disable flag exists to prevent. A disabled instance is
/// reported distinctly from a never-installed one (`SoleInstanceError::
/// Disabled` vs. `::None`), so callers do not lose that information.
pub fn open_for_package(
    main_db: &DbPool,
    org_id: &str,
    package_id: &str,
    migrate: Migrate,
) -> Result<(String, DbPool)> {
    let addon_id = crate::dispatch::app_gate::sole_enabled_instance(main_db, package_id).map_err(
        |e| match e {
            crate::dispatch::app_gate::SoleInstanceError::None => {
                anyhow!("application '{package_id}' is not installed")
            }
            crate::dispatch::app_gate::SoleInstanceError::Disabled => {
                anyhow!("application '{package_id}' is installed but disabled")
            }
            crate::dispatch::app_gate::SoleInstanceError::Ambiguous(count) => anyhow!(
                "application '{package_id}' has {count} enabled instances; \
                 open_for_package cannot pick one"
            ),
            crate::dispatch::app_gate::SoleInstanceError::Lookup => {
                anyhow!("application '{package_id}' instance lookup failed")
            }
        },
    )?;
    let pool = open(main_db, org_id, &addon_id, migrate)?;
    Ok((addon_id, pool))
}

/// Opens (creating if absent) the database at `path` with the core PRAGMAs,
/// runs `migrate` and registers the pool under `addon_id`. Same contract as
/// `db::init` for the main file: WAL, a writer plus a read pool so reads never
/// queue behind writes.
fn open_at(addon_id: &str, path: &Path, migrate: Migrate) -> Result<DbPool> {
    info!("native app '{addon_id}': opening content database {}", path.display());
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA mmap_size=268435456;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA busy_timeout=5000;\
         PRAGMA wal_autocheckpoint=2000;",
    )?;
    migrate(&conn)?;
    let pool: DbPool = Arc::new(crate::db::Db::with_read_pool(conn, path)?);
    // A concurrent first open of the same instance keeps whichever pool won
    // the race; the loser's connection is dropped with its `Arc`.
    let entry = registry()
        .entry(addon_id.to_string())
        .or_insert_with(|| pool.clone());
    Ok(entry.clone())
}

/// Checkpoints and drops the pool for `addon_id` so the file can be removed.
/// Called by instance uninstall BEFORE the data dir is deleted; a no-op for
/// instances that were never opened.
pub fn close(addon_id: &str) {
    if let Some((_, pool)) = registry().remove(addon_id) {
        checkpoint(addon_id, &pool);
    }
}

/// Shutdown hook: checkpoints every open content database so a kill does not
/// leave unflushed `-wal` files behind.
pub fn checkpoint_all() {
    for item in registry().iter() {
        checkpoint(item.key(), item.value());
    }
}

fn checkpoint(addon_id: &str, pool: &DbPool) {
    match pool.write() {
        Ok(conn) => {
            if let Err(e) = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE") {
                warn!("native app '{addon_id}': content db WAL checkpoint failed: {e}");
            }
        }
        Err(e) => warn!("native app '{addon_id}': content db checkpoint lock failed: {e}"),
    }
}

/// Versioned migration runner shared by every native app: tracks applied
/// versions in `app_schema_version` and applies each pending `(version, sql)`
/// step in its own transaction. Apps declare their steps as a static slice
/// and call this from their `Migrate` fn.
pub fn run_versioned_migrations(conn: &Connection, app: &str, steps: &[(i64, &str)]) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM app_schema_version",
        [],
        |row| row.get(0),
    )?;
    for (version, sql) in steps {
        if *version > current {
            info!("native app '{app}': content db migration {version}");
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO app_schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrate(conn: &Connection) -> Result<()> {
        run_versioned_migrations(
            conn,
            "test-app",
            &[
                (1, "CREATE TABLE things (id INTEGER PRIMARY KEY, name TEXT NOT NULL);"),
                (2, "ALTER TABLE things ADD COLUMN note TEXT;"),
            ],
        )
    }

    #[test]
    fn versioned_migrations_apply_once_and_in_order() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 2);
        conn.execute("INSERT INTO things (name, note) VALUES ('a', 'b')", [])
            .unwrap();
    }

    fn native_section(singleton: bool) -> crate::addon::AddonNativeSection {
        crate::addon::lifecycle::parse_manifest_toml(
            &crate::addon::native_apps::test_support::fixture_manifest_toml(singleton),
        )
        .expect("fixture manifest")
        .native
        .expect("[native] section")
    }

    #[test]
    fn a_singleton_database_lives_where_the_lifecycle_put_it_whoever_asks() {
        let singleton = native_section(true);
        for org in ["org-a", "org-b", crate::services::org::DEFAULT_ORG_ID] {
            assert_eq!(data_dir_org(&singleton, org), crate::services::org::DEFAULT_ORG_ID, "{org}");
        }
        // A multi-instance package is not changed by this rule.
        assert_eq!(data_dir_org(&native_section(false), "org-b"), "org-b");
    }

    /// Through `open` itself, with the pool NOT yet registered — the state a
    /// failed `native_init` leaves: org B asks first, and the file it gets is
    /// the lifecycle's one under `DEFAULT_ORG_ID`, the one org A then reads.
    #[test]
    fn a_singleton_opened_first_by_another_org_is_still_the_node_database() {
        crate::addon::fs_sandbox::with_tmp_home(|| {
            let conn = Connection::open_in_memory().unwrap();
            crate::db::migrations::run(&conn).unwrap();
            let main: DbPool = Arc::new(crate::db::Db::from_connection(conn));
            let addon_id = crate::addon::fs_sandbox::unique_test_addon_id("singleton-app");
            main.write()
                .unwrap()
                .execute(
                    "INSERT INTO addons (addon_id, name, version, package_id, package_version, \
                     runtime, is_enabled, manifest_json) \
                     VALUES (?1, 'Fixture', '1.0.0', 'test-hook-app', '1.0.0', 'native', 1, ?2)",
                    rusqlite::params![
                        addon_id,
                        crate::addon::native_apps::test_support::fixture_manifest_toml(true)
                    ],
                )
                .unwrap();

            let first = open(&main, "org-b", &addon_id, migrate).unwrap();
            first
                .write()
                .unwrap()
                .execute("INSERT INTO things (name) VALUES ('written-for-b')", [])
                .unwrap();
            close(&addon_id);

            let default_dir = crate::addon::fs_sandbox::addon_data_dir_path(
                crate::services::org::DEFAULT_ORG_ID,
                &addon_id,
            )
            .unwrap();
            assert!(default_dir.join("fixture.db").exists());
            assert!(
                !crate::addon::fs_sandbox::addon_data_dir_path("org-b", &addon_id)
                    .unwrap()
                    .exists(),
                "the requesting org got a database directory of its own"
            );

            let again = open(&main, "org-a", &addon_id, migrate).unwrap();
            let name: String = again
                .read()
                .unwrap()
                .query_row("SELECT name FROM things", [], |r| r.get(0))
                .unwrap();
            assert_eq!(name, "written-for-b");
            close(&addon_id);
            let _ = std::fs::remove_dir_all(default_dir);
        });
    }

    #[test]
    fn open_at_registers_then_close_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content.db");
        let pool = open_at("test-app-00000001", &path, migrate).unwrap();
        assert!(registry().contains_key("test-app-00000001"));
        // Second open returns the registered pool, not a new connection.
        let again = open_at("test-app-00000001", &path, migrate).unwrap();
        assert!(Arc::ptr_eq(&pool, &again));
        close("test-app-00000001");
        assert!(!registry().contains_key("test-app-00000001"));
        assert!(path.exists());
    }
}
