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
pub fn open(main_db: &DbPool, org_id: &str, addon_id: &str, migrate: Migrate) -> Result<DbPool> {
    if let Some(pool) = registry().get(addon_id) {
        return Ok(pool.clone());
    }
    let row = crate::db::repository::get_addon(main_db, addon_id)?
        .ok_or_else(|| anyhow!("app instance '{addon_id}' is not installed"))?;
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&row.manifest_json)?;
    let db_file = manifest
        .native
        .as_ref()
        .and_then(|n| n.db_file.as_deref())
        .ok_or_else(|| anyhow!("app instance '{addon_id}' declares no native.db_file"))?;
    let dir = crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id)
        .map_err(|e| anyhow!("instance data dir for '{addon_id}': {e:?}"))?;
    open_at(addon_id, &dir.join(db_file), migrate)
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
