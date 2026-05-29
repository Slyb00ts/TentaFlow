// =============================================================================
// File: protocol/mod.rs — typed protocol primitives
// Purpose: envelopes, channels, IDs, generic CBOR Value, and channel-specific
// payload modules: control (§5), ui (§6), stream (§7). host_fn (§host_fn doc)
// and mesh (existing MESH_PROTOCOL_v1.md) land in later chunks. See
// docs/ADDON_BINARY_PROTOCOL_v1.md.
// =============================================================================

#[macro_use]
pub mod macros;

pub mod camera;
pub mod canonical;
pub mod control;
pub mod envelope;
pub mod frame;
pub mod ids;
pub mod stream;
pub mod streaming;
pub mod ui;
pub mod value;
