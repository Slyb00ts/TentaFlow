// =============================================================================
// File: mesh/ufp2/mod.rs — UFP/2 wire format for mesh transport
// Purpose: implement the UFP/2 CBOR envelope per
// docs/UNIFIED_FRAME_PROTOCOL_v2.md as the sole wire shape for mesh
// unicast streams. Every unicast send goes through `send_ufp2_to_peer` /
// `broadcast_ufp2_to_trusted`; every unicast receive is decoded by
// `classify_inbound`. Bi-stream protocols (FORWARD_REQ, FORWARD_STREAM_REQ)
// still use their own framing and do not flow through this module.
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
pub use receive::{classify_inbound, InboundMeshFrame};
pub use send::build_signed_envelope_wire;
