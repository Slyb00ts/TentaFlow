// =============================================================================
// File: protocol/mod.rs — typed protocol primitives
// Purpose: envelopes, channels, IDs, generic CBOR Value, and channel-specific
// payload modules (currently: control). UI/host_fn/stream/mesh come in later
// chunks. See docs/ADDON_BINARY_PROTOCOL_v1.md.
// =============================================================================

#[macro_use]
pub mod macros;

pub mod control;
pub mod envelope;
pub mod ids;
pub mod ui;
pub mod value;
