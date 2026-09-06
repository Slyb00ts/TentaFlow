// =============================================================================
// File: features.rs
// Purpose: The wire shape of ONE environment-feature probe, shared by every app
//          that installs system dependencies on a node: the RESULT of
//          evaluating one `FeatureSpec` on one host. Identical for storage
//          (TentaNas) and virtualization (TentaVM) because both drive the same
//          UI — a row that says what is missing, which packages would fix it,
//          and whether the fix is optional.
//
//          The SPEC side (what a feature needs per distribution) has no shared
//          home yet. PLAN §8.2 puts it in a `system/features.rs` inside the
//          core; that file does not exist, and today's only `FeatureSpec` is
//          private to `tentaflow-core/src/tentanas/environment.rs`. This
//          module is the half that is already shared.
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
    /// PRESENTATIONAL PROSE, and knowingly not translated yet. The producer
    /// composes English sentences from a template — `tentanas/environment.rs`
    /// writes `format!("missing: {}", …)`, `format!("found {v}, need at least
    /// {req}")`, `format!("kernel module {m} not loaded")` — and the browser
    /// prints them verbatim. By the rule this module's siblings follow it
    /// should be an i18n key plus parameters, exactly like `VmText` in
    /// `tentavm.rs`. It is not, because this type is on the wire of a SHIPPED
    /// TentaNas screen: changing its shape changes that app's contract and its
    /// frontend at once, which is a decision for the TentaNas owner, not a
    /// side effect of adding virtualization. Until then: prose, one language,
    /// and said out loud rather than filed under "data".
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
        let names: Vec<String> = structs.iter().map(|item| item.name.clone()).collect();
        assert_eq!(
            names,
            vec!["FeatureState".to_string()],
            "this module holds exactly one wire struct; a second one needs its own pin below"
        );
        assert!(
            wire_pin::wire_enums(SOURCE).is_empty(),
            "this module gained a wire enum, which nothing here pins yet"
        );

        let item = &structs[0];
        let entries = item.entries();
        assert_eq!(
            item.members.len(),
            9,
            "'FeatureState' field COUNT changed — and it changed for TentaNas too. \
             Live entries:\n{}",
            entries.join("\n")
        );
        assert_eq!(
            name_digest(&entries),
            0xbf03_26b2_7f48_3cb8,
            "'FeatureState' field NAMES, TYPES, ORDER or a serde attribute changed — including \
             one written above the struct, which renames every key at once. Both TentaNas and \
             TentaVM decode these by name in the browser, and no round-trip test can see the \
             break because it re-encodes with the new declaration. Live entries:\n{}",
            entries.join("\n")
        );
    }
}
