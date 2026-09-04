// ===== File: tentaquant/mod.rs — TentaQuant, the quantum lab application =====
//
// A native app on the app platform and the FIRST multi-instance one
// (`singleton = false`): one instance is one laboratory — a student group, a
// research team, a company workshop. Each keeps its own `tentaquant.db`, its
// own content directory and, above all, its own permission matrix, which
// intersected with that instance's Visibility IS its membership (plan
// §10.1/§10.2 — `quant.read` is `default = "allow"`, so the matrix alone admits
// the whole organization and Visibility scopes the lab to its group). Nothing
// here maintains a member table, and no request resolves "the" instance by
// package: every request names the lab it means and goes through
// `require_app_instance_permission`.
//
// Layering:
//   db         schema and rows of one lab's tentaquant.db
//   cas        the lab's content store (`files/<sha256>`) and chunked uploads
//   people     the matrix expansion the UI reads instead of a member list
//   circuit    the OpenQASM 3 front end of tier T1 (validate, export, options)
//   keyframes  the recorded evolution of a run, live and in the store
//   runs       T1 execution: slots, cancellation, the run stream, orphans
//   targets    the tiers a lab offers and the `device="auto"` rule
//
// Uninstall removes exactly one lab: `teardown` closes that instance's pool so
// the platform can wipe its directory, and touches nothing else on the node.

pub mod cas;
pub mod circuit;
pub mod db;
pub mod keyframes;
pub mod people;
pub mod runs;
pub mod targets;

use anyhow::Result;

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::db::DbPool;

pub use db::PACKAGE_ID;

/// The instance database of ONE laboratory, opened on first use.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// The instance's own directory — `tentaquant.db` plus the `files/` blob store.
pub fn data_dir(org_id: &str, addon_id: &str) -> Result<std::path::PathBuf> {
    crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id)
        .map_err(|e| anyhow::anyhow!("tentaquant data dir for '{addon_id}': {e:?}"))
}

/// Native init hook: opens (and thereby creates and migrates) the lab's
/// database. Idempotent — reconcile calls it again on every boot and enable,
/// and a migration failure surfaces here as `init_error` node status instead of
/// as a failing request much later.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    tracing::info!(
        "native app '{}': TentaQuant lab initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// Teardown plan: everything a lab owns is inside its own instance directory —
/// the database, the notebooks' blobs and every run artifact. Pure, because the
/// uninstall dialog calls it on every open.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentaquant_data_dir",
        description: "laboratory data directory (tentaquant.db: projects, notebooks, runs — and the files/ content store)",
        removed: true,
    }])
}

/// Native teardown hook: closes THIS lab's pool so the platform can remove its
/// directory. Other instances of the package keep running — closing them, or
/// wiping anything outside `ctx.data_dir`, would make uninstalling one lab
/// destroy another.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    crate::addon::app_db::close(ctx.addon_id);
    tracing::info!(
        "native app '{}': TentaQuant lab closed before wipe",
        ctx.addon_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::tentaquant::PERMISSION_IDS;

    const MANIFEST: &str = include_str!("app-manifest.toml");

    /// The manifest IS the access model of a laboratory (plan §10.2): six
    /// permissions, in this order, with these risk levels and these defaults.
    /// Anything else here would silently change who may do what in every
    /// installed lab on the next reconcile, because `seed_permission_defaults`
    /// reads exactly this table.
    #[test]
    fn the_manifest_declares_exactly_the_six_permissions_of_the_plan() {
        let manifest =
            crate::addon::lifecycle::parse_manifest_toml(MANIFEST).expect("manifest parses");
        assert_eq!(manifest.addon_id, PACKAGE_ID);

        let declared: Vec<(&str, &str, &str)> = manifest
            .declared_permissions
            .iter()
            .map(|p| (p.id.as_str(), p.risk.as_str(), p.default_grant.as_str()))
            .collect();
        assert_eq!(
            declared,
            vec![
                ("quant.read", "low", "allow"),
                ("quant.run", "low", "allow"),
                ("quant.run.gpu", "low", "allow"),
                ("quant.run.qpu", "medium", "allow"),
                ("quant.instruct", "medium", "deny"),
                ("quant.admin", "critical", "deny"),
            ]
        );
        // The protocol constant and the manifest must not drift: responses
        // report granted subsets of PERMISSION_IDS.
        let ids: Vec<&str> = declared.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, PERMISSION_IDS.to_vec());
    }

    /// The first multi-instance native package. `singleton = false` is what
    /// lets a node hold several laboratories, and `db_file` is what
    /// `app_db::open` needs to give each of them its own database.
    #[test]
    fn the_package_is_multi_instance_with_its_own_database() {
        let manifest =
            crate::addon::lifecycle::parse_manifest_toml(MANIFEST).expect("manifest parses");
        assert!(manifest.is_native());
        let native = manifest.native.as_ref().expect("[native] section");
        assert!(!native.singleton);
        assert_eq!(native.db_file.as_deref(), Some("tentaquant.db"));
        assert_eq!(native.routes, vec!["tentaquant".to_string()]);
        assert_eq!(native.i18n_namespace.as_deref(), Some("tentaquant"));
    }

    /// Uninstalling one laboratory removes that laboratory and nothing else:
    /// the plan lists exactly one path, the instance's own directory.
    #[test]
    fn teardown_plan_covers_only_this_instances_directory() {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        let db: crate::db::DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = NativeAppContext {
            db: &db,
            addon_id: "tentaquant-00000000",
            org_id: "org-test",
            data_dir: tmp.path().to_path_buf(),
        };
        let entries = native_teardown_plan(&ctx).expect("plan");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, tmp.path());
        assert_eq!(entries[0].kind, "tentaquant_data_dir");
        assert!(entries[0].removed);
    }
}
