// =============================================================================
// File: addon/ui.rs
// UI channel types for the addon CBOR binary protocol. The typed definitions
// live in `tentaflow-sdk-spec`; this module re-exports the subset needed by
// host-side dispatch.
// =============================================================================

pub use tentaflow_sdk_spec::validate_canonical;
pub use tentaflow_sdk_spec::UiPayload;
pub use tentaflow_sdk_spec::UiTag;
