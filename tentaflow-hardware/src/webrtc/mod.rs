// =============================================================================
// File: webrtc/mod.rs
// Purpose: Generic, vendor-agnostic WebRTC channel. This is the native "dumb
//          pipe" that Core will expose to addons (Chunk 1b host functions);
//          robot-specific logic lives in the addon, not here.
// =============================================================================

pub mod channel;

pub use channel::{ChannelState, DcMessage, KeepaliveConfig, WebRtcChannel, WebRtcConfig};
