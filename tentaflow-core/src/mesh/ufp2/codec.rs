// =============================================================================
// File: mesh/ufp2/codec.rs — UFP/2 envelope encode/decode for mesh traffic
// Purpose: build a complete UFP/2 envelope around an outgoing mesh payload,
// or decode an arriving UFP/2 envelope back into (kind, body). The codec
// is wire-format-only — it does NOT touch the iroh stream, signing keys,
// or trust state; those concerns live in `send.rs` / `receive.rs`.
//
// Sender flow (`encode_outgoing`):
//   1. Build Envelope skeleton: protocol_version=2, channel=Mesh (0x04),
//      kind = `MeshKind` from discriminator, source/destination NodeAddress
//      from raw 32-byte Ed25519 pubkeys, fresh ULID-style message_id, body
//      = caller-provided plaintext payload.
//   2. Caller (`send.rs`) runs `sign_envelope` and `send_envelope_pipeline`
//      to add signature / encryption / compression as needed.
//   3. Codec returns the canonical CBOR-encoded envelope bytes ready to
//      write to the wire.
//
// Receiver flow (`decode_incoming`):
//   1. Caller (`receive.rs`) has already peeked the first byte and decided
//      this is a UFP/2 envelope (CBOR map header).
//   2. Decode bytes → Envelope.
//   3. Run structural `validate_envelope` (4c1g — enforces channel/kind
//      ranges, auth invariants, source-subject binding, etc.).
//   4. Return (legacy_discriminator, body_bytes) so existing legacy
//      dispatch code can route the message into its existing handler.
// =============================================================================

use std::time::{SystemTime, UNIX_EPOCH};

use tentaflow_sdk_spec::protocol::frame::{
    address::NodeAddress,
    auth::Auth,
    channel::{channels, Kind},
    envelope::{Envelope, FrameProtocolVersion, MessageId, Priority, MESSAGE_ID_LEN, NODE_ID_LEN},
    error::FrameError,
    flags::Flags,
    validator::validate_envelope,
};

use super::discriminators::{channel_from_legacy, kind_from_legacy, legacy_from_channel_kind};

/// Result of decoding an incoming UFP/2 mesh envelope: the legacy
/// discriminator (so it slots into existing dispatch) plus the raw body
/// bytes. The signed envelope itself is also returned so callers that
/// need to inspect source/auth/epoch can do so without re-decoding.
pub struct DecodedMeshEnvelope {
    pub legacy_discriminator: u8,
    pub body: Vec<u8>,
    pub envelope: Envelope,
}

/// Encode an outgoing mesh/sync payload into a canonical UFP/2 envelope. The
/// returned envelope is UNSIGNED and UNENCRYPTED — caller's `send.rs`
/// applies signature/encryption via the sdk-spec `send_envelope_pipeline`
/// based on per-pair crypto state.
///
/// `source_node_pubkey` and `destination_node_pubkey` are raw 32-byte
/// Ed25519 keys (the same shape used by `iroh` peer identity and the
/// existing `tentaflow_protocol::mesh` security layer).
///
/// `epoch` is the sender's current policy_epoch (§6.2). Mesh/sync traffic is
/// always `NodeIdentity`-signed in UFP/2 per §11.3, so the validator will
/// require a non-None epoch.
pub fn build_envelope(
    source_node_pubkey: [u8; NODE_ID_LEN],
    destination_node_pubkey: [u8; NODE_ID_LEN],
    legacy_discriminator: u8,
    body: Vec<u8>,
    epoch: u32,
) -> Envelope {
    let channel = channel_from_legacy(legacy_discriminator);
    let kind = kind_from_legacy(legacy_discriminator);
    Envelope {
        protocol_version: FrameProtocolVersion::V2,
        message_id: fresh_message_id(),
        source: NodeAddress::node(source_node_pubkey),
        destination: NodeAddress::node(destination_node_pubkey),
        created_at_ms: now_ms(),
        flags: Flags::NONE.with(Flags::IS_SIGNED),
        priority: priority_for_kind(kind),
        channel,
        kind,
        body,
        correlation_id: None,
        trace_id: None,
        ttl_ms: None,
        auth: Auth::node_unsigned(source_node_pubkey, epoch),
        fragment_index: None,
        fragment_count: None,
        forwarded_via: None,
    }
}

/// Decode an incoming UFP/2 mesh envelope from raw wire bytes.
///
/// Performs:
///   1. Canonical CBOR decode (`minicbor::decode::<Envelope>`).
///   2. Structural validation via `validate_envelope` (4c1g).
///   3. Channel guard: accepts Mesh (0x04) and SyncLedger (0x06) only —
///      iroh mesh transport carries both steady-state mesh and ledger frames.
///   4. Discriminator extraction: legacy u8 derived from `kind`.
///
/// Signature verification (`verify_envelope`) and replay protection
/// (`ReplayGuard`) are the caller's responsibility — those need access to
/// per-peer state (trusted-keys pool, dedup LRU) that the codec doesn't own.
pub fn decode_incoming(bytes: &[u8]) -> Result<DecodedMeshEnvelope, FrameError> {
    let envelope: Envelope = minicbor::decode(bytes).map_err(|e| {
        FrameError::new(
            tentaflow_sdk_spec::protocol::frame::error::FrameErrorCode::CanonicalEncoding,
            format!("decode_incoming: CBOR decode failed: {e}"),
        )
    })?;
    validate_envelope(&envelope)?;
    if envelope.channel != channels::MESH && envelope.channel != channels::SYNC_LEDGER {
        return Err(FrameError::new(
            tentaflow_sdk_spec::protocol::frame::error::FrameErrorCode::UnknownChannel,
            format!(
                "decode_incoming: arrived on mesh transport but channel = 0x{:02X} (expected Mesh 0x04 or SyncLedger 0x06)",
                envelope.channel.0
            ),
        ));
    }
    let legacy = legacy_from_channel_kind(envelope.channel, envelope.kind).ok_or_else(|| {
        FrameError::new(
            tentaflow_sdk_spec::protocol::frame::error::FrameErrorCode::UnknownKind,
            format!(
                "decode_incoming: channel 0x{:02X} kind 0x{:04X} cannot map to internal MESH_MSG_* dispatch",
                envelope.channel.0, envelope.kind.0
            ),
        )
    })?;
    let body = envelope.body.clone();
    Ok(DecodedMeshEnvelope {
        legacy_discriminator: legacy,
        body,
        envelope,
    })
}

/// Encode a UFP/2 envelope into canonical CBOR bytes ready for wire
/// transmission. Thin wrapper around `minicbor::encode` exposed so that
/// `send.rs` does not have to depend on minicbor directly.
pub fn encode_envelope(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let mut buf = Vec::with_capacity(256 + envelope.body.len());
    minicbor::encode(envelope, &mut buf).map_err(|e| {
        FrameError::new(
            tentaflow_sdk_spec::protocol::frame::error::FrameErrorCode::CanonicalEncoding,
            format!("encode_envelope: CBOR encode failed: {e}"),
        )
    })?;
    Ok(buf)
}

/// Heuristic byte test: does this byte look like the start of a CBOR map
/// header that could plausibly be a UFP/2 envelope (10..=17 keys)?
///
/// The UFP/2 envelope is always a definite-length CBOR map (canonical
/// Faza 6 profile). For maps with 0..=23 entries the major-type + count
/// is encoded as a single byte `0xA0 + N`. UFP/2 envelopes carry between
/// 10 mandatory keys (no optionals, no fragments, no hop trail) and 17
/// max keys, so the first byte falls in `0xAA..=0xB1`.
///
/// Legacy MESH_MSG_* discriminators sit in `0x10..=0x4C`, with no overlap.
pub const fn looks_like_ufp2_envelope_first_byte(first: u8) -> bool {
    first >= 0xAA && first <= 0xB1
}

fn fresh_message_id() -> MessageId {
    // ULID-shaped: 48-bit big-endian millisecond timestamp || 80 bits random.
    let mut out = [0u8; MESSAGE_ID_LEN];
    let now = now_ms() as u128;
    let ts_bytes = (now & 0x0000_FFFF_FFFF_FFFF).to_be_bytes();
    // ts_bytes is 16 bytes (u128); take the trailing 6 bytes (low 48 bits).
    out[..6].copy_from_slice(&ts_bytes[10..16]);
    // 10 bytes of CSPRNG randomness.
    use rand_core_06::{OsRng, RngCore};
    let mut rand_bytes = [0u8; 10];
    OsRng.fill_bytes(&mut rand_bytes);
    out[6..].copy_from_slice(&rand_bytes);
    MessageId(out)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn priority_for_kind(kind: Kind) -> Priority {
    // Heartbeat / topology gossip are low-impact background traffic.
    // Pairing / trust / sync interactive. Logs / deploy progress are bulk.
    match kind.0 {
        0x0010 | 0x001A | 0x001B => Priority::Bulk,            // heartbeat, topology, known peers
        0x0020..=0x0027 => Priority::Interactive,              // pairing/trust
        0x0030..=0x0033 => Priority::Bulk,                     // command / deploy
        _ => Priority::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_envelope_carries_kind_and_body() {
        let env = build_envelope(
            [0x11u8; NODE_ID_LEN],
            [0x22u8; NODE_ID_LEN],
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            b"heartbeat-payload".to_vec(),
            7,
        );
        assert_eq!(env.channel, channels::MESH);
        assert_eq!(env.kind.0 as u8, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT);
        assert_eq!(env.body, b"heartbeat-payload");
        assert_eq!(env.auth.epoch, Some(7));
    }

    #[test]
    fn build_envelope_message_ids_are_distinct() {
        let env_a = build_envelope(
            [0u8; NODE_ID_LEN],
            [1u8; NODE_ID_LEN],
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            vec![],
            0,
        );
        let env_b = build_envelope(
            [0u8; NODE_ID_LEN],
            [1u8; NODE_ID_LEN],
            tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT,
            vec![],
            0,
        );
        assert_ne!(env_a.message_id, env_b.message_id);
    }

    #[test]
    fn looks_like_ufp2_envelope_first_byte_disjoint_from_legacy() {
        // Every UFP/2 envelope first byte must NOT collide with any legacy
        // MESH_MSG_* discriminator value.
        for disc in 0x10u8..=0x4C {
            assert!(
                !looks_like_ufp2_envelope_first_byte(disc),
                "legacy discriminator 0x{:02X} must not be misinterpreted as UFP/2",
                disc
            );
        }
    }

    #[test]
    fn looks_like_ufp2_envelope_first_byte_accepts_plausible_map_headers() {
        // 0xAA = map with 10 entries (minimum UFP/2 envelope).
        // 0xB1 = map with 17 entries (max with all optionals + fragments + hop trail).
        assert!(looks_like_ufp2_envelope_first_byte(0xAA));
        assert!(looks_like_ufp2_envelope_first_byte(0xAD));
        assert!(looks_like_ufp2_envelope_first_byte(0xB1));
    }

    #[test]
    fn looks_like_ufp2_envelope_first_byte_rejects_outliers() {
        assert!(!looks_like_ufp2_envelope_first_byte(0xA9));
        assert!(!looks_like_ufp2_envelope_first_byte(0xB2));
        assert!(!looks_like_ufp2_envelope_first_byte(0x00));
        assert!(!looks_like_ufp2_envelope_first_byte(0xFF));
    }
}
