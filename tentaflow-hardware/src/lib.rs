// =============================================================================
// File: lib.rs
// Purpose: tentaflow-hardware — native device/robot integrations, organized by
//          vendor module. First vendor: unitree (Go2). Each device exposes its
//          transport behind a common contract so Core can drive any of them.
// =============================================================================

pub mod unitree;
#[cfg(feature = "full")]
pub mod webrtc;
