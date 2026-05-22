// =============================================================================
// File: mesh/ufp2/mod.rs — UFP/2 wire format integration for mesh transport
// Purpose: bridge the legacy `[disc:u8][rkyv-payload]` mesh wire and the
// new UFP/2 CBOR envelope per docs/UNIFIED_FRAME_PROTOCOL_v2.md. During
// Faza 6 Krok 4c2.x migration both wire shapes coexist on the same iroh
// transport, distinguished by the first byte of the stream:
//   - 0x10..=0x4C  → legacy MESH_MSG_* (still active during migration)
//   - 0xAA..=0xB1  → UFP/2 envelope CBOR map header (10..=17 keys)
// Each chunk in 4c2.1..=4c2.5 migrates one group of message types from
// the legacy path to UFP/2; 4c2.6 deletes the legacy path entirely.
// =============================================================================

pub mod codec;
pub mod discriminators;
pub mod receive;
pub mod send;

pub use codec::{
    build_envelope, decode_incoming, encode_envelope, looks_like_ufp2_envelope_first_byte,
    DecodedMeshEnvelope,
};
pub use discriminators::{
    is_migrated_to_ufp2_discriminator, kind_from_legacy, kinds, legacy_from_kind,
};
pub use receive::{classify_inbound, is_legacy_mesh_discriminator, InboundMeshFrame};
pub use send::build_signed_envelope_wire;
