// =============================================================================
// File: vision/settings.rs — process-wide vision runtime settings
// =============================================================================
//
// The `[vision]` section of the config TOML is the ONLY operator mechanism for
// tuning the vision/camera-CV runtime — there are deliberately NO environment
// variables. The parsed `VisionConfig` is frozen here once per process:
//   * the core initializes it from `NodeConfig` right after the config loads,
//   * a `vision-worker` process initializes it from the serialized
//     `--vision-config` CLI argument its supervisor passes,
//   * benches/examples initialize it from their own CLI flags.
// Code that runs before/without an explicit `init` (unit tests, examples that
// don't tune anything) reads the built-in defaults, which are byte-identical
// to the historical env-unset behavior.

use std::sync::OnceLock;

use crate::config::VisionConfig;

static SETTINGS: OnceLock<VisionConfig> = OnceLock::new();

/// Freezes the process-wide vision settings. Must run before ANY vision
/// singleton reads them; a second call (or a call after `get` already froze
/// the defaults) fails loudly instead of silently keeping stale values.
pub fn init(cfg: VisionConfig) -> anyhow::Result<()> {
    SETTINGS.set(cfg).map_err(|_| {
        anyhow::anyhow!(
            "vision settings already frozen — vision::settings::init must run before any \
             vision component reads them"
        )
    })
}

/// The process-wide vision settings. Falls back to (and freezes) the built-in
/// defaults when `init` never ran, so unit tests and examples work without a
/// config file.
pub fn get() -> &'static VisionConfig {
    SETTINGS.get_or_init(VisionConfig::default)
}
