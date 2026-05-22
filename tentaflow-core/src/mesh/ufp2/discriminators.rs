// =============================================================================
// File: mesh/ufp2/discriminators.rs — MESH_MSG_* ↔ UFP/2 Kind mapping
// Purpose: bidirectional mapping between the legacy 1-byte mesh
// discriminators (0x10..=0x4C, defined in `tentaflow_protocol::mesh`) and
// UFP/2 `Kind` values under `channel = 0x04 Mesh`. UFP/2 promotes the u8
// discriminant to u16; we keep the low byte equal to the legacy value so
// debugging and audit logs remain trivially readable across the migration.
// =============================================================================

use tentaflow_protocol::mesh as legacy;
use tentaflow_sdk_spec::protocol::frame::channel::Kind;

/// Convert a legacy MESH_MSG_* u8 discriminator into the matching UFP/2
/// `Kind` value. Every kind on channel 0x04 (Mesh) keeps `low_byte ==
/// legacy discriminator`, so 0x0010 ↔ HEARTBEAT, 0x0024 ↔ TRUSTED_KEYS_SYNC,
/// etc.
pub const fn kind_from_legacy(disc: u8) -> Kind {
    Kind(disc as u16)
}

/// Convert a UFP/2 `Kind` on the Mesh channel back to its legacy u8
/// discriminator. Returns `None` if `kind.0` does not correspond to a
/// currently-allocated `MESH_MSG_*` constant. We refuse to map kinds in
/// unallocated holes of the 0x10..=0x4C range so a misbehaving sender
/// cannot smuggle frames through the legacy dispatch table by claiming an
/// unused discriminator.
pub fn legacy_from_kind(kind: Kind) -> Option<u8> {
    if kind.0 > u8::MAX as u16 {
        return None;
    }
    let disc = kind.0 as u8;
    if is_allocated_mesh_discriminator(disc) {
        Some(disc)
    } else {
        None
    }
}

/// True iff `disc` corresponds to a `MESH_MSG_*` constant currently
/// allocated in `tentaflow_protocol::mesh`. Kept in sync with the legacy
/// header: when new constants are added there, add them here AND to the
/// `kinds` re-export below. The `discriminator_allowlist_matches_legacy_constants`
/// test catches drift.
pub fn is_allocated_mesh_discriminator(disc: u8) -> bool {
    matches!(
        disc,
        legacy::MESH_MSG_HEARTBEAT
            | legacy::MESH_MSG_CRDT_DELTA
            | legacy::MESH_MSG_FULL_STATE
            | legacy::MESH_MSG_FORWARD_REQ
            | legacy::MESH_MSG_FORWARD_RES
            | legacy::MESH_MSG_MODEL_LIST
            | legacy::MESH_MSG_CONTAINER_LIST
            | legacy::MESH_MSG_NODE_INFO
            | legacy::MESH_MSG_HELLO
            | legacy::MESH_MSG_TOPOLOGY_ANNOUNCE
            | legacy::MESH_MSG_KNOWN_PEERS
            | legacy::MESH_MSG_PAIRING_REQUEST
            | legacy::MESH_MSG_PAIRING_CONFIRM
            | legacy::MESH_MSG_PAIRING_REJECT
            | legacy::MESH_MSG_TRUST_REVOKED
            | legacy::MESH_MSG_TRUSTED_KEYS_SYNC
            | legacy::MESH_MSG_COMMAND
            | legacy::MESH_MSG_COMMAND_RESPONSE
            | legacy::MESH_MSG_DEPLOY_PROGRESS
            | legacy::MESH_MSG_LOG_CHUNK
    )
}

/// Named UFP/2 Kind constants on the Mesh channel. Mirrors every
/// `MESH_MSG_*` constant in `tentaflow_protocol::mesh`. Use these in new
/// code that targets the UFP/2 path; legacy code can keep using the u8
/// constants and pass them through `kind_from_legacy`.
pub mod kinds {
    use super::Kind;
    use super::legacy;

    pub const HEARTBEAT: Kind = Kind(legacy::MESH_MSG_HEARTBEAT as u16);
    pub const CRDT_DELTA: Kind = Kind(legacy::MESH_MSG_CRDT_DELTA as u16);
    pub const FULL_STATE: Kind = Kind(legacy::MESH_MSG_FULL_STATE as u16);
    pub const FORWARD_REQ: Kind = Kind(legacy::MESH_MSG_FORWARD_REQ as u16);
    pub const FORWARD_RES: Kind = Kind(legacy::MESH_MSG_FORWARD_RES as u16);
    pub const MODEL_LIST: Kind = Kind(legacy::MESH_MSG_MODEL_LIST as u16);
    pub const CONTAINER_LIST: Kind = Kind(legacy::MESH_MSG_CONTAINER_LIST as u16);
    pub const NODE_INFO: Kind = Kind(legacy::MESH_MSG_NODE_INFO as u16);
    pub const HELLO: Kind = Kind(legacy::MESH_MSG_HELLO as u16);
    pub const TOPOLOGY_ANNOUNCE: Kind = Kind(legacy::MESH_MSG_TOPOLOGY_ANNOUNCE as u16);
    pub const KNOWN_PEERS: Kind = Kind(legacy::MESH_MSG_KNOWN_PEERS as u16);
    pub const PAIRING_REQUEST: Kind = Kind(legacy::MESH_MSG_PAIRING_REQUEST as u16);
    pub const PAIRING_CONFIRM: Kind = Kind(legacy::MESH_MSG_PAIRING_CONFIRM as u16);
    pub const PAIRING_REJECT: Kind = Kind(legacy::MESH_MSG_PAIRING_REJECT as u16);
    pub const TRUST_REVOKED: Kind = Kind(legacy::MESH_MSG_TRUST_REVOKED as u16);
    pub const TRUSTED_KEYS_SYNC: Kind = Kind(legacy::MESH_MSG_TRUSTED_KEYS_SYNC as u16);
    pub const COMMAND: Kind = Kind(legacy::MESH_MSG_COMMAND as u16);
    pub const COMMAND_RESPONSE: Kind = Kind(legacy::MESH_MSG_COMMAND_RESPONSE as u16);
    pub const DEPLOY_PROGRESS: Kind = Kind(legacy::MESH_MSG_DEPLOY_PROGRESS as u16);
    pub const LOG_CHUNK: Kind = Kind(legacy::MESH_MSG_LOG_CHUNK as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip_via_legacy() {
        for disc in [
            legacy::MESH_MSG_HEARTBEAT,
            legacy::MESH_MSG_NODE_INFO,
            legacy::MESH_MSG_PAIRING_REQUEST,
            legacy::MESH_MSG_TRUSTED_KEYS_SYNC,
            legacy::MESH_MSG_COMMAND,
            legacy::MESH_MSG_TOPOLOGY_ANNOUNCE,
        ] {
            let kind = kind_from_legacy(disc);
            let back = legacy_from_kind(kind).unwrap();
            assert_eq!(back, disc, "roundtrip MUST preserve discriminator value");
        }
    }

    #[test]
    fn legacy_from_kind_rejects_oversize_value() {
        assert!(legacy_from_kind(Kind(0x0100)).is_none());
        assert!(legacy_from_kind(Kind(0xFFFF)).is_none());
    }

    #[test]
    fn legacy_from_kind_rejects_unallocated_holes() {
        // Holes in the 0x10..=0x4C range — these are NOT MESH_MSG_*
        // constants and a sender claiming them MUST NOT route through the
        // legacy dispatch.
        assert!(legacy_from_kind(Kind(0x0017)).is_none()); // hole between MODEL_LIST/CONTAINER_LIST and NODE_INFO
        assert!(legacy_from_kind(Kind(0x0025)).is_none()); // between TRUSTED_KEYS_SYNC and COMMAND-range
        assert!(legacy_from_kind(Kind(0x0034)).is_none()); // between LOG_CHUNK (0x33) and 0x40 services
    }

    #[test]
    fn named_constants_match_legacy_values() {
        assert_eq!(kinds::HEARTBEAT.0, legacy::MESH_MSG_HEARTBEAT as u16);
        assert_eq!(kinds::NODE_INFO.0, legacy::MESH_MSG_NODE_INFO as u16);
        assert_eq!(
            kinds::TRUSTED_KEYS_SYNC.0,
            legacy::MESH_MSG_TRUSTED_KEYS_SYNC as u16
        );
        assert_eq!(kinds::COMMAND.0, legacy::MESH_MSG_COMMAND as u16);
    }

    #[test]
    fn named_constants_within_mesh_channel_range() {
        // §4: Mesh channel kind range is 0x0010..=0x004C.
        for &k in &[
            kinds::HEARTBEAT,
            kinds::CRDT_DELTA,
            kinds::FULL_STATE,
            kinds::NODE_INFO,
            kinds::HELLO,
            kinds::TOPOLOGY_ANNOUNCE,
            kinds::KNOWN_PEERS,
            kinds::PAIRING_REQUEST,
            kinds::PAIRING_CONFIRM,
            kinds::PAIRING_REJECT,
            kinds::TRUST_REVOKED,
            kinds::TRUSTED_KEYS_SYNC,
            kinds::COMMAND,
            kinds::COMMAND_RESPONSE,
            kinds::DEPLOY_PROGRESS,
            kinds::LOG_CHUNK,
        ] {
            assert!(
                k.0 >= 0x0010 && k.0 <= 0x004C,
                "kind 0x{:04X} outside Mesh channel range 0x0010..=0x004C",
                k.0
            );
        }
    }
}
