// =============================================================================
// Plik: api/mod.rs
// Opis: Handlery API — OpenAI-compatible API i Dashboard REST API.
// =============================================================================

pub mod openai;

pub mod frame_pickup;

pub mod rate_limit;

pub mod frames;

#[cfg(feature = "camera")]
pub mod recording;

#[cfg(feature = "dashboard-api")]
pub mod dashboard;

#[cfg(feature = "dashboard-api")]
pub mod unified_server;

#[cfg(feature = "dashboard-api")]
pub mod tls_pem;
