// =============================================================================
// File: environment.rs
// Purpose: Wire types for node environment identity (Dev/Test/Prod, ROADMAP
//          Z12) and the manual, explicitly-directed config-bundle pull that
//          replaces cross-environment sync. A node's environment is an
//          identity attribute (like its node_id), not a separate instance:
//          one mesh, but direct sync only ever happens between nodes that
//          declare the SAME environment (fenced in the sync envelope, the
//          ledger admission gate, the pairing handshake and the alias
//          resolver — see `sync::ledger`, `net::iroh::pairing`,
//          `services::runtime::resolver` in tentaflow-core). Moving
//          configuration ACROSS environments (e.g. Test -> Prod) is always a
//          deliberate, admin-initiated pull of a whitelisted config bundle,
//          never sync.
//
// Packed into a single `EnvironmentPromotionPayload` inner enum so the whole
// surface burns one `MessageBody` discriminant slot (same pack pattern as
// `events.rs` / `model_conversion.rs`).
//
// Append-only: new variants go at the END of `EnvironmentPromotionPayload`
// and new struct fields carry `#[serde(default)]`, so a peer that predates a
// field still decodes the message instead of failing the frame.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// A node's declared environment identity. Ordered `Dev < Test < Prod` — the
/// derived `Ord` follows declaration order, which `ImportApply`'s "promotion
/// upward" gate (D-Z12.8) and `SetKind`'s Prod confirmation (D-Z12.9) both
/// rely on to decide whether a change moves toward Prod.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, SerdeSerialize, SerdeDeserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum NodeEnvironment {
    Dev,
    Test,
    Prod,
}

impl NodeEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeEnvironment::Dev => "dev",
            NodeEnvironment::Test => "test",
            NodeEnvironment::Prod => "prod",
        }
    }

    /// Case-insensitive parse of the wire/settings string form. Anything
    /// unrecognized returns `None` — callers fail closed rather than
    /// guessing a default (the DEFAULT for a MISSING value, as opposed to an
    /// invalid one, is `NodeEnvironment::default()` = `Prod`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dev" => Some(NodeEnvironment::Dev),
            "test" => Some(NodeEnvironment::Test),
            "prod" => Some(NodeEnvironment::Prod),
            _ => None,
        }
    }
}

// A node that has never declared an environment (pre-Z12 install, or a peer
// trust-paired before the `trusted_nodes.environment` column existed) is
// conservatively treated as `Prod` — the tightest, safest default: it can
// never accidentally gain automatic sync access to a Dev/Test node's data,
// and a genuinely-Prod node upgrading in place keeps behaving as Prod without
// any migration step. Mirrors the `settings.node_environment` default
// (ZADANIA.md Z12 step 2).
impl Default for NodeEnvironment {
    fn default() -> Self {
        NodeEnvironment::Prod
    }
}

impl std::fmt::Display for NodeEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// -----------------------------------------------------------------------------
// GetKind / SetKind — the local node's own environment identity.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentGetKindRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentGetKindResponse {
    pub kind: NodeEnvironment,
    pub isolation_strict: bool,
}

/// Switches the local node's declared environment. Always a two-step UX on
/// the client (confirmation modal first, D-Z12.9) but the ONLY gate that
/// actually matters is server-side: `confirm_environment_name` MUST equal
/// exactly `"PROD"` when `new_kind == Prod`, checked in the handler, not
/// merely by a disabled button in the UI (pitfall #8).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentSetKindRequest {
    pub new_kind: NodeEnvironment,
    #[serde(default)]
    pub confirm_environment_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentSetKindResponse {
    pub kind: NodeEnvironment,
    /// Number of core operations re-seeded into the outbox under the freshly
    /// bumped epoch (same wipe+reseed the sync runtime performs for a core
    /// baseline reset) — surfaced so the UI can show it happened, not just
    /// that the setting flipped.
    pub reseeded_operations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentSetStrictIsolationRequest {
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentSetStrictIsolationResponse {
    pub strict: bool,
}

// -----------------------------------------------------------------------------
// Config bundle — whitelist-only, never secrets, never clinical data.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentBundleTableCount {
    pub table: String,
    pub row_count: u64,
}

/// File-transport export of the LOCAL node's own current config bundle
/// (`services::config_bundle::export_bundle`). The same bytes are also what a
/// donor returns over the QUIC pull path (`MeshCommandType::ConfigBundleExport`)
/// — one archive format, two transports (D-Z12.4).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentExportBundleRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentExportBundleResponse {
    pub filename: String,
    pub archive_bytes: Vec<u8>,
    pub manifest_sha256: String,
    pub source_environment: NodeEnvironment,
    pub table_counts: Vec<EnvironmentBundleTableCount>,
}

/// A previously-exported bundle handed back to the SAME node for a file-based
/// import preview/apply — the requester never uploads raw table rows blindly,
/// it goes through the same preview/apply pair as the QUIC pull.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentImportFromFileRequest {
    pub archive_bytes: Vec<u8>,
}

// -----------------------------------------------------------------------------
// QUIC pull — donor select -> start -> poll, mirrors `mesh-baseline-adopt.js`
// (`PHASE_ORDER`: Elected -> Receiving -> Importing -> Imported -> Completed).
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullDonorListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullDonorInfo {
    pub node_id: String,
    pub hostname: String,
    pub environment: NodeEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullDonorListResponse {
    pub donors: Vec<EnvironmentPullDonorInfo>,
}

/// Fetches the donor's current bundle over QUIC (`MeshCommandType::
/// ConfigBundleExport`) and stores it as the pending pull, ready for
/// `ImportPreviewDiff`/`ImportApply`. Synchronous from the caller's point of
/// view (the bundle is small — flows/aliases/settings, not a whole baseline
/// snapshot) — `pull_id` is returned for `ImportPreviewDiff`/`ImportApply`
/// to reference the fetched bundle without re-fetching it.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullStartRequest {
    pub donor_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullStartResponse {
    pub pull_id: String,
    /// "receiving" | "imported" | "failed" — a successful fetch always lands
    /// on "imported" immediately (nothing is APPLIED to the DB yet; "imported"
    /// here means "the bundle bytes are held locally", matching the
    /// `mesh-baseline-adopt.js` phase vocabulary this wizard is modeled on).
    pub phase: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullStatusRequest {
    pub pull_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentPullStatusResponse {
    pub pull_id: String,
    pub phase: String,
    #[serde(default)]
    pub error: Option<String>,
}

// -----------------------------------------------------------------------------
// Diff + apply — the only two operations that actually touch the local DB.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentDiffEntry {
    pub table: String,
    pub resource_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentImportPreviewDiffRequest {
    pub pull_id: String,
}

/// Counts here are what the M05 warning modal's "N flows, M settings..." copy
/// is built from (D-Z12.8) — the UI must never compute its own count from a
/// different source than what `ImportApply` will actually act on.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentImportPreviewDiffResponse {
    pub pull_id: String,
    pub from_environment: NodeEnvironment,
    pub to_environment: NodeEnvironment,
    pub added: Vec<EnvironmentDiffEntry>,
    pub changed: Vec<EnvironmentDiffEntry>,
    pub skipped: Vec<EnvironmentDiffEntry>,
    pub flows_count: u32,
    pub settings_count: u32,
    pub aliases_count: u32,
}

/// Applies a previously-fetched pull. `confirm_environment_name` is REQUIRED
/// and validated SERVER-SIDE (fail-closed, pitfall #6) whenever
/// `to_environment` outranks `from_environment` (`Dev < Test < Prod`) — an
/// upward promotion, in particular anything landing on Prod. The UI shows the
/// warning modal with the numeric diff BEFORE this field (D-Z12.8), but that
/// is UX only; the server never trusts that the modal was shown.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentImportApplyRequest {
    pub pull_id: String,
    #[serde(default)]
    pub confirm_environment_name: Option<String>,
    /// `(table, resource_id)` pairs selected in the diff UI, encoded as
    /// `"table:resource_id"`. Unselected entries are skipped, never applied
    /// implicitly — there is no "select all by default" on the wire.
    #[serde(default)]
    pub selected_resource_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct EnvironmentImportApplyResponse {
    pub applied: bool,
    pub imported_count: u32,
}

/// The whole Z12 environment/config-bundle surface behind one
/// `MessageBody::EnvironmentPromotionBody`.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum EnvironmentPromotionPayload {
    GetKindRequest(EnvironmentGetKindRequest),
    GetKindResponse(EnvironmentGetKindResponse),
    SetKindRequest(EnvironmentSetKindRequest),
    SetKindResponse(EnvironmentSetKindResponse),
    SetStrictIsolationRequest(EnvironmentSetStrictIsolationRequest),
    SetStrictIsolationResponse(EnvironmentSetStrictIsolationResponse),
    ExportBundleRequest(EnvironmentExportBundleRequest),
    ExportBundleResponse(EnvironmentExportBundleResponse),
    ImportFromFileRequest(EnvironmentImportFromFileRequest),
    PullDonorListRequest(EnvironmentPullDonorListRequest),
    PullDonorListResponse(EnvironmentPullDonorListResponse),
    PullStartRequest(EnvironmentPullStartRequest),
    PullStartResponse(EnvironmentPullStartResponse),
    PullStatusRequest(EnvironmentPullStatusRequest),
    PullStatusResponse(EnvironmentPullStatusResponse),
    ImportPreviewDiffRequest(EnvironmentImportPreviewDiffRequest),
    ImportPreviewDiffResponse(EnvironmentImportPreviewDiffResponse),
    ImportApplyRequest(EnvironmentImportApplyRequest),
    ImportApplyResponse(EnvironmentImportApplyResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn node_environment_rank_is_dev_test_prod() {
        assert!(NodeEnvironment::Dev < NodeEnvironment::Test);
        assert!(NodeEnvironment::Test < NodeEnvironment::Prod);
    }

    #[test]
    fn node_environment_parse_is_case_insensitive() {
        assert_eq!(NodeEnvironment::parse("PROD"), Some(NodeEnvironment::Prod));
        assert_eq!(
            NodeEnvironment::parse("  test "),
            Some(NodeEnvironment::Test)
        );
        assert_eq!(NodeEnvironment::parse("staging"), None);
    }

    #[test]
    fn node_environment_default_is_prod() {
        assert_eq!(NodeEnvironment::default(), NodeEnvironment::Prod);
    }

    #[test]
    fn set_kind_roundtrip() {
        let req = MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::SetKindRequest(EnvironmentSetKindRequest {
                new_kind: NodeEnvironment::Prod,
                confirm_environment_name: Some("PROD".to_string()),
            }),
        );
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            req
        );
    }

    #[test]
    fn import_apply_decodes_without_defaulted_fields() {
        let bare = serde_json::json!({"pull_id": "p1"});
        let bytes = crate::cbor::encode(&bare).expect("encode");
        let decoded: EnvironmentImportApplyRequest = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded.pull_id, "p1");
        assert!(decoded.confirm_environment_name.is_none());
        assert!(decoded.selected_resource_keys.is_empty());
    }

    /// Ciborium tags an externally-tagged enum by variant NAME, not by index —
    /// appending a variant is safe, renaming one is not.
    #[test]
    fn message_body_is_tagged_by_variant_name() {
        let body = MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::GetKindRequest(EnvironmentGetKindRequest {}),
        );
        let bytes = crate::cbor::encode(&body).expect("encode");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("EnvironmentPromotionBody"),
            "outer variant name must be on the wire"
        );
        assert!(
            text.contains("GetKindRequest"),
            "inner variant name must be on the wire"
        );
    }
}
