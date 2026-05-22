// =============================================================================
// File: protocol/frame/mod.rs — UFP/2 wire types (Krok 4c1a, no crypto)
// Purpose: Envelope, NodeAddress, Auth, Flags, Channel, Kind types per
// docs/UNIFIED_FRAME_PROTOCOL_v2.md v0.9. This chunk introduces ONLY the
// data carriers and their canonical CBOR encode/decode.
//
// Out of scope for 4c1a (delivered in later chunks):
//   - Ed25519 sign/verify (§6.3)                      → 4c1b
//   - ChaCha20-Poly1305 AEAD + AAD + nonce (§7.2)    → 4c1c
//   - lz4 frame compression + pipeline (§7.1, §8)    → 4c1d
//   - Fragmentation + reassembly atomicity (§10)     → 4c1e
//   - Replay protection LRU + 8-step pipeline (§9)   → 4c1f
//   - Structural validator that enforces §11.3 auth invariants, channel/kind
//     range checks, reserved flag bits = 0, explicit-CBOR-null rejection,
//     unknown map-key rejection, and fragment field presence consistency
//                                                     → 4c1g
//
// The types in this module are NOT a complete UFP/2 receive gate on their
// own. They MUST NOT be used to decode untrusted bytes from a network path
// until 4c1g lands. They are safe to use for in-process construction,
// roundtrip tests, and as the foundation for subsequent crypto/validator
// chunks.
// =============================================================================

pub mod address;
pub mod auth;
pub mod channel;
pub mod envelope;
pub mod error;
pub mod flags;

pub use address::{NodeAddress, NodeAddressKind};
pub use auth::{Auth, AuthKind};
pub use channel::{channels, Channel, Kind};
pub use envelope::{
    Envelope, FrameProtocolVersion, MessageId, Priority, TraceId, FRAME_PROTOCOL_VERSION,
    MESSAGE_ID_LEN, NODE_ID_LEN, SIGNATURE_LEN, TRACE_ID_LEN,
};
pub use error::{FrameError, FrameErrorCode};
pub use flags::Flags;
