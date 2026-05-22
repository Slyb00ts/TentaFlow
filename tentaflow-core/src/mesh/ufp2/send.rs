// =============================================================================
// File: mesh/ufp2/send.rs — UFP/2 mesh sender path
// Purpose: turn `(destination_pubkey, MESH_MSG_*, body)` into signed UFP/2
// envelope wire bytes ready to feed into iroh's `send_to_peer`. Replaces
// the legacy `[discriminator || raw_body]` shape one message type at a
// time; existing send paths that have not migrated yet keep using
// `tentaflow_protocol::mesh::MESH_MSG_*` directly.
// =============================================================================

use ed25519_dalek::SigningKey;

use tentaflow_sdk_spec::protocol::frame::{
    envelope::NODE_ID_LEN, error::FrameError, sign::sign_envelope,
};

use super::codec::{build_envelope, encode_envelope};

/// Build, sign, and CBOR-encode a UFP/2 envelope carrying a mesh payload.
/// Returns the wire bytes ready for the iroh stream.
///
/// All four crypto inputs come from the caller's mesh security layer:
/// - `signing_key`: the node's Ed25519 private key (used to sign the envelope).
/// - `source_node_pubkey`: the corresponding public key. MUST match
///   `signing_key.verifying_key()` or `sign_envelope` returns
///   `BodyValidationFailed` — sender-side correctness check.
/// - `destination_node_pubkey`: the trusted peer's pubkey (looked up from
///   the iroh node id by callers).
/// - `epoch`: current policy_epoch for this org (§6.2 revocation flow).
pub fn build_signed_envelope_wire(
    signing_key: &SigningKey,
    source_node_pubkey: [u8; NODE_ID_LEN],
    destination_node_pubkey: [u8; NODE_ID_LEN],
    legacy_discriminator: u8,
    body: Vec<u8>,
    epoch: u32,
) -> Result<Vec<u8>, FrameError> {
    let mut envelope = build_envelope(
        source_node_pubkey,
        destination_node_pubkey,
        legacy_discriminator,
        body,
        epoch,
    );
    sign_envelope(&mut envelope, signing_key)?;
    encode_envelope(&envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core_06::OsRng;
    use tentaflow_sdk_spec::protocol::frame::{
        sign::{public_key_bytes, verify_envelope},
        validator::validate_envelope,
    };

    use crate::mesh::ufp2::codec::decode_incoming;

    fn fresh_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    #[test]
    fn signed_envelope_wire_roundtrips_through_decoder() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];

        let wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"hb-body-bytes".to_vec(),
            42,
        )
        .unwrap();

        let decoded = decode_incoming(&wire).unwrap();
        assert_eq!(
            decoded.legacy_discriminator,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT
        );
        assert_eq!(decoded.body, b"hb-body-bytes");
        // Signature verifies, structural invariants pass.
        verify_envelope(&decoded.envelope).unwrap();
        validate_envelope(&decoded.envelope).unwrap();
        assert_eq!(decoded.envelope.auth.epoch, Some(42));
    }

    #[test]
    fn signed_envelope_rejects_subject_id_mismatch() {
        let key = fresh_key();
        let other = fresh_key();
        let wrong_source = public_key_bytes(&other);
        let dest = [0x22u8; NODE_ID_LEN];
        // source pubkey passed in does NOT match the signing key — sign
        // rejects this before producing wire bytes.
        let r = build_signed_envelope_wire(
            &key,
            wrong_source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"x".to_vec(),
            1,
        );
        assert!(r.is_err());
    }

    #[test]
    fn build_signed_envelope_wire_lower_layer_has_no_discriminator_gate() {
        // build_signed_envelope_wire itself does NOT gate discriminators —
        // it's a lower-level builder. The gate is in
        // `IrohMeshManager::send_ufp2_to_peer` via
        // `is_migrated_to_ufp2_discriminator`. This test documents the
        // boundary so future 4c2.x migrations know to route through the
        // manager helper rather than calling the builder directly with
        // raw u8 values.
        use crate::mesh::ufp2::is_migrated_to_ufp2_discriminator;
        assert!(is_migrated_to_ufp2_discriminator(
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REQUEST
        ));
        assert!(is_migrated_to_ufp2_discriminator(
            tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED
        ));
        // Not yet migrated — future 4c2.x will add these to the allowlist.
        assert!(!is_migrated_to_ufp2_discriminator(
            tentaflow_protocol::mesh::MESH_MSG_NODE_INFO
        ));
        assert!(!is_migrated_to_ufp2_discriminator(
            tentaflow_protocol::mesh::MESH_MSG_HELLO
        ));
        assert!(!is_migrated_to_ufp2_discriminator(
            tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH
        ));
    }

    #[test]
    fn signed_envelope_tamper_detected_at_decode() {
        let key = fresh_key();
        let source = public_key_bytes(&key);
        let dest = [0x22u8; NODE_ID_LEN];

        let mut wire = build_signed_envelope_wire(
            &key,
            source,
            dest,
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"original".to_vec(),
            1,
        )
        .unwrap();
        // Flip a byte deep enough to land inside the body bstr (past the
        // canonical map header + early key/value pairs).
        let last = wire.len() - 1;
        wire[last] ^= 0xFF;

        // CBOR decode + structural validator MAY succeed (if the flipped
        // byte landed in body bytes), but signature verification then fails.
        if let Ok(decoded) = decode_incoming(&wire) {
            assert!(verify_envelope(&decoded.envelope).is_err());
        }
    }
}
