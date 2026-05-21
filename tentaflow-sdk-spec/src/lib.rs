// =============================================================================
// File: lib.rs — tentaflow-sdk-spec crate root
// Purpose: single source of truth for TentaFlow addon protocol types, UI catalog
// and codegen annotations. Wire format: CBOR Core Deterministic Encoding
// (RFC 8949 §4.2.1+§4.2.2). See docs/ADDON_BINARY_PROTOCOL_v1.md.
//
// Strict canonical-decode policy (§2.2): typed decoders here enforce a defensive
// subset — bstr fixed lengths, protocol_version == 1, ControlTag whitelist,
// per-variant field whitelisting for ResumeStatus / RejectReason / RateLimitScope,
// indefinite-length reject in Value. The full §2.2 wire validator (reject
// NonCanonicalIntegerWidth / NonCanonicalFloatWidth / NonCanonicalKeyOrder /
// DuplicateMapKey / unknown-keys on every derived map) lives in the host
// dispatch path landing in Krok 4 of Faza 6 (see SYNC_LEDGER_PLAN / addon
// rewrite roadmap). Encoders here already produce canonical output.
// =============================================================================

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod protocol;

pub use protocol::{
    control::{
        AuthContext, Backpressure, BackpressureSeverity, Capability, CapabilityRejection,
        CapabilityRevoked, CborMap, ControlPayload, ControlTag, CreditBudget, CreditGrant,
        GrantRationale, Heartbeat, ProtocolHello, ProtocolReject, ProtocolWelcome, QueueDepth,
        RateLimitScope, RateLimitUpdate, RejectReason, Resume, ResumeMode, ResumeStatus,
        ServerLimits, SessionEnd, SessionEndCode,
    },
    envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion, PROTOCOL_VERSION},
    ids::{ClientActionId, DeviceId, Hash32, NodeId, SessionId, TraceId},
    value::Value,
};
