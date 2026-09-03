// =============================================================================
// File: addon/native_apps.rs — registry of NATIVE core applications
//       (app-platform). A native app is compiled into core and reuses the
//       whole addon platform (catalog, instances, permission matrix); this
//       registry supplies the per-app lifecycle hooks the WASM runtime cannot.
// =============================================================================

use std::path::PathBuf;

use anyhow::Result;

/// Instance-scoped context handed to lifecycle hooks. `data_dir` is the
/// instance's own directory (orgs/<org>/addons/<addon_id>/) — the same
/// containment every addon instance gets. `db` is the MAIN database (platform
/// layer); the app's own content database comes from `app_db::open`.
pub struct NativeAppContext<'a> {
    pub db: &'a crate::db::DbPool,
    pub addon_id: &'a str,
    pub org_id: &'a str,
    pub data_dir: PathBuf,
}

/// Key prefix of the per-node reconcile status rows in `addon_config`
/// (double-underscore namespace, same convention as `__vector_config`). The
/// rows replicate with the instance's config partition — one key per node, so
/// there are no LWW collisions — and instance uninstall purges them with the
/// rest of the scoped tables.
pub const NODE_STATUS_KEY_PREFIX: &str = "__node_status/";

/// Records THIS node's reconcile outcome for a native instance
/// ("ready" | "unsupported" | "init_error"). Best-effort: a status write must
/// never fail the reconcile that produced it.
pub fn record_node_status(db: &crate::db::DbPool, addon_id: &str, status: &str, detail: &str) {
    let node_id =
        crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    let value = serde_json::json!({ "status": status, "detail": detail }).to_string();
    if let Err(e) = crate::db::repository::upsert_addon_config_value(
        db,
        addon_id,
        &format!("{NODE_STATUS_KEY_PREFIX}{node_id}"),
        &value,
        false,
        None,
    ) {
        tracing::warn!("native app '{addon_id}': node status write failed: {e}");
    }
}

/// One entry of the teardown manifest: what uninstall is about to remove (or
/// consciously leave behind). Surfaced in the uninstall dialog and audit log.
pub struct TeardownEntry {
    pub path: PathBuf,
    /// Stable id the dashboard localizes (`addon_uninstall.entries.<kind>`).
    pub kind: &'static str,
    /// Static English description for logs and the audit trail.
    pub description: &'static str,
    /// false = listed as "consciously left behind" instead of deleted.
    pub removed: bool,
}

/// Lifecycle hooks a native app plugs into the platform. All run on the
/// local node; the fleet-wide fan-out happens through sync reconcile.
pub struct NativeAppHooks {
    pub package_id: &'static str,
    /// Prepare instance state (data dir exists when called; create the app's
    /// own database/schema here). Must be idempotent — reconcile re-runs it.
    pub init: fn(&NativeAppContext) -> Result<()>,
    /// Instance was enabled (admin toggle or replicated enable). `None` = the
    /// app has nothing to start beyond what `init` already did. Must be
    /// idempotent; `init` runs first and may be the whole implementation.
    pub on_enable: Option<fn(&NativeAppContext) -> Result<()>>,
    /// Instance was disabled. `None` = nothing to stop (`disable_semantics`
    /// and `background_on_disable` describe the intent; this executes it).
    pub on_disable: Option<fn(&NativeAppContext)>,
    /// Enumerate app state for the uninstall dialog: what the wipe removes and
    /// what it consciously leaves behind. Must be side-effect free — the
    /// dialog calls it on every open, long before the admin confirms.
    pub teardown_plan: fn(&NativeAppContext) -> Result<Vec<TeardownEntry>>,
    /// App-specific cleanup OUTSIDE the data dir, run right before the platform
    /// removes the data dir. Must not touch user/content data other apps own.
    pub teardown: fn(&NativeAppContext) -> Result<()>,
}

/// Every native app compiled into this binary. Grows with plan-01 P2
/// (Studios retrofit) and new native apps (TentaNas, Chat).
static REGISTRY: &[NativeAppHooks] = &[
    NativeAppHooks {
        package_id: "benchmark-studio",
        init: benchmark_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    },
    NativeAppHooks {
        package_id: "ml-studio",
        init: ml_studio_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    },
    NativeAppHooks {
        package_id: "projekty",
        init: projekty_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    },
    NativeAppHooks {
        package_id: "code-studio",
        init: code_studio_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    },
    NativeAppHooks {
        package_id: "meeting-bot",
        init: meeting_bot_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    },
    NativeAppHooks {
        package_id: crate::tentanas::PACKAGE_ID,
        init: crate::tentanas::native_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: crate::tentanas::native_teardown_plan,
        teardown: crate::tentanas::native_teardown,
    },
    NativeAppHooks {
        package_id: crate::bus::native::PACKAGE_ID,
        init: crate::bus::native::native_init,
        on_enable: Some(crate::bus::native::native_on_enable),
        on_disable: Some(crate::bus::native::native_on_disable),
        teardown_plan: crate::bus::native::native_teardown_plan,
        teardown: crate::bus::native::native_teardown,
    },
];

/// Plan for apps whose whole instance state lives in the data dir (their own
/// database included — `app_db::close` runs before the wipe).
fn data_dir_only_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "data_dir",
        description: "instance data directory",
        removed: true,
    }])
}

/// Teardown for apps that keep nothing outside the data dir.
fn no_external_state(_ctx: &NativeAppContext) -> Result<()> {
    Ok(())
}

/// Hooks for a package id, or None for WASM packages.
pub fn hooks_for(package_id: &str) -> Option<&'static NativeAppHooks> {
    #[cfg(any(test, feature = "test-support"))]
    if package_id == test_support::PACKAGE_ID {
        return Some(&test_support::HOOKS);
    }
    REGISTRY.iter().find(|h| h.package_id == package_id)
}

/// Runs the enable/disable hook for a native instance on THIS node.
/// Best-effort and logged: a hook failure must not fail the toggle (the DB
/// flag is the truth; the gate already refuses requests either way).
///
/// Idempotence contract: this fires on EVERY reconcile of a synced instance
/// (`addon::AddonManager::reconcile_synced_addon`), not only on a real
/// enabled ↔ disabled transition — a node catching up on a replicated
/// install/update has no "previous state" of its own to diff against, only
/// the current `is_enabled` flag. The dashboard toggle handler, by contrast,
/// calls this once, only when `set_addon_enabled` actually flips the flag.
/// `on_enable`/`on_disable` hooks MUST therefore be safe to call repeatedly
/// for the same state (already required on `NativeAppHooks` itself; this is
/// the call site that exercises it).
pub fn notify_enabled(
    db: &crate::db::DbPool,
    addon_id: &str,
    package_id: &str,
    manifest: &crate::addon::AddonManifest,
    enabled: bool,
) {
    if !manifest.is_native() {
        return;
    }
    // Cheap insurance for the "nothing may ever mix" invariant: refuse to run
    // a hook against a data dir/instance that is not actually an instance of
    // the named package (a caller passing a mismatched pair would otherwise
    // silently start/stop the wrong app's state).
    match crate::db::repository::get_instance_of_package(db, package_id, addon_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::warn!(
                "native app '{addon_id}': not an instance of package '{package_id}' — \
                 refusing enable-notify"
            );
            return;
        }
        Err(e) => {
            tracing::warn!("native app '{addon_id}': membership lookup failed: {e}");
            return;
        }
    }
    let Some(hooks) = hooks_for(package_id) else {
        return;
    };
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    if enabled {
        let data_dir = match crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("native app '{addon_id}': enable-notify data dir: {e:?}");
                return;
            }
        };
        if let Some(on_enable) = hooks.on_enable {
            let ctx = NativeAppContext {
                db,
                addon_id,
                org_id,
                data_dir,
            };
            if let Err(e) = on_enable(&ctx) {
                tracing::warn!("native app '{addon_id}': on_enable hook failed: {e}");
            }
        }
    } else if let Some(on_disable) = hooks.on_disable {
        // Non-creating resolver: a disable notification must never resurrect
        // a data dir an uninstall already removed (or one an in-flight
        // install has not created yet) — `addon_data_dir` would create it.
        let data_dir = match crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, addon_id) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("native app '{addon_id}': disable-notify data dir: {e:?}");
                return;
            }
        };
        let ctx = NativeAppContext {
            db,
            addon_id,
            org_id,
            data_dir,
        };
        on_disable(&ctx);
    }
}

/// Generic fixture for platform tests: a native app entry whose enable/
/// disable hooks are observable, so a test can assert `notify_enabled`
/// actually reaches them without depending on any real app's hooks (the six
/// shipped registry entries have `on_enable`/`on_disable` set to `None`).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::{data_dir_only_plan, no_external_state, NativeAppContext, NativeAppHooks};
    use anyhow::Result;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Package id of the fixture — never a real shipped app.
    pub const PACKAGE_ID: &str = "test-hook-app";
    pub static ENABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static DISABLE_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// A manifest for `PACKAGE_ID`, parseable by `lifecycle::parse_manifest_toml`.
    /// `hooks_for` only recognizes `PACKAGE_ID` itself, so every fixture
    /// instance shares this package id; a test that needs several independent
    /// catalog rows (e.g. one singleton, one not) distinguishes them by
    /// `version` when calling `upsert_addon_package`, not by package id.
    pub fn fixture_manifest_toml(singleton: bool) -> String {
        format!(
            r#"[addon]
id = "{PACKAGE_ID}"
name = "Test Fixture App"
version = "1.0.0"
description = "Generic platform fixture for native-app tests."
category = "test"
author = "TentaFlow"
icon = "trend"
runtime = "native"
platforms = []

[application]
entry_panel = "main"
title = "Test Fixture App"
icon = "trend"
description = "Test fixture"
sort_order = 100

[native]
singleton = {singleton}
routes = ["{PACKAGE_ID}"]
db_file = "fixture.db"

[[permission]]
id = "test.read"
display_name = "Read"
description = "Read the test fixture resource."
risk = "low"
default = "allow"
"#
        )
    }

    fn init(_ctx: &NativeAppContext) -> Result<()> {
        Ok(())
    }

    fn on_enable(_ctx: &NativeAppContext) -> Result<()> {
        ENABLE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn on_disable(_ctx: &NativeAppContext) {
        DISABLE_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) static HOOKS: NativeAppHooks = NativeAppHooks {
        package_id: PACKAGE_ID,
        init,
        on_enable: Some(on_enable),
        on_disable: Some(on_disable),
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
    };
}

/// Recovers the package id from an instance id (`{package_id}-{8hex}`,
/// `unique_instance_id` in lifecycle.rs). Needed on the sync-remove path,
/// where the local `addons` row is already gone. None when the id does not
/// match the instance shape (e.g. a pre-split legacy row).
pub fn package_of_instance(addon_id: &str) -> Option<&str> {
    let (package, suffix) = addon_id.rsplit_once('-')?;
    if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_hexdigit()) && !package.is_empty() {
        Some(package)
    } else {
        None
    }
}

/// True when the manifest's `platforms` list covers the OS this node runs on
/// (empty list = all platforms). Values follow `std::env::consts::OS`
/// ("linux" / "macos" / "windows"), same convention the manifests use.
pub fn platform_supported(platforms: &[String]) -> bool {
    platforms.is_empty() || platforms.iter().any(|p| p == std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_of_instance_parses_instance_shape() {
        assert_eq!(
            package_of_instance("benchmark-studio-8a3f2c1d"),
            Some("benchmark-studio")
        );
        // Legacy pre-split rows (addon_id == package_id) are not instances.
        assert_eq!(package_of_instance("benchmark-studio"), None);
        assert_eq!(package_of_instance("x-12345678"), Some("x"));
        assert_eq!(package_of_instance("x-1234567z"), None);
        assert_eq!(package_of_instance("-12345678"), None);
    }

    /// Every registered app previews its wipe without side effects and lists
    /// its data dir as removed — the uninstall dialog relies on both.
    #[test]
    fn every_teardown_plan_lists_the_data_dir_and_leaves_it_untouched() {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        let db: crate::db::DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("content.db"), b"x").expect("seed file");
        for hooks in REGISTRY {
            let ctx = NativeAppContext {
                db: &db,
                addon_id: "plan-test-00000000",
                org_id: "default",
                data_dir: tmp.path().to_path_buf(),
            };
            let entries = (hooks.teardown_plan)(&ctx).expect(hooks.package_id);
            let data_dir = entries
                .iter()
                .find(|e| e.path == tmp.path())
                .unwrap_or_else(|| panic!("{}: data dir missing from plan", hooks.package_id));
            assert!(data_dir.removed, "{}: data dir must be removed", hooks.package_id);
            assert!(
                tmp.path().join("content.db").exists(),
                "{}: plan must not touch the data dir",
                hooks.package_id
            );
        }
    }

    #[test]
    fn platform_supported_matches_current_os_or_empty() {
        assert!(platform_supported(&[]));
        assert!(platform_supported(&[std::env::consts::OS.to_string()]));
        assert!(!platform_supported(&["solaris".to_string()]));
    }
}

// =============================================================================
// Benchmark Studio — definitions, runs and results live in the instance
// database declared by the manifest (`native.db_file`); the main DB holds only
// the platform layer. Teardown needs no extra step: the data dir wipe takes
// the file with it.
// =============================================================================

fn benchmark_init(ctx: &NativeAppContext) -> Result<()> {
    // Opening runs the schema migration, so the content db exists and is
    // current right after install/reconcile — the first request never pays
    // for it, and a migration failure surfaces as `init_error` node status
    // instead of a failing handler later.
    crate::addon::app_db::open(ctx.db, ctx.org_id, ctx.addon_id, crate::benchmark::db::migrate)?;
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// ML Studio — content (projects, datasets, models, runs) still lives in the
// main DB until the P2.1 retrofit moves it into the instance database
// declared by the manifest.
// =============================================================================

fn ml_studio_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Projekty — the project registry and per-project databases stay where they
// are until the P2.1-style content move; the hooks manage only the platform
// instance surface.
// =============================================================================

fn projekty_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Code Studio — the workspace registry and memberships stay in the main DB
// per plan §6 (they replicate); the node-local vault and provisioning saga
// state live in the instance's own `code_studio.db`; per-workspace runtime
// content lives with each workspace on its owner node.
// =============================================================================

fn code_studio_init(ctx: &NativeAppContext) -> Result<()> {
    // Opening the content DB here creates the file and applies its schema on
    // install; every later open is a registry hit. Idempotent by construction.
    crate::addon::app_db::open(
        ctx.db,
        ctx.org_id,
        ctx.addon_id,
        crate::code_studio::db::migrate,
    )?;
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Meeting Bot — transcripts and recording blobs stay where they are until the
// content move; full wipe of recording blobs waits on blob GC (research/04).
// =============================================================================

fn meeting_bot_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}
