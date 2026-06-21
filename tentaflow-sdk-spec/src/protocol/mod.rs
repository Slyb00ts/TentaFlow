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
pub mod camera_metadata;
pub mod canonical;
pub mod control;
pub mod doc_parse;
pub mod document;
pub mod envelope;
pub mod flow;
pub mod frame;
pub mod gate;
pub mod graph;
pub mod ids;
pub mod recording;
pub mod robot;
pub mod services;
pub mod state;
pub mod stream;
pub mod streaming;
pub mod ui;
pub mod value;
pub mod vector;
pub mod vector_query;
pub mod webrtc;
