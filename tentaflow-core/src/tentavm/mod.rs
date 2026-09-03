// ===== File: tentavm/mod.rs — TentaVM, the virtualization application =====
//
// A native app on the app platform, and the first one that is NOT a singleton:
// an installed instance is an environment ("Środowisko", plan §2) with its own
// registry of machines, its own permission matrix and its own `tentavm.db` on
// every node. Several environments live side by side and SHARE the hosts —
// every mesh node is a host in every environment, without an "add host" step.
//
// The data is split in two, and the split is the reason both halves exist:
//
//   registry (main `tentaflow.db`, synchronized)  what exists and who may
//       touch it — hosts, connectors, machines, disks, snapshots, jobs. Any
//       node can answer a list request from its own copy.
//   instance DB (`tentavm.db`, per node, NEVER synchronized)  what only the
//       owner node knows or may hold — secrets, provisioning inputs, saga
//       state, probe results, event and job histories (`db.rs`).
//
// `native_init` runs on every supported node, on install and on every sync
// reconcile, so it must be idempotent: it opens (creating) the instance
// database, brings its schema up to date, and publishes THIS node as a host
// row of the organization. The host row is created by `init` by rule
// (plan §4.1) — a node is a host because it exists, not because somebody
// added it.

pub mod db;

use anyhow::{anyhow, Result};

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::db::DbPool;

pub const PACKAGE_ID: &str = "tentavm";

/// Status a freshly published node host carries until the environment probe
/// has run. Not "ready": nothing has yet shown that this node can start a
/// machine, and a host promising more than it can do is worse than one asking
/// to be set up.
const INITIAL_HOST_STATUS: &str = "needs_install";

/// The instance database, opened (and created) on first use.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// Mesh node id of this node. `local_node_id` is `None` before the sync
/// runtime starts (single-node boots, tests); the same fallback the rest of
/// the app platform uses keeps the row addressable instead of absent.
fn local_node_id() -> String {
    crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The name the host card shows. The identity registry is the source of truth
/// for a node's name; the hostname is what the node calls itself before any
/// admin renamed it.
fn local_display_name(main_db: &DbPool, node_id: &str) -> String {
    crate::db::repository::get_sync_node_identity(main_db, node_id)
        .ok()
        .flatten()
        .map(|identity| identity.display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(crate::mesh::node_info_collector::local_hostname)
}

/// Publishes this node as a host of the organization (plan §4.1) and returns
/// the host id. Hosts are org-scoped, so this row is written once for the node
/// and shared by every environment.
///
/// The id of a `node` host IS the mesh node id: the node already carries a
/// fleet-unique identifier, and minting a second one would only add a mapping
/// nobody can resolve from a job or a grant.
///
/// Re-running it must not disturb a node that has already been probed: the
/// status, the engine list and the capabilities belong to the probe, so the
/// upsert touches only the name and the ownership, and only when the name
/// actually changed — a no-op boot must not produce a registry write that the
/// sync ledger would then replicate fleet-wide.
fn ensure_local_host(main_db: &DbPool, org_id: &str) -> Result<String> {
    let node_id = local_node_id();
    let display_name = local_display_name(main_db, &node_id);
    let now = now();
    let conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm: main db lock: {e}"))?;
    conn.execute(
        "INSERT INTO vm_hosts \
            (id, org_id, kind, node_id, connector_id, external_ref, display_name, \
             engines_json, capabilities_json, status, owner_node_id, owner_epoch, \
             created_at, updated_at, updated_by_node) \
         VALUES (?1, ?2, 'node', ?1, NULL, NULL, ?3, '[]', '{}', ?4, ?1, 0, ?5, ?5, ?1) \
         ON CONFLICT(id) DO UPDATE SET \
             display_name = excluded.display_name, \
             owner_node_id = excluded.owner_node_id, \
             updated_at = excluded.updated_at, \
             updated_by_node = excluded.updated_by_node \
         WHERE vm_hosts.display_name <> excluded.display_name \
            OR vm_hosts.owner_node_id <> excluded.owner_node_id",
        rusqlite::params![node_id, org_id, display_name, INITIAL_HOST_STATUS, now],
    )?;
    Ok(node_id)
}

/// Native init hook: the instance database and this node's host row.
/// Idempotent — install and every reconcile call it again.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    let host_id = ensure_local_host(ctx.db, ctx.org_id)?;
    crate::addon::native_apps::record_node_status(ctx.db, ctx.addon_id, "ready", "");
    tracing::info!(
        "native app '{}': TentaVM initialized at {:?} (host {host_id})",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// The rows of the teardown plan. `exists` is passed in so the SHAPE of the
/// plan is testable without this node having any of these directories.
///
/// Machines are not touched. Uninstalling an environment is a management
/// decision about TentaFlow, not about the workloads a hypervisor is running:
/// libvirt/Hyper-V/Incus keep their domains, and the machines' runtime files
/// (disks, seed, NVRAM, TPM — plan §4.3) live outside the instance data dir
/// precisely so this wipe cannot reach them. The host row is org-scoped and
/// shared by the other environments, so it stays too.
fn teardown_entries(
    data_dir: std::path::PathBuf,
    guests: std::path::PathBuf,
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Vec<TeardownEntry> {
    let mut entries = vec![TeardownEntry {
        path: data_dir,
        kind: "tentavm_data_dir",
        description:
            "instance data directory (tentavm.db: connector secrets, provisioning inputs, job history)",
        removed: true,
    }];
    if exists(&guests) {
        entries.push(TeardownEntry {
            path: guests,
            kind: "tentavm_guest_runtime",
            description: "machine runtime files of this environment: disks, seed, NVRAM and TPM (kept — the machines keep running)",
            removed: false,
        });
    }
    entries
}

/// Teardown plan: what removing THIS environment from THIS node takes with it.
/// Pure — the uninstall dialog calls it on every open.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(teardown_entries(
        ctx.data_dir.clone(),
        guests_root(ctx.addon_id),
        &|path| path.exists(),
    ))
}

/// Runtime directory of an environment's machines (plan §4.3). It sits outside
/// the instance data dir because the disks of a running machine must not be
/// wiped by an app uninstall.
fn guests_root(addon_id: &str) -> std::path::PathBuf {
    crate::paths::tentaflow_home()
        .join("tentavm")
        .join(addon_id)
        .join("guests")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    fn hosts(db: &DbPool) -> Vec<(String, String, String, String)> {
        let conn = db.read().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, node_id, display_name, status FROM vm_hosts ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    /// The row `native_init` publishes: one host for this node, owned by it,
    /// waiting for the environment probe.
    #[test]
    fn init_publishes_this_node_as_a_host() {
        let db = main_db();
        let host_id = ensure_local_host(&db, crate::services::org::DEFAULT_ORG_ID).unwrap();
        let rows = hosts(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, host_id);
        assert_eq!(rows[0].1, host_id);
        assert!(!rows[0].2.is_empty(), "a host card needs a name");
        assert_eq!(rows[0].3, INITIAL_HOST_STATUS);
    }

    /// Reconcile runs the hook again on every boot and on every replicated
    /// change of the instance row.
    #[test]
    fn repeated_init_does_not_duplicate_the_local_host_row() {
        let db = main_db();
        let org = crate::services::org::DEFAULT_ORG_ID;
        ensure_local_host(&db, org).unwrap();
        ensure_local_host(&db, org).unwrap();
        ensure_local_host(&db, org).unwrap();
        assert_eq!(hosts(&db).len(), 1);
    }

    /// What the probe wrote is the truth about the host; a later init must not
    /// push it back to `needs_install`.
    #[test]
    fn repeated_init_keeps_the_probed_state_of_the_host() {
        let db = main_db();
        let org = crate::services::org::DEFAULT_ORG_ID;
        let host_id = ensure_local_host(&db, org).unwrap();
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE vm_hosts SET status = 'ready', engines_json = '[\"kvm\"]' WHERE id = ?1",
                rusqlite::params![host_id],
            )
            .unwrap();
        }
        ensure_local_host(&db, org).unwrap();
        let conn = db.read().unwrap();
        let (status, engines): (String, String) = conn
            .query_row(
                "SELECT status, engines_json FROM vm_hosts WHERE id = ?1",
                rusqlite::params![host_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "ready");
        assert_eq!(engines, "[\"kvm\"]");
    }

    /// The package manifest is the contract the platform reads at boot: a
    /// typo here is a package that silently never reaches the catalog.
    #[test]
    fn the_bundled_manifest_declares_a_multi_instance_native_app() {
        let toml = crate::addon::bundled::native_manifest(PACKAGE_ID)
            .expect("TentaVM must be a bundled native package");
        let manifest = crate::addon::lifecycle::parse_manifest_toml(toml).expect("manifest parses");
        assert!(manifest.is_native());
        assert_eq!(manifest.addon_id, PACKAGE_ID);
        let native = manifest.native.as_ref().expect("[native] section");
        native.validate().expect("native section is valid");
        // Environments (plan §2): several installed copies of this app coexist.
        assert!(!native.singleton);
        assert_eq!(native.db_file.as_deref(), Some("tentavm.db"));
        assert_eq!(native.routes, vec!["tentavm".to_string()]);
        assert_eq!(native.i18n_namespace.as_deref(), Some("tentavm"));
        let permissions: Vec<&str> = manifest
            .declared_permissions
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        for required in ["vm.read", "vm.deploy", "vm.manage", "vm.admin"] {
            assert!(permissions.contains(&required), "{required} missing");
        }
        // `vm.read` is the only one a fresh install grants: everything else is
        // a decision an admin takes per environment.
        for perm in &manifest.declared_permissions {
            assert!(perm.id.starts_with("vm."), "stray permission {}", perm.id);
            let expected = if perm.id == "vm.read" { "allow" } else { "deny" };
            assert_eq!(perm.default_grant, expected, "default of {}", perm.id);
        }
    }

    /// The plan lists only what is actually there: the data dir is always
    /// removed, and the machines' runtime directory appears only when this
    /// environment has one — and then as consciously kept.
    #[test]
    fn teardown_plan_removes_the_data_dir_and_keeps_machine_runtime() {
        let data_dir = std::path::PathBuf::from("/instances/tentavm-00000000");
        let guests = guests_root("tentavm-00000000");

        let bare = teardown_entries(data_dir.clone(), guests.clone(), &|_| false);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].path, data_dir);
        assert!(bare[0].removed);

        let with_runtime = teardown_entries(data_dir, guests.clone(), &|_| true);
        let row = with_runtime
            .iter()
            .find(|e| e.path == guests)
            .expect("runtime row");
        assert!(!row.removed, "machine disks survive an uninstall");
    }
}
