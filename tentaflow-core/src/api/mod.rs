// =============================================================================
// Plik: api/mod.rs
// Opis: Handlery API — OpenAI-compatible API i Dashboard REST API.
// =============================================================================

pub mod openai;

#[cfg(feature = "dashboard-api")]
pub mod dashboard;

#[cfg(feature = "dashboard-api")]
pub mod unified_server;

#[cfg(feature = "dashboard-api")]
pub mod tls_pem;
