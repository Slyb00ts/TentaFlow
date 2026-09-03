// =============================================================================
// File: bus/native.rs — TentaBus as a native app (plan-app-platform §2 W2,
//       §7 W6). Lifecycle hooks the platform calls per instance:
//       `native_init` provisions the instance's own `tentabus.db`,
//       `native_on_enable`/`native_on_disable` start/stop the engine,
//       `native_teardown_plan`/`native_teardown` preview and execute the
//       uninstall wipe.
//
//       W2 SKELETON: every hook below compiles, is registered in
//       `addon::native_apps::REGISTRY` (`bundled.rs`), and keeps the
//       platform contract
//       (`every_teardown_plan_lists_the_data_dir_and_leaves_it_untouched`),
//       but starts/stops no engine and deletes no row — the engine registry
//       (`bus::init_instance`/`bus::stop_instance`, W4), replication (W5)
//       and the instance-scoped core tables (migrations 141-145, W3) they
//       would act on do not exist yet. Each deferred body says so below and
//       names the wave that fills it in.
// =============================================================================

use anyhow::Result;

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::db::DbPool;

use super::db;
use super::instance::BusInstanceId;

/// `[addon].id` in `bus/app-manifest.toml`, the id `addon::native_apps`
/// registers this package's hooks under.
pub const PACKAGE_ID: &str = BusInstanceId::PACKAGE_ID;

/// The instance's own `tentabus.db` (consumer groups, pause state), opened
/// on first use. Everything else — topics, partitions, schemas, field
/// policies, ACLs — stays in the main database (§1.4); nothing else lives
/// here.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// Native init hook: creates the instance's `tentabus.db` and brings its
/// schema up to date. Idempotent — it re-runs on install, on every enable
/// (`native_on_enable` calls it again below), on a replicated
/// install/update (`AddonManager::reconcile_synced_addon`), and on every
/// boot for every ENABLED instance (`AddonManager::start_installed_native_
/// instances`, called from `tentaflow/src/main.rs`; W1 fix round — boot did
/// not run any native hook before that). Deliberately does NOT start the
/// engine (`bus::init_instance`, W4): a freshly installed instance starts
/// DISABLED (`addon/lifecycle.rs:268-277`), and a disabled bus must not hold
/// flocks on its segments.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    tracing::info!(
        "native app '{}': TentaBus instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// Starts the engine, its three background threads, and — when the mesh
/// manager exists on this node — replication + the accept-handler
/// registration (§7 W6). W2 skeleton: `bus::init_instance` and the
/// `BUS_INSTANCES` registry it would start against do not exist until W4, so
/// this only re-runs the idempotent db open — enough to exercise the
/// `on_enable` wiring itself without any bus behaviour change.
pub fn native_on_enable(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    Ok(())
}

/// `disable_semantics = "stop"`: stops the engine's three background
/// threads, replication, and drops every partition handle (§7 W6). W2
/// skeleton: nothing to stop yet — deferred to W4 (`bus::stop_instance`) and
/// W5 (replication leader/follower runners).
pub fn native_on_disable(_ctx: &NativeAppContext) {
    // Deferred to W4/W6: bus::stop_instance(&instance_id) + replication
    // unregister. No engine can be running yet (native_on_enable does not
    // start one), so there is nothing to stop on this node today.
}

/// Teardown plan (§7 W6), side-effect free — the uninstall dialog calls it
/// on every open, including for a disabled instance. `lifecycle::
/// teardown_plan` sizes each returned entry INDEPENDENTLY via `path_size`
/// and the uninstall dialog SUMS `sizeBytes` over every `removed` entry
/// (`www/js/modules/addons/uninstall-dialog.js`) — so this hook must return
/// entries whose paths never overlap, or the operator sees an inflated
/// total. One entry, one path: `tentabus_data_dir` covers the WHOLE instance
/// data dir (both `log/` — topics, partitions, segments — and
/// `tentabus.db`) in a single `path_size` walk. It also satisfies the
/// platform invariant every registry entry must meet
/// (`native_apps.rs::every_teardown_plan_lists_the_data_dir_and_leaves_it_untouched`).
///
/// The five instance-scoped core tables plus the topic ACL rows
/// (plan-app-platform §1.4) are NOT listed here: they live in the shared
/// `tentaflow.db`, not under this data dir, they have no `instance_id`
/// column to filter by until W3, and W2's `native_teardown` below does not
/// delete them yet. Listing them as `removed: true` here would be a lie the
/// dialog renders as fact; W6 adds the entry back with the real per-instance
/// row counts and wires the delete into `native_teardown`.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentabus_data_dir",
        description: "instance data directory: topics, partitions and segments (log/) \
                       plus tentabus.db (consumer groups, pause state)",
        removed: true,
    }])
}

/// Full teardown (§7 W6, in order): stop the engine and replication, close
/// the content db handle, delete this instance's rows from the five synced
/// core tables plus its topic ACL rows (each publishing a sync tombstone so
/// the uninstall propagates fleet-wide), bump the schema registry
/// generation, audit `bus.instance.teardown` with the counts.
///
/// W2 skeleton: only step 2 (`app_db::close`) runs — it is MANDATORY before
/// the platform's `remove_dir_all` regardless of wave, an open WAL handle
/// blocks it on Windows (`addon/lifecycle.rs:461-463`). Stopping the engine
/// (W4/W6, via `native_on_disable`) and deleting the instance-scoped core
/// rows (W3 for the tables, W6 for the delete-by-instance repository
/// functions and their tombstones) are deferred — neither exists yet.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    crate::addon::app_db::close(ctx.addon_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        db: &'a DbPool,
        addon_id: &'static str,
        data_dir: std::path::PathBuf,
    ) -> NativeAppContext<'a> {
        NativeAppContext {
            db,
            addon_id,
            org_id: "default",
            data_dir,
        }
    }

    fn test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn package_id_matches_the_instance_id_prefix() {
        assert_eq!(PACKAGE_ID, "tentabus");
        assert_eq!(PACKAGE_ID, BusInstanceId::PACKAGE_ID);
    }

    #[test]
    fn teardown_plan_lists_exactly_one_entry_covering_the_whole_data_dir() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        let entries = native_teardown_plan(&c).expect("plan");
        let kinds: Vec<&str> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec!["tentabus_data_dir"],
            "one entry per path — a second entry whose path is a subset of \
             the data dir would double-count bytes in the uninstall dialog's \
             total (it sums sizeBytes over every removed entry independently)"
        );
        assert!(
            entries.iter().all(|e| e.removed),
            "every entry is removed on wipe"
        );
        assert!(entries.iter().any(|e| e.path == tmp.path()));
    }

    #[test]
    fn teardown_plan_has_no_duplicate_paths() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        let entries = native_teardown_plan(&c).expect("plan");
        let mut paths: Vec<&std::path::Path> = entries.iter().map(|e| e.path.as_path()).collect();
        let before = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(
            paths.len(),
            before,
            "no two entries may share a path — the uninstall dialog would \
             render the same directory twice and the byte total would be \
             inflated by however many entries repeat it"
        );
    }

    #[test]
    fn teardown_plan_is_side_effect_free() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("marker"), b"x").expect("seed file");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        native_teardown_plan(&c).expect("plan");
        assert!(
            tmp.path().join("marker").exists(),
            "plan must not touch the data dir"
        );
    }

    #[test]
    fn teardown_closes_the_content_db_handle_without_error() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        // No instance row installed — teardown must still succeed: closing an
        // addon_id that was never opened is a documented no-op
        // (`addon/app_db.rs::close`).
        native_teardown(&c).expect("teardown");
    }
}
