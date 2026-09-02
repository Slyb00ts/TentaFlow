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
/// containment every addon instance gets.
pub struct NativeAppContext<'a> {
    pub addon_id: &'a str,
    pub org_id: &'a str,
    pub data_dir: PathBuf,
}

/// One entry of the teardown manifest: what uninstall is about to remove (or
/// consciously leave behind). Surfaced in the uninstall dialog and audit log.
pub struct TeardownEntry {
    pub path: PathBuf,
    /// Static English description; the dashboard localizes via i18n keys.
    pub description: &'static str,
    /// false = listed as "consciously left behind" instead of deleted.
    pub removed: bool,
}

/// Lifecycle hooks a native app plugs into the platform. Both run on the
/// local node; the fleet-wide fan-out happens through sync reconcile.
pub struct NativeAppHooks {
    pub package_id: &'static str,
    /// Prepare instance state (data dir exists when called; create the app's
    /// own database/schema here). Must be idempotent — reconcile re-runs it.
    pub init: fn(&NativeAppContext) -> Result<()>,
    /// Enumerate app state for the uninstall dialog and perform app-specific
    /// cleanup OUTSIDE the data dir (the platform removes the data dir itself
    /// afterwards). Must not touch user/content data other apps own.
    pub teardown: fn(&NativeAppContext) -> Result<Vec<TeardownEntry>>,
}

/// Every native app compiled into this binary. Grows with plan-01 P2
/// (Studios retrofit) and new native apps (TentaNas, Chat).
static REGISTRY: &[NativeAppHooks] = &[
    NativeAppHooks {
        package_id: "benchmark-studio",
        init: benchmark_init,
        teardown: benchmark_teardown,
    },
    NativeAppHooks {
        package_id: "ml-studio",
        init: ml_studio_init,
        teardown: ml_studio_teardown,
    },
];

/// Hooks for a package id, or None for WASM packages.
pub fn hooks_for(package_id: &str) -> Option<&'static NativeAppHooks> {
    REGISTRY.iter().find(|h| h.package_id == package_id)
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

    #[test]
    fn platform_supported_matches_current_os_or_empty() {
        assert!(platform_supported(&[]));
        assert!(platform_supported(&[std::env::consts::OS.to_string()]));
        assert!(!platform_supported(&["solaris".to_string()]));
    }
}

// =============================================================================
// Benchmark Studio (pilot) — content still lives in the main DB until the
// P2.1 retrofit moves it into the instance database declared by the manifest.
// =============================================================================

fn benchmark_init(ctx: &NativeAppContext) -> Result<()> {
    // The platform created the data dir; nothing else to prepare until the
    // benchmark tables move out of the main DB (plan-01 P2.1).
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

fn benchmark_teardown(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        description: "instance data directory",
        removed: true,
    }])
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

fn ml_studio_teardown(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        description: "instance data directory",
        removed: true,
    }])
}
