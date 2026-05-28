// =============================================================================
// File: mesh/ufp2/receive.rs — UFP/2 receive dispatcher
// Purpose: classify incoming mesh frames as UFP/2 envelopes and reject
// any other shape. After 4c2.6c every unicast send path emits a signed
// UFP/2 envelope, so the legacy `[disc][CBOR-payload]` wire is no longer
// accepted on uni-streams (FORWARD_REQ / FORWARD_STREAM_REQ travel on a
// separate bi-stream protocol that does not flow through this path).
//
// Signature verification (`verify_envelope`) IS performed here because it
// is purely a function of the envelope + its embedded `auth.subject_id`.
// Higher-level policy (is this peer in the trust pool, replay LRU,
// permission epoch) remains the caller's responsibility.
// =============================================================================

use tentaflow_sdk_spec::protocol::frame::{
    envelope::NODE_ID_LEN,
    error::{FrameError, FrameErrorCode},
    sign::verify_envelope,
};

use super::codec::{decode_incoming, looks_like_ufp2_envelope_first_byte, DecodedMeshEnvelope};

/// Outcome of inspecting an incoming mesh stream's first byte and tail.
/// Only UFP/2 envelopes are accepted; any other shape returns a
/// `FrameError` from `classify_inbound`.
pub enum InboundMeshFrame {
    /// UFP/2 envelope: decoded, structurally validated, AND signature-
    /// verified against `auth.subject_id`. Caller still owns trust /
    /// replay / epoch checks against application state.
    Ufp2(DecodedMeshEnvelope),
}

/// Classify an arriving mesh stream. `first_byte` was peeled off the
/// stream by the caller; `tail` is everything after it.
///
/// Reassembles the complete envelope bytes (`[first_byte || tail]`),
/// CBOR-decodes them, runs the structural validator, and verifies the
/// Ed25519 signature. It additionally binds:
/// - `envelope.source.id == transport_peer_pubkey` — the signed source
///   MUST match the actual iroh peer that wrote the bytes. Prevents a
///   trusted peer from relaying or replaying another node's envelope.
/// - `envelope.destination.id == local_node_pubkey` — the signed
///   destination MUST be this node.
///
/// On any failure the caller receives a `FrameError` carrying the
/// appropriate §11 code so it can log + drop without further work.
pub fn classify_inbound(
    first_byte: u8,
    tail: Vec<u8>,
    transport_peer_pubkey: [u8; NODE_ID_LEN],
    local_node_pubkey: [u8; NODE_ID_LEN],
) -> Result<InboundMeshFrame, FrameError> {
    if !looks_like_ufp2_envelope_first_byte(first_byte) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            format!(
                "classify_inbound: first byte 0x{:02X} is not a UFP/2 envelope CBOR map header (0xAA..=0xB1); legacy `[disc][CBOR]` wire was removed in 4c2.6c",
                first_byte
            ),
        ));
    }
    let mut full = Vec::with_capacity(1 + tail.len());
    full.push(first_byte);
    full.extend_from_slice(&tail);
    let decoded = decode_incoming(&full)?;
    if decoded.envelope.source.id != transport_peer_pubkey {
        return Err(FrameError::new(
            FrameErrorCode::PermissionDenied,
            "classify_inbound: envelope.source.id does not match the transport peer's Ed25519 pubkey (relay/replay attempt)",
        ));
    }
    if decoded.envelope.destination.id != local_node_pubkey {
        return Err(FrameError::new(
            FrameErrorCode::PermissionDenied,
            "classify_inbound: envelope.destination.id does not match local node pubkey (unicast misroute)",
        ));
    }
    verify_envelope(&decoded.envelope)?;
    Ok(InboundMeshFrame::Ufp2(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand_core_06::OsRng;
    use tentaflow_sdk_spec::protocol::frame::envelope::NODE_ID_LEN;
    use tentaflow_sdk_spec::protocol::frame::sign::public_key_bytes;

    use crate::mesh::ufp2::send::build_signed_envelope_wire;

    fn fresh_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    #[test]
    fn classify_legacy_discriminator_first_byte_is_rejected() {
        // 4c2.6c removed the legacy `[disc][CBOR]` receive path. A raw
        // MESH_MSG_* first byte MUST now produce a BodyValidationFailed
        // FrameError instead of being routed.
        let r = classify_inbound(
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"raw-CBOR-bytes".to_vec(),
            [0xAAu8; NODE_ID_LEN],
            [0xBBu8; NODE_ID_LEN],
        );
        assert!(r.is_err());
    }

    #[test]
    fn classify_ufp2_envelope_returns_ufp2_branch_and_verifies_sig() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];
        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"hb-payload".to_vec(),
            5,
        )
        .unwrap();
        let first = wire[0];
        let tail = wire[1..].to_vec();
        let r = classify_inbound(first, tail, source, dest).unwrap();
        match r {
            InboundMeshFrame::Ufp2(decoded) => {
                assert_eq!(
                    decoded.legacy_discriminator,
                    tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT
                );
                assert_eq!(decoded.body, b"hb-payload");
            }
            _ => panic!("expected Ufp2 branch"),
        }
    }

    #[test]
    fn classify_ufp2_rejects_source_mismatch() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];
        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"hb".to_vec(),
            0,
        )
        .unwrap();
        // Transport peer is a DIFFERENT pubkey than what the envelope claims
        // — receiver MUST reject as PermissionDenied (relay attempt).
        let wrong_peer = [0xEEu8; NODE_ID_LEN];
        let r = classify_inbound(wire[0], wire[1..].to_vec(), wrong_peer, dest);
        assert!(r.is_err());
    }

    #[test]
    fn classify_ufp2_rejects_destination_mismatch() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];
        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"hb".to_vec(),
            0,
        )
        .unwrap();
        // Envelope was addressed to `dest` but the local node has a
        // different pubkey — receiver MUST reject (misroute).
        let wrong_local = [0xCCu8; NODE_ID_LEN];
        let r = classify_inbound(wire[0], wire[1..].to_vec(), source, wrong_local);
        assert!(r.is_err());
    }

    #[test]
    fn classify_ufp2_envelope_rejects_tampered_signature() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];
        let mut wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"x".to_vec(),
            0,
        )
        .unwrap();
        let off = wire.len() - 20;
        wire[off] ^= 0xFF;
        let r = classify_inbound(wire[0], wire[1..].to_vec(), source, dest);
        assert!(r.is_err());
    }

    #[test]
    fn classify_ufp2_carries_pairing_request_discriminator() {
        // 4c2.2: pairing types travel UFP/2 just like heartbeat. The
        // classifier extracts the legacy discriminator from the envelope's
        // `kind` field so existing dispatch + frame_policy::is_pre_trust_frame
        // continue to work unchanged.
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x33u8; NODE_ID_LEN];
        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REQUEST,
            b"pairing-request-body".to_vec(),
            0,
        )
        .unwrap();
        let r = classify_inbound(wire[0], wire[1..].to_vec(), source, dest).unwrap();
        match r {
            InboundMeshFrame::Ufp2(decoded) => {
                assert_eq!(
                    decoded.legacy_discriminator,
                    tentaflow_protocol::mesh::MESH_MSG_PAIRING_REQUEST
                );
                assert_eq!(decoded.body, b"pairing-request-body");
            }
            _ => panic!("expected Ufp2 branch"),
        }
    }

    #[test]
    fn classify_ufp2_carries_trusted_keys_sync_discriminator() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x44u8; NODE_ID_LEN];
        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
            b"trusted-keys-payload".to_vec(),
            0,
        )
        .unwrap();
        let r = classify_inbound(wire[0], wire[1..].to_vec(), source, dest).unwrap();
        match r {
            InboundMeshFrame::Ufp2(decoded) => {
                assert_eq!(
                    decoded.legacy_discriminator,
                    tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC
                );
                assert_eq!(decoded.body, b"trusted-keys-payload");
            }
            _ => panic!("expected Ufp2 branch"),
        }
    }

    #[test]
    fn classify_unknown_first_byte_returns_protocol_error() {
        let peer = [0u8; NODE_ID_LEN];
        let local = [1u8; NODE_ID_LEN];
        let r = classify_inbound(0x00, Vec::new(), peer, local);
        assert!(r.is_err());
        let r2 = classify_inbound(0xFF, Vec::new(), peer, local);
        assert!(r2.is_err());
    }

}
