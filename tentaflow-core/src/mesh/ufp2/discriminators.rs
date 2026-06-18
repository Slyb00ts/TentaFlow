// =============================================================================
// File: mesh/ufp2/discriminators.rs — internal mesh dispatch to UFP/2 mapping
// Purpose: map the internal 1-byte mesh dispatch ids to their UFP/2
// `(channel, kind)`. Steady-state mesh frames stay on `channel=0x04 Mesh`;
// sync frames use `channel=0x06 SyncLedger` while still returning the
// internal dispatch id to the existing runtime.
// =============================================================================

use tentaflow_protocol::mesh as legacy;
use tentaflow_sdk_spec::protocol::frame::channel::{channels, Channel, Kind};

/// Convert a legacy MESH_MSG_* u8 discriminator into the matching UFP/2
/// `Kind` value. Mesh-channel kinds keep `low_byte == legacy discriminator`;
/// sync-channel kinds use the compact SyncLedger catalog ids 1..6.
pub const fn kind_from_legacy(disc: u8) -> Kind {
    match disc {
        legacy::MESH_MSG_SYNC_PUSH => sync_kinds::PUSH,
        legacy::MESH_MSG_SYNC_ACK => sync_kinds::ACK,
        legacy::MESH_MSG_SYNC_PULL => sync_kinds::PULL,
        legacy::MESH_MSG_SYNC_PULL_RESPONSE => sync_kinds::PULL_RESPONSE,
        legacy::MESH_MSG_SYNC_SNAPSHOT_PULL => sync_kinds::SNAPSHOT_PULL,
        legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE => sync_kinds::SNAPSHOT_RESPONSE,
        _ => Kind(disc as u16),
    }
}

pub const fn channel_from_legacy(disc: u8) -> Channel {
    match disc {
        legacy::MESH_MSG_SYNC_PUSH
        | legacy::MESH_MSG_SYNC_ACK
        | legacy::MESH_MSG_SYNC_PULL
        | legacy::MESH_MSG_SYNC_PULL_RESPONSE
        | legacy::MESH_MSG_SYNC_SNAPSHOT_PULL
        | legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE => channels::SYNC_LEDGER,
        _ => channels::MESH,
    }
}

/// Convert a UFP/2 `Kind` on the Mesh channel back to its legacy u8
/// discriminator. Returns `None` if `kind.0` does not correspond to a
/// currently-migrated `MESH_MSG_*` constant. We refuse to map kinds for
/// types not yet on the UFP/2 path so a misbehaving sender cannot smuggle
/// frames through the legacy dispatch table by claiming a discriminator
/// for an unmigrated message type — the legacy wire is the only valid
/// path for those until their 4c2.x migration commit.
pub fn legacy_from_kind(kind: Kind) -> Option<u8> {
    legacy_from_channel_kind(channels::MESH, kind)
}

pub fn legacy_from_channel_kind(channel: Channel, kind: Kind) -> Option<u8> {
    if channel == channels::SYNC_LEDGER {
        return match kind {
            sync_kinds::PUSH => Some(legacy::MESH_MSG_SYNC_PUSH),
            sync_kinds::ACK => Some(legacy::MESH_MSG_SYNC_ACK),
            sync_kinds::PULL => Some(legacy::MESH_MSG_SYNC_PULL),
            sync_kinds::PULL_RESPONSE => Some(legacy::MESH_MSG_SYNC_PULL_RESPONSE),
            sync_kinds::SNAPSHOT_PULL => Some(legacy::MESH_MSG_SYNC_SNAPSHOT_PULL),
            sync_kinds::SNAPSHOT_RESPONSE => Some(legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE),
            _ => None,
        };
    }
    if channel != channels::MESH {
        return None;
    }
    if kind.0 > u8::MAX as u16 {
        return None;
    }
    let disc = kind.0 as u8;
    if is_migrated_to_ufp2_discriminator(disc) {
        Some(disc)
    } else {
        None
    }
}

pub mod sync_kinds {
    use super::Kind;

    pub const PUSH: Kind = Kind(0x0001);
    pub const ACK: Kind = Kind(0x0002);
    pub const PULL: Kind = Kind(0x0003);
    pub const PULL_RESPONSE: Kind = Kind(0x0004);
    pub const SNAPSHOT_PULL: Kind = Kind(0x0005);
    pub const SNAPSHOT_RESPONSE: Kind = Kind(0x0006);
}

/// True iff `disc` is a `MESH_MSG_*` discriminator whose send path has
/// already been migrated to UFP/2 in some 4c2.x chunk. `send_ufp2_to_peer`
/// uses this as a gate so that pre-migration types (which still go through
/// the legacy `[disc][CBOR]` wire) are not accidentally double-routed
/// through the UFP/2 path. Each 4c2.x chunk that migrates a new type
/// extends this list AND the test below.
///
/// Currently migrated:
/// - 4c2.1: HEARTBEAT
/// - 4c2.2: PAIRING_REQUEST, PAIRING_CONFIRM, PAIRING_REJECT,
///   TRUST_REVOKED, TRUSTED_KEYS_SYNC
/// - 4c2.3: COMMAND, COMMAND_RESPONSE, DEPLOY_PROGRESS, LOG_CHUNK,
///   SERVICES_GET, SERVICES_GET_RESPONSE, SERVICES_ANNOUNCE, SERVICES_UPDATE
/// - 4c2.4: HMAC_KEYS_SYNC, FRAME_PROXY_REQUEST, FRAME_PROXY_RESPONSE
/// - 4c2.5: SYNC_PUSH, SYNC_ACK, SYNC_PULL, SYNC_PULL_RESPONSE,
///   SYNC_SNAPSHOT_PULL, SYNC_SNAPSHOT_RESPONSE
/// - 4c2.6b: NODE_INFO, HELLO, TOPOLOGY_ANNOUNCE, KNOWN_PEERS,
///   ALIAS_SYNC, MODEL_LIST, NODE_LEAVING
/// - ROUTING_SYNC (broadcast konfiguracji routingu — klastry + czlonkowie)
pub fn is_migrated_to_ufp2_discriminator(disc: u8) -> bool {
    matches!(
        disc,
        legacy::MESH_MSG_HEARTBEAT
            | legacy::MESH_MSG_PAIRING_REQUEST
            | legacy::MESH_MSG_PAIRING_CONFIRM
            | legacy::MESH_MSG_PAIRING_REJECT
            | legacy::MESH_MSG_TRUST_REVOKED
            | legacy::MESH_MSG_TRUSTED_KEYS_SYNC
            | legacy::MESH_MSG_COMMAND
            | legacy::MESH_MSG_COMMAND_RESPONSE
            | legacy::MESH_MSG_DEPLOY_PROGRESS
            | legacy::MESH_MSG_LOG_CHUNK
            | legacy::MESH_MSG_SERVICES_GET
            | legacy::MESH_MSG_SERVICES_GET_RESPONSE
            | legacy::MESH_MSG_SERVICES_ANNOUNCE
            | legacy::MESH_MSG_SERVICES_UPDATE
            | legacy::MESH_MSG_HMAC_KEYS_SYNC
            | legacy::MESH_MSG_FRAME_PROXY_REQUEST
            | legacy::MESH_MSG_FRAME_PROXY_RESPONSE
            | legacy::MESH_MSG_STORAGE_PROXY_REQUEST
            | legacy::MESH_MSG_STORAGE_PROXY_RESPONSE
            | legacy::MESH_MSG_SYNC_PUSH
            | legacy::MESH_MSG_SYNC_ACK
            | legacy::MESH_MSG_SYNC_PULL
            | legacy::MESH_MSG_SYNC_PULL_RESPONSE
            | legacy::MESH_MSG_SYNC_SNAPSHOT_PULL
            | legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE
            | legacy::MESH_MSG_NODE_INFO
            | legacy::MESH_MSG_HELLO
            | legacy::MESH_MSG_TOPOLOGY_ANNOUNCE
            | legacy::MESH_MSG_KNOWN_PEERS
            | legacy::MESH_MSG_ALIAS_SYNC
            | legacy::MESH_MSG_MODEL_LIST
            | legacy::MESH_MSG_NODE_LEAVING
            | legacy::MESH_MSG_ROUTING_SYNC
            | legacy::MESH_MSG_ROBOTS_ANNOUNCE
            | legacy::MESH_MSG_ROBOTS_GET
            | legacy::MESH_MSG_ROBOTS_GET_RESPONSE
            | legacy::MESH_MSG_ROBOTS_UPDATE
    )
}

/// Named UFP/2 Kind constants on the Mesh channel. Mirrors every
/// `MESH_MSG_*` constant in `tentaflow_protocol::mesh`. Use these in new
/// code that targets the UFP/2 path; legacy code can keep using the u8
/// constants and pass them through `kind_from_legacy`.
pub mod kinds {
    use super::legacy;
    use super::Kind;

    pub const HEARTBEAT: Kind = Kind(legacy::MESH_MSG_HEARTBEAT as u16);
    pub const FORWARD_REQ: Kind = Kind(legacy::MESH_MSG_FORWARD_REQ as u16);
    pub const MODEL_LIST: Kind = Kind(legacy::MESH_MSG_MODEL_LIST as u16);
    pub const NODE_INFO: Kind = Kind(legacy::MESH_MSG_NODE_INFO as u16);
    pub const ALIAS_SYNC: Kind = Kind(legacy::MESH_MSG_ALIAS_SYNC as u16);
    pub const ROUTING_SYNC: Kind = Kind(legacy::MESH_MSG_ROUTING_SYNC as u16);
    pub const ROBOTS_ANNOUNCE: Kind = Kind(legacy::MESH_MSG_ROBOTS_ANNOUNCE as u16);
    pub const ROBOTS_GET: Kind = Kind(legacy::MESH_MSG_ROBOTS_GET as u16);
    pub const ROBOTS_GET_RESPONSE: Kind = Kind(legacy::MESH_MSG_ROBOTS_GET_RESPONSE as u16);
    pub const ROBOTS_UPDATE: Kind = Kind(legacy::MESH_MSG_ROBOTS_UPDATE as u16);
    pub const NODE_LEAVING: Kind = Kind(legacy::MESH_MSG_NODE_LEAVING as u16);
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
    pub const HMAC_KEYS_SYNC: Kind = Kind(legacy::MESH_MSG_HMAC_KEYS_SYNC as u16);
    pub const FRAME_PROXY_REQUEST: Kind = Kind(legacy::MESH_MSG_FRAME_PROXY_REQUEST as u16);
    pub const FRAME_PROXY_RESPONSE: Kind = Kind(legacy::MESH_MSG_FRAME_PROXY_RESPONSE as u16);
    pub const STORAGE_PROXY_REQUEST: Kind = Kind(legacy::MESH_MSG_STORAGE_PROXY_REQUEST as u16);
    pub const STORAGE_PROXY_RESPONSE: Kind = Kind(legacy::MESH_MSG_STORAGE_PROXY_RESPONSE as u16);
    pub const SYNC_PUSH: Kind = super::sync_kinds::PUSH;
    pub const SYNC_ACK: Kind = super::sync_kinds::ACK;
    pub const SYNC_PULL: Kind = super::sync_kinds::PULL;
    pub const SYNC_PULL_RESPONSE: Kind = super::sync_kinds::PULL_RESPONSE;
    pub const SYNC_SNAPSHOT_PULL: Kind = super::sync_kinds::SNAPSHOT_PULL;
    pub const SYNC_SNAPSHOT_RESPONSE: Kind = super::sync_kinds::SNAPSHOT_RESPONSE;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip_via_legacy_for_migrated_types() {
        for disc in [
            legacy::MESH_MSG_HEARTBEAT,
            legacy::MESH_MSG_PAIRING_REQUEST,
            legacy::MESH_MSG_PAIRING_CONFIRM,
            legacy::MESH_MSG_PAIRING_REJECT,
            legacy::MESH_MSG_TRUST_REVOKED,
            legacy::MESH_MSG_TRUSTED_KEYS_SYNC,
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
    fn legacy_from_kind_rejects_unmigrated_discriminators() {
        // Bi-stream protocol discriminators (FORWARD_REQ, FORWARD_STREAM_REQ)
        // never travel as UFP/2 unicast envelopes — sender claiming them on
        // the UFP/2 path MUST be rejected.
        assert!(legacy_from_kind(Kind(legacy::MESH_MSG_FORWARD_REQ as u16)).is_none());
        assert!(legacy_from_kind(Kind(legacy::MESH_MSG_FORWARD_STREAM_REQ as u16)).is_none());
        // Camera live relay subscribe is also a bi-stream-only discriminator.
        assert!(legacy_from_kind(Kind(legacy::MESH_MSG_CAMERA_STREAM_SUBSCRIBE as u16)).is_none());
        // Holes in the 0x10..=0x4C range — not even legacy.
        assert!(legacy_from_kind(Kind(0x0017)).is_none());
        assert!(legacy_from_kind(Kind(0x0036)).is_none());
    }

    #[test]
    fn migrated_discriminator_allowlist_matches_chunks() {
        // 4c2.1 + 4c2.2 + 4c2.3 + 4c2.4 + 4c2.5 + 4c2.6b plus
        // storage proxy types are migrated.
        // types. When a new 4c2.x chunk lands, add its types here AND to
        // `is_migrated_to_ufp2_discriminator`.
        let migrated = [
            legacy::MESH_MSG_HEARTBEAT,
            legacy::MESH_MSG_PAIRING_REQUEST,
            legacy::MESH_MSG_PAIRING_CONFIRM,
            legacy::MESH_MSG_PAIRING_REJECT,
            legacy::MESH_MSG_TRUST_REVOKED,
            legacy::MESH_MSG_TRUSTED_KEYS_SYNC,
            legacy::MESH_MSG_COMMAND,
            legacy::MESH_MSG_COMMAND_RESPONSE,
            legacy::MESH_MSG_DEPLOY_PROGRESS,
            legacy::MESH_MSG_LOG_CHUNK,
            legacy::MESH_MSG_SERVICES_GET,
            legacy::MESH_MSG_SERVICES_GET_RESPONSE,
            legacy::MESH_MSG_SERVICES_ANNOUNCE,
            legacy::MESH_MSG_SERVICES_UPDATE,
            legacy::MESH_MSG_HMAC_KEYS_SYNC,
            legacy::MESH_MSG_FRAME_PROXY_REQUEST,
            legacy::MESH_MSG_FRAME_PROXY_RESPONSE,
            legacy::MESH_MSG_STORAGE_PROXY_REQUEST,
            legacy::MESH_MSG_STORAGE_PROXY_RESPONSE,
            legacy::MESH_MSG_SYNC_PUSH,
            legacy::MESH_MSG_SYNC_ACK,
            legacy::MESH_MSG_SYNC_PULL,
            legacy::MESH_MSG_SYNC_PULL_RESPONSE,
            legacy::MESH_MSG_SYNC_SNAPSHOT_PULL,
            legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
            legacy::MESH_MSG_NODE_INFO,
            legacy::MESH_MSG_HELLO,
            legacy::MESH_MSG_TOPOLOGY_ANNOUNCE,
            legacy::MESH_MSG_KNOWN_PEERS,
            legacy::MESH_MSG_ALIAS_SYNC,
            legacy::MESH_MSG_MODEL_LIST,
            legacy::MESH_MSG_NODE_LEAVING,
            legacy::MESH_MSG_ROUTING_SYNC,
            legacy::MESH_MSG_ROBOTS_ANNOUNCE,
            legacy::MESH_MSG_ROBOTS_GET,
            legacy::MESH_MSG_ROBOTS_GET_RESPONSE,
            legacy::MESH_MSG_ROBOTS_UPDATE,
        ];
        for d in migrated {
            assert!(
                is_migrated_to_ufp2_discriminator(d),
                "discriminator 0x{:02X} should be on the migrated allowlist",
                d
            );
        }
        // Remaining legacy-only mesh types (bi-stream protocol — not
        // migrated to UFP/2 unicast envelope).
        assert!(!is_migrated_to_ufp2_discriminator(
            legacy::MESH_MSG_FORWARD_REQ
        ));
        assert!(!is_migrated_to_ufp2_discriminator(
            legacy::MESH_MSG_FORWARD_STREAM_REQ
        ));
        assert!(!is_migrated_to_ufp2_discriminator(
            legacy::MESH_MSG_CAMERA_STREAM_SUBSCRIBE
        ));
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
            kinds::STORAGE_PROXY_REQUEST,
            kinds::STORAGE_PROXY_RESPONSE,
        ] {
            assert!(
                k.0 >= 0x0010 && k.0 <= 0x004C,
                "kind 0x{:04X} outside Mesh channel range 0x0010..=0x004C",
                k.0
            );
        }
    }

    #[test]
    fn sync_discriminators_use_sync_ledger_channel_kinds() {
        let cases = [
            (legacy::MESH_MSG_SYNC_PUSH, sync_kinds::PUSH),
            (legacy::MESH_MSG_SYNC_ACK, sync_kinds::ACK),
            (legacy::MESH_MSG_SYNC_PULL, sync_kinds::PULL),
            (
                legacy::MESH_MSG_SYNC_PULL_RESPONSE,
                sync_kinds::PULL_RESPONSE,
            ),
            (
                legacy::MESH_MSG_SYNC_SNAPSHOT_PULL,
                sync_kinds::SNAPSHOT_PULL,
            ),
            (
                legacy::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
                sync_kinds::SNAPSHOT_RESPONSE,
            ),
        ];

        for (legacy_discriminator, sync_kind) in cases {
            assert_eq!(
                channel_from_legacy(legacy_discriminator),
                channels::SYNC_LEDGER
            );
            assert_eq!(kind_from_legacy(legacy_discriminator), sync_kind);
            assert_eq!(
                legacy_from_channel_kind(channels::SYNC_LEDGER, sync_kind),
                Some(legacy_discriminator)
            );
            assert_eq!(legacy_from_channel_kind(channels::MESH, sync_kind), None);
        }
    }
}
