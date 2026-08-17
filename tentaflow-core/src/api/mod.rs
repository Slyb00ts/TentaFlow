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

pub mod legal;

pub mod model_bundle;

pub mod ml_studio_export;
pub mod project_studio_export;

pub mod ml_studio_share;

pub mod dashboard;

pub mod unified_server;

pub mod tls_pem;

pub mod mtls;
