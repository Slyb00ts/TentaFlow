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

pub mod access;
pub mod db;
pub mod policy;
pub mod probe;

use anyhow::{anyhow, Result};

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::db::DbPool;

pub const PACKAGE_ID: &str = "tentavm";

/// The package manifest, compiled in. The catalog registration
/// (`addon::bundled::NATIVE_APP_PACKAGES`) is deliberately NOT here yet: an
/// installable package needs its i18n keys and its Router screen, which come
/// with the UI shell. Until then the manifest is what the hooks and their
/// tests are checked against.
pub const APP_MANIFEST: &str = include_str!("app-manifest.toml");

/// Status a freshly published node host carries until the environment probe
/// has run. Not "ready": nothing has yet shown that this node can start a
/// machine, and a host promising more than it can do is worse than one asking
/// to be set up.
///
/// It is a PLACEHOLDER, not a verdict — the probe replaces it with
/// `probe::host_status`, which is the only thing that can say `unsupported`.
/// A machine without VT-x reads `needs_install` for as long as this stands,
/// which is why `native_init` schedules the probe and the dashboard read
/// measures on first use.
const INITIAL_HOST_STATUS: &str = "needs_install";

/// The instance database, opened (and created) on first use.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// One timestamp format for the whole app: RFC 3339, UTC, seconds precision.
/// `access.rs` compares two of these as STRINGS to decide whether a term has
/// passed, which is only correct because this shape sorts lexicographically in
/// the same order it sorts chronologically — so there is exactly one function
/// that mints them.
/// The same clock, reachable from the sync arm. Named separately so the two
/// callers are visible: `now()` is the app's, this is the materializer's.
pub(crate) fn now_for_registry() -> String {
    now()
}

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The name the host card shows. The identity registry is the source of truth
/// for a node's name, the hostname is what the node calls itself before an
/// admin renamed it, and the node id is the last resort — the column is NOT
/// NULL and a card without a name is unusable, so an empty string may never
/// reach the row.
fn local_display_name(main_db: &DbPool, node_id: &str) -> String {
    crate::db::repository::get_sync_node_identity(main_db, node_id)
        .ok()
        .flatten()
        .map(|identity| identity.display_name)
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            let host = crate::mesh::node_info_collector::local_hostname();
            (!host.trim().is_empty()).then_some(host)
        })
        .unwrap_or_else(|| node_id.to_string())
}

/// Publishes `node_id` as a host of the organization (plan §4.1). Hosts are
/// org-scoped, so this row is written once for the node and shared by every
/// environment.
///
/// The id of a `node` host IS the mesh node id: the node already carries a
/// fleet-unique identifier, and minting a second one would only add a mapping
/// nobody can resolve from a job or a grant. That is also why the caller must
/// pass a REAL node id — a placeholder would become the primary key of a
/// replicated identity row that nothing ever deletes, and after step 7 two
/// different nodes would claim it.
///
/// Re-running it must not disturb a node that has already been probed: the
/// status, the engine list and the capabilities belong to the probe, so the
/// upsert touches only the name and the ownership, and only when one of them
/// actually changed — a no-op boot must not produce a registry write that the
/// sync ledger would then replicate fleet-wide.
fn ensure_local_host(main_db: &DbPool, org_id: &str, node_id: &str) -> Result<()> {
    let display_name = local_display_name(main_db, node_id);
    let now = now();
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm: main db lock: {e}"))?;
    // One transaction for the row and its capture. The Sync Ledger mints the
    // HLC inside this transaction, so a capture committed without its row (or a
    // row without its capture) would publish a state that never existed.
    let tx = conn.transaction()?;
    let changed = tx.execute(
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
    // The registry is replicated, so a row that changed has to leave the node —
    // a descriptor only says that it MAY. The condition is the row count SQLite
    // itself reports for the upsert, which is the answer to "did the WHERE
    // clause above fire": the no-op-boot rule and the no-op-capture rule are
    // then one rule with one implementation, and a boot that writes nothing
    // cannot mint an operation that says otherwise.
    if changed > 0 {
        crate::sync::tentavm_registry::capture_row(
            &tx,
            crate::sync::core_registry::CoreSyncResourceKind::VmHost,
            &[node_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Native init hook: the instance database and this node's host row.
/// Idempotent — install and every reconcile call it again.
///
/// Without a mesh identity the host row is SKIPPED, not faked. `vm_hosts.id`
/// of a node host is the node id itself, so a placeholder would mint a
/// permanent, replicated identity row for a node that has none; the next boot
/// with a working identity would add a second `kind='node'` row next to it and
/// the partial unique index would not catch it. `sync::runtime::init` failing
/// does not stop the daemon (`tentaflow/src/main.rs`), so this is a real
/// production state — and nothing recovers from it on its own. The `init` hook
/// runs on instance install (`lifecycle.rs`) and from `reconcile_synced_addon`,
/// which fires only on a REPLICATED change to this instance or its package
/// blob; the daemon has no startup pass over installed native instances (PLAN
/// §19 lists that as phase-0 work). Until one of those happens the only trace
/// is the `warn!` below.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    match publish_local_host(ctx.db, ctx.org_id, crate::sync::runtime::local_node_id())? {
        Some(node_id) => {
            // §17.5 step 2 is "init: probe → host row". The row is above; the
            // probe is scheduled rather than awaited, because this hook is
            // synchronous and a probe costs several process spawns. It is also
            // not the only trigger: the daemon has no startup pass over
            // installed native instances, so `probe::ensure_local_probe`
            // measures on the first dashboard read as well. Whichever runs
            // first, the other one finds a fresh answer and returns.
            let scheduled =
                probe::schedule_local_probe(ctx.db, ctx.org_id, ctx.addon_id, &node_id);
            tracing::info!(
                "native app '{}': TentaVM initialized at {:?} (host {node_id}, \
                 environment probe scheduled: {scheduled})",
                ctx.addon_id,
                ctx.data_dir
            )
        }
        None => tracing::warn!(
            "native app '{}': TentaVM initialized at {:?} WITHOUT a host row — \
             this node has no mesh identity (sync runtime not running); the host \
             appears when a replicated change to this instance re-runs init, not \
             on the next boot",
            ctx.addon_id,
            ctx.data_dir
        ),
    }
    Ok(())
}

/// The identity decision of [`native_init`], separated so BOTH outcomes are
/// testable without an installed instance: `Some` publishes the host row and
/// returns its id, `None` publishes nothing.
fn publish_local_host(
    main_db: &DbPool,
    org_id: &str,
    node_id: Option<String>,
) -> Result<Option<String>> {
    let Some(node_id) = node_id else {
        return Ok(None);
    };
    ensure_local_host(main_db, org_id, &node_id)?;
    Ok(Some(node_id))
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
///
/// The registry row is listed as well, and it is the row an admin is most
/// likely to be surprised by: `lifecycle::uninstall` deletes the `addon_*`
/// tables and the `addons` row, and NOTHING else — the `vm_*` rows carrying
/// this environment's `instance_id` stay in the shared database. Removing
/// them is a replicated delete, so it belongs to the sync step; until then
/// the dialog says so out loud instead of implying a clean wipe.
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
    entries.push(TeardownEntry {
        // No path: these are ROWS in the shared platform database, and
        // `lifecycle::teardown_plan` sizes every entry it is given. Naming the
        // main database here made the dialog print its whole size — gigabytes
        // of other apps' data — as "what this environment leaves behind". An
        // empty path measures 0 and prints nothing, which is the honest answer
        // until `TeardownEntry` can say "not a path" (see step 13).
        path: std::path::PathBuf::new(),
        kind: "tentavm_registry_rows",
        description: "registry rows of this environment (machines, grants, settings, jobs, tags) stay in the shared database: deleting them is a replicated operation and lands with the sync step",
        removed: false,
    });
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
pub(crate) fn guests_root(addon_id: &str) -> std::path::PathBuf {
    crate::paths::tentaflow_home()
        .join("tentavm")
        .join(addon_id)
        .join("guests")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "1f2e3d4c5b6a79880f1e2d3c4b5a69780f1e2d3c4b5a69781f2e3d4c5b6a7988";
    const ORG: &str = crate::services::org::DEFAULT_ORG_ID;

    fn main_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    fn hosts(db: &DbPool) -> Vec<(String, String, String, String, String)> {
        let conn = db.read().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, node_id, display_name, status, owner_node_id \
                 FROM vm_hosts ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
    }

    /// The row `native_init` publishes: one host for this node, owned by it,
    /// waiting for the environment probe. The status is asserted against the
    /// literal of plan §2/§17.5, not against the constant the code uses.
    #[test]
    fn init_publishes_this_node_as_a_host() {
        let db = main_db();
        let published = publish_local_host(&db, ORG, Some(NODE.to_string())).unwrap();
        assert_eq!(published.as_deref(), Some(NODE));
        let rows = hosts(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, NODE, "the host id of a node IS its node id");
        assert_eq!(rows[0].1, NODE);
        assert!(!rows[0].2.is_empty(), "a host card needs a name");
        assert_eq!(rows[0].3, "needs_install");
        assert_eq!(rows[0].4, NODE, "a node owns its own host row");
    }

    /// A node without a mesh identity publishes NOTHING. A placeholder id
    /// would become the primary key of a replicated identity row that no code
    /// path ever deletes, and the next boot with a real identity would add a
    /// second `kind='node'` row beside it.
    #[test]
    fn a_node_without_an_identity_publishes_no_host() {
        let db = main_db();
        assert_eq!(publish_local_host(&db, ORG, None).unwrap(), None);
        assert!(hosts(&db).is_empty());

        // …and the identity arriving later publishes exactly one row, so the
        // skipped boot costs nothing but a delay.
        publish_local_host(&db, ORG, Some(NODE.to_string())).unwrap();
        assert_eq!(hosts(&db).len(), 1);
    }

    /// Reconcile runs the hook again on every boot and on every replicated
    /// change of the instance row.
    #[test]
    fn repeated_init_does_not_duplicate_the_local_host_row() {
        let db = main_db();
        ensure_local_host(&db, ORG, NODE).unwrap();
        ensure_local_host(&db, ORG, NODE).unwrap();
        ensure_local_host(&db, ORG, NODE).unwrap();
        assert_eq!(hosts(&db).len(), 1);
    }

    /// What the probe wrote is the truth about the host; a later init must not
    /// push it back to `needs_install`.
    #[test]
    fn repeated_init_keeps_the_probed_state_of_the_host() {
        let db = main_db();
        ensure_local_host(&db, ORG, NODE).unwrap();
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE vm_hosts SET status = 'ready', engines_json = '[\"kvm\"]' WHERE id = ?1",
                rusqlite::params![NODE],
            )
            .unwrap();
        }
        ensure_local_host(&db, ORG, NODE).unwrap();
        let conn = db.read().unwrap();
        let (status, engines): (String, String) = conn
            .query_row(
                "SELECT status, engines_json FROM vm_hosts WHERE id = ?1",
                rusqlite::params![NODE],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "ready");
        assert_eq!(engines, "[\"kvm\"]");
    }

    /// The manifest is the contract the platform reads when the package is
    /// registered into the catalog (step 9). A typo here is a package that
    /// silently never reaches it.
    #[test]
    fn the_manifest_declares_a_multi_instance_native_app() {
        let manifest =
            crate::addon::lifecycle::parse_manifest_toml(APP_MANIFEST).expect("manifest parses");
        assert!(manifest.is_native());
        assert_eq!(manifest.addon_id, PACKAGE_ID);
        let native = manifest.native.as_ref().expect("[native] section");
        native.validate().expect("native section is valid");
        // Environments (plan §2): several installed copies of this app coexist.
        assert!(!native.singleton);
        assert_eq!(native.db_file.as_deref(), Some("tentavm.db"));
        assert_eq!(native.routes, vec!["tentavm".to_string()]);
        assert_eq!(native.i18n_namespace.as_deref(), Some("tentavm"));
        // The ids plan §15 authorizes with, verbatim.
        // Id, RISK and default together. Risk is what the consent dialog
        // shows the admin before they grant a permission, and nothing else
        // checks it: the native install path does not call `validate_manifest`,
        // so lowering `vm.admin` from critical to low is otherwise a silent
        // edit. `vm.read` is the only permission a fresh install grants —
        // everything else is a decision an admin takes per environment.
        let permissions: Vec<(&str, &str, &str)> = manifest
            .declared_permissions
            .iter()
            .map(|p| (p.id.as_str(), p.risk.as_str(), p.default_grant.as_str()))
            .collect();
        assert_eq!(
            permissions,
            vec![
                ("vm.read", "low", "allow"),
                ("vm.operate", "medium", "deny"),
                ("vm.create", "medium", "deny"),
                ("vm.migrate", "high", "deny"),
                ("vm.hosts.manage", "high", "deny"),
                ("vm.devices.manage", "high", "deny"),
                ("vm.connectors.manage", "critical", "deny"),
                ("vm.admin", "critical", "deny"),
            ]
        );
    }

    /// Until the catalog registration lands with the UI shell, the package
    /// must NOT be installable: its tile would read `apps.tentavm.name` and
    /// its route would not exist.
    #[test]
    fn the_package_is_not_in_the_catalog_yet() {
        assert!(
            crate::addon::bundled::native_manifest(PACKAGE_ID).is_none(),
            "TentaVM must stay out of NATIVE_APP_PACKAGES until the UI shell \
             brings its i18n keys and its Router screen"
        );
    }

    /// The plan lists what is actually there and, above all, what STAYS: the
    /// registry rows of the environment survive the uninstall, and the dialog
    /// has to say so.
    #[test]
    fn teardown_plan_removes_the_data_dir_and_names_what_stays() {
        let data_dir = std::path::PathBuf::from("/instances/tentavm-00000000");
        let guests = guests_root("tentavm-00000000");

        let bare = teardown_entries(data_dir.clone(), guests.clone(), &|_| false);
        assert_eq!(bare[0].path, data_dir);
        assert!(bare[0].removed);
        let registry = bare
            .iter()
            .find(|e| e.kind == "tentavm_registry_rows")
            .expect("the rows that stay must be listed");
        assert!(!registry.removed);
        // Guard for the size the uninstall dialog prints: `teardown_plan` sizes
        // EVERY entry, so any path here becomes a byte count next to the words
        // "stays behind". Rows in the shared database have no path, and naming
        // one made the dialog offer the whole platform database as the answer.
        assert_eq!(
            registry.path,
            std::path::PathBuf::new(),
            "rows in the shared database have no path to size"
        );
        // A node that never ran a machine has no runtime directory row.
        assert!(!bare.iter().any(|e| e.kind == "tentavm_guest_runtime"));

        let with_runtime = teardown_entries(data_dir, guests.clone(), &|_| true);
        let row = with_runtime
            .iter()
            .find(|e| e.path == guests)
            .expect("runtime row");
        assert!(!row.removed, "machine disks survive an uninstall");
    }
}
