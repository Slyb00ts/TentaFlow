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
// `require_instance_permission`.
//
// Layering:
//   db         schema and rows of one lab's tentaquant.db
//   cas        the lab's content store (`files/<sha256>`) and chunked uploads
//   people     the matrix expansion the UI reads instead of a member list
//   circuit    the OpenQASM 3 front end of tier T1 (validate, export, options)
//   keyframes  the recorded evolution of a run, live and in the store
//   kata       the course: embedded katas, grading, unlocking and the ranking
//   examples   the gallery: embedded circuits with a reference outcome
//   runs       T1 execution: slots, cancellation, the run stream, orphans
//   compare    the distributions of several runs on one aligned axis
//   state      reduced quantities of a run's state, asked for after the fact
//   export     the scientific package of one run, as one .zip
//   targets    the tiers a lab offers and the `device="auto"` rule
//
// Uninstall removes exactly one lab: `teardown` closes that instance's pool so
// the platform can wipe its directory, and touches nothing else on the node.

pub mod cas;
pub mod circuit;
pub mod compare;
pub mod db;
pub mod examples;
pub mod export;
pub mod kata;
pub mod keyframes;
pub mod people;
pub mod runs;
pub mod state;
pub mod targets;

use anyhow::{bail, Result};

use crate::addon::install_steps::{InstallStepOutcome, InstallStepRun, InstallStepStatus};
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

/// Id of the manifest's one install step (`[[install_step]]`).
const BELL_TEST_STEP: &str = "bell-test";

const BELL_QASM3: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\n\
                          h q[0];\ncx q[0], q[1];\nc = measure q;\n";

/// Shots of the Bell test. At 4096 a fair split deviates from 50 % by under
/// 0.8 % (one sigma), so the 5-sigma tolerance below still catches a skewed
/// simulator while a correct one never trips it.
const BELL_SHOTS: u64 = 4096;

/// Fixed seed: the verdict is a property of the simulator, not of luck, and a
/// re-run of the step must give the same answer.
const BELL_SEED: u64 = 0x0BE1_1BE1;

/// Native install-step hook. The Bell test is the only step the manifest
/// declares; any other id reaching this hook is a registry/manifest mismatch.
pub fn native_install_step(
    _ctx: &NativeAppContext,
    run: &InstallStepRun,
) -> Result<InstallStepOutcome> {
    match run.step_id {
        BELL_TEST_STEP => bell_test(),
        other => bail!("TentaQuant declares no install step '{other}'"),
    }
}

/// Runs a 2-qubit Bell circuit through the T1 path — the same front end and
/// simulator crate a lab's runs use, on this node's CPU — and checks the one
/// thing a Bell pair guarantees: only `00` and `11` are ever measured, in a
/// near-even split. Any `01`/`10` is a simulator fault, not noise, because T1
/// has none. Read-only and seeded, so safe to repeat.
fn bell_test() -> Result<InstallStepOutcome> {
    use tentaflow_quantum::sim::statevector;
    use tentaflow_quantum::sim::{Cancel, Device};

    let started = std::time::Instant::now();
    let parsed = circuit::parse(BELL_QASM3, "")
        .map_err(|d| anyhow::anyhow!("the Bell circuit was rejected: {}", d.message))?;
    let options = circuit::sim_options(
        &tentaflow_protocol::tentaquant::SimulateOptions {
            shots: BELL_SHOTS,
            seed: BELL_SEED,
            ..Default::default()
        },
        circuit::MAX_CORE_QUBITS,
    );
    let result = statevector::run(&parsed.circuit, &options, Device::Cpu, BELL_SHOTS, Cancel::none())?;
    let duration_ms = started.elapsed().as_millis();

    let count = |key: &str| result.counts.get(key).copied().unwrap_or(0);
    let (zeros, ones) = (count("00"), count("11"));
    let leaked = result.shots.saturating_sub(zeros + ones);
    let share = zeros as f64 / result.shots.max(1) as f64;
    let sigma = (0.25 / result.shots.max(1) as f64).sqrt();
    let skew = (share - 0.5).abs();

    let details = vec![
        ("00".to_string(), zeros.to_string()),
        ("11".to_string(), ones.to_string()),
        ("other".to_string(), leaked.to_string()),
        ("shots".to_string(), result.shots.to_string()),
        ("duration_ms".to_string(), duration_ms.to_string()),
    ];
    let split = format!(
        "00 = {zeros}, 11 = {ones} of {} shots ({:.1} % / {:.1} %) in {duration_ms} ms",
        result.shots,
        share * 100.0,
        (1.0 - share) * 100.0
    );
    if leaked > 0 {
        return Ok(InstallStepOutcome {
            status: InstallStepStatus::Failed,
            message: format!("Bell test failed: {leaked} shots measured 01 or 10; {split}"),
            details,
        });
    }
    if skew > 5.0 * sigma {
        return Ok(InstallStepOutcome {
            status: InstallStepStatus::Failed,
            message: format!("Bell test failed: the 00/11 split is skewed; {split}"),
            details,
        });
    }
    Ok(InstallStepOutcome {
        status: InstallStepStatus::Ok,
        message: format!("Bell test passed: {split}"),
        details,
    })
}

/// Teardown plan: everything a lab owns is inside its own instance directory —
/// the database, the notebooks' blobs and every run artifact. Pure, because the
/// uninstall dialog calls it on every open.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentaquant_data_dir",
        description: "laboratory data directory (tentaquant.db: projects, notebooks, runs — and the files/ content store)".into(),
        removed: true,
        ..Default::default()
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

    #[test]
    fn the_manifest_declares_the_bell_test_step_and_nothing_else() {
        let manifest =
            crate::addon::lifecycle::parse_manifest_toml(MANIFEST).expect("manifest parses");
        let ids: Vec<&str> = manifest.install_steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec![BELL_TEST_STEP]);
        assert!(manifest.install_steps[0].fields.is_empty(), "the Bell test asks nothing");
    }

    /// The step is a real run of the T1 path: a correct simulator yields only
    /// 00 and 11, in a split the tolerance accepts, and reports what it measured.
    #[test]
    fn the_bell_test_passes_on_the_t1_simulator_and_reports_its_measurements() {
        let outcome = bell_test().expect("runs");
        assert_eq!(outcome.status, InstallStepStatus::Ok, "{}", outcome.message);
        let detail = |k: &str| -> u64 {
            outcome
                .details
                .iter()
                .find(|(name, _)| name == k)
                .unwrap_or_else(|| panic!("detail '{k}' missing"))
                .1
                .parse()
                .expect("numeric detail")
        };
        assert_eq!(detail("shots"), BELL_SHOTS);
        assert_eq!(detail("00") + detail("11"), BELL_SHOTS);
        assert_eq!(detail("other"), 0);
        assert!(outcome.message.contains("passed"), "{}", outcome.message);
        // Seeded: a re-run measures the same split.
        let again = bell_test().expect("runs again");
        assert_eq!(again.details[..4], outcome.details[..4]);
    }

    #[test]
    fn the_hook_dispatches_the_bell_step_and_refuses_unknown_ids() {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        let db: crate::db::DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let ctx = NativeAppContext {
            db: &db,
            addon_id: "tentaquant-00000000",
            org_id: "org-test",
            data_dir: std::path::PathBuf::from("/nonexistent"),
        };
        let values = std::collections::BTreeMap::new();
        let ok = native_install_step(&ctx, &InstallStepRun { step_id: BELL_TEST_STEP, values: &values })
            .expect("bell step");
        assert_eq!(ok.status, InstallStepStatus::Ok);
        assert!(native_install_step(&ctx, &InstallStepRun { step_id: "gpu-nodes", values: &values }).is_err());
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
