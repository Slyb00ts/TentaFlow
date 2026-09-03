// =============================================================================
// File: features.rs
// Purpose: The wire shape of ONE environment-feature probe, shared by every app
//          that installs system dependencies on a node. `FeatureSpec` (the
//          declaration of what a feature needs per distribution) lives in the
//          core's `system/features.rs`; this is the RESULT of evaluating one
//          such spec on one host, and it is identical for storage (TentaNas)
//          and virtualization (TentaVM) because both drive the same UI: a row
//          that says what is missing, which packages would fix it, and whether
//          the fix is optional. PLAN §8.2 puts the spec side in a shared
//          `system/features.rs` inside the core; that file does not exist yet
//          (today the only `FeatureSpec` is private to
//          `tentaflow-core/src/tentanas/environment.rs`), so this module is
//          the shared half that already does.
// Example: MessageBody::TentaVmBody(TentaVmPayload::HostProbeResponse {
//              host_id, environment: VmHostEnvironment { features, .. },
//          })
// =============================================================================

use serde::{Deserialize, Serialize};

/// One feature probe of an environment screen. `id` is the key of the app's
/// own `FeatureSpec` table ('zfs', 'smb' … for storage; 'kvm_base',
/// 'podman_rootless' … for virtualization). `status` is 'ok' | 'missing' (a
/// binary is absent) | 'outdated' (below `required_version`) |
/// 'missing_module' (the kernel side is absent) | 'no_device' (no hardware —
/// the RDMA and VT-x rows). `packages` are what "Install" would pass to the
/// package manager — shown verbatim before anything runs. `optional` marks a
/// feature whose absence degrades the app instead of blocking it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FeatureState {
    pub id: String,
    pub status: String,
    pub version: Option<String>,
    pub required_version: Option<String>,
    pub binaries: Vec<String>,
    pub kernel_module: Option<String>,
    pub packages: Vec<String>,
    pub detail: String,
    pub optional: bool,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use crate::wire_pin::{self, name_digest};

    /// This very file. `FeatureState` rides in TWO payloads — TentaNas's
    /// `NasEnvironment` and TentaVM's `VmHostEnvironment` — and lives in
    /// neither module, so neither module's pin can see it. This is where its
    /// nine field names and types are frozen; without it a rename here changes
    /// the wire of a shipped app and every round-trip test still passes,
    /// because both sides re-encode with the new name.
    const SOURCE: &str = include_str!("features.rs");

    #[test]
    fn features_source_is_parseable() {
        wire_pin::assert_parseable(SOURCE);
    }

    #[test]
    fn feature_state_fields_are_pinned() {
        let structs = wire_pin::wire_structs(SOURCE);
        let names: Vec<String> = structs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            names,
            vec!["FeatureState".to_string()],
            "this module holds exactly one wire struct; a second one needs its own pin below"
        );

        let fields = &structs[0].1;
        assert_eq!(
            fields.len(),
            9,
            "'FeatureState' field COUNT changed — and it changed for TentaNas too. Live              fields:\n{}",
            fields.join("\n")
        );
        assert_eq!(
            name_digest(fields),
            0xf58d_2b78_650d_f926,
            "'FeatureState' field NAMES, TYPES, ORDER or serde attributes changed. Both              TentaNas and TentaVM decode these by name in the browser, and no round-trip test              can see the break because it re-encodes with the new declaration. Live              fields:\n{}",
            fields.join("\n")
        );
    }
}
