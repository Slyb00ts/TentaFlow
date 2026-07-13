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
pub mod aead;
pub mod auth;
pub mod channel;
pub mod compress;
pub mod envelope;
pub mod error;
pub mod flags;
pub mod fragment;
pub mod pipeline;
pub mod replay;
pub mod sign;
pub mod validator;

pub use address::{NodeAddress, NodeAddressKind};
pub use aead::{
    compute_aad, decrypt_body, decrypt_envelope_body, encrypt_body, encrypt_envelope_body, AeadKey,
    NonceCounter, PersistHint, AEAD_KEY_LEN, AEAD_NONCE_LEN, AEAD_TAG_LEN,
    COUNTER_ROTATION_THRESHOLD, MAX_KEY_AGE_MS, NONCE_COUNTER_LEN, NONCE_PREFIX_LEN,
};
pub use auth::{Auth, AuthKind};
pub use channel::{channels, Channel, Kind};
pub use compress::{
    compress_body, decompress_body, decompress_body_with_limit, should_compress,
    COMPRESSION_THRESHOLD_BYTES, DEFAULT_MAX_DECOMPRESSED_BYTES,
};
pub use envelope::{
    Envelope, FrameProtocolVersion, MessageId, Priority, TraceId, FRAME_PROTOCOL_VERSION,
    MESSAGE_ID_LEN, NODE_ID_LEN, SIGNATURE_LEN, TRACE_ID_LEN,
};
pub use error::{FrameError, FrameErrorCode};
pub use flags::Flags;
pub use fragment::{
    finalize_reassembled_envelope, split_envelope_into_fragments, AcceptOutcome,
    FragmentSendCrypto, ReassemblyManager, AEAD_OVERHEAD_BYTES, MAX_FRAGMENT_COUNT,
    MAX_REASSEMBLY_BYTES, REASSEMBLY_TIMEOUT_MS,
};
pub use pipeline::{receive_envelope_pipeline, send_envelope_pipeline, ReceiveCrypto, SendCrypto};
pub use replay::{
    check_clock_skew_with, DedupKey, ReplayGuard, DEFAULT_CLOCK_SKEW_MS,
    DEFAULT_DEDUP_CAPACITY_PER_SOURCE, NON_FRAGMENT_INDEX_SENTINEL,
};
pub use sign::{
    canonical_envelope_for_signing, public_key_bytes, sign_envelope, signing_key_from_bytes,
    verify_envelope, SIGNATURE_PLACEHOLDER,
};
pub use validator::{channel_auth_policy, validate_envelope, ChannelAuthPolicy, SignRequirement};
