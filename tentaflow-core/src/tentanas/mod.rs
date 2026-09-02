// ===== File: tentanas/mod.rs — TentaNas, the storage application (plan-02) =====
//
// A native app on the app platform: one global instance, every node runs its
// own copy against its own disks and its own `tentanas.db`. The dashboard
// picks a node in the header and every request of the family is forwarded
// to it (`dispatch/app_route.rs`); the fleet list is the only request the
// dashboard's node answers itself.
//
// Layering:
//   broker      the only place a system command is executed
//   elevation   the node's privilege channel (helper or armed password)
//   environment what the node can do (features, versions, package manager)
//   fleet       the node list of the header and each node's published summary
//   disks       inventory, live I/O, SMART, health, sampler
//   zfs         shared plumbing of the ZFS layer (tool lookup, -Hp parsing)
//   pools       zpool list/status/iostat, health, the layout wizard
//   datasets    zfs list/get for filesystems, zvols and their properties
//   snapshots   snapshot list, GFS retention, the automatic snapshot job
//   scheduler   scrubs, automatic snapshots and SMART tests on a clock
//   keystore    encryption keys of native-ZFS datasets (outside the data dir)
//   jobs        long-running work with a persisted log
//   db          schema and rows of tentanas.db
//
// Uninstall NEVER destroys pools or user data: teardown removes the app's
// own database and the privilege channel it created, nothing else (§5.8).
// The dataset keystore is deliberately NOT in that database — see keystore.rs.

pub mod broker;
pub mod datasets;
pub mod db;
pub mod disks;
pub mod elevation;
pub mod environment;
pub mod fleet;
pub mod jobs;
pub mod keystore;
pub mod pools;
pub mod scheduler;
pub mod snapshots;
pub mod zfs;

use anyhow::Result;

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::db::DbPool;

pub const PACKAGE_ID: &str = "tentanas";

/// The instance database, opened on first use.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// Native init hook: schema, orphaned jobs, and the sampler. Idempotent —
/// reconcile calls it again on every boot and enable.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    let pool = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    let orphaned = db::fail_orphaned_jobs(&pool)?;
    if orphaned > 0 {
        tracing::info!("tentanas: marked {orphaned} interrupted jobs as failed");
    }
    disks::start_sampler(ctx.db.clone(), ctx.addon_id.to_string(), pool.clone());
    scheduler::start(pool);
    tracing::info!(
        "native app '{}': TentaNas initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// Teardown plan (§5.8): the instance database goes with the data dir; the
/// helper + sudoers line are listed as LEFT BEHIND because removing them
/// needs a fresh sudo password, which the Environment tab collects through
/// `ElevationRemoveRequest`. Pure — the uninstall dialog previews it.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    let mut entries = vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentanas_data_dir",
        description: "instance data directory (tentanas.db: disk history, alerts, jobs)",
        removed: true,
    }];
    // The keystore lives outside the data dir precisely so this wipe cannot
    // reach it: the encrypted datasets stay on the pools, so their keys must
    // stay too, and deleting them is a separate deliberate act.
    let keystore = keystore::store_path(ctx.addon_id);
    if keystore.exists() {
        entries.push(TeardownEntry {
            path: keystore,
            kind: "tentanas_keystore",
            description: "ZFS dataset encryption keys (kept: the datasets survive uninstall)",
            removed: false,
        });
    }
    if std::path::Path::new(tentanas_helper::HELPER_INSTALL_PATH).exists() {
        entries.push(TeardownEntry {
            path: tentanas_helper::HELPER_INSTALL_PATH.into(),
            kind: "tentanas_helper",
            description: "privilege helper (remove with a sudo password from the Environment tab)",
            removed: false,
        });
    }
    if std::path::Path::new(tentanas_helper::SUDOERS_INSTALL_PATH).exists() {
        entries.push(TeardownEntry {
            path: tentanas_helper::SUDOERS_INSTALL_PATH.into(),
            kind: "tentanas_sudoers",
            description: "sudoers rule for the privilege helper",
            removed: false,
        });
    }
    Ok(entries)
}

/// Native teardown hook: drops the armed password and closes the instance
/// database so the data dir wipe that follows finds no open handle.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    elevation::disarm();
    crate::addon::app_db::close(ctx.addon_id);
    Ok(())
}
