// =============================================================================
// File: unitree/go2/mod.rs
// Purpose: Unitree Go2 (Air) transport. WebRTC LAN signaling handshake now;
//          data-channel control + video ingress build on top.
// =============================================================================

pub mod protocol;
#[cfg(feature = "full")]
pub mod handshake;
#[cfg(feature = "full")]
pub mod session;
