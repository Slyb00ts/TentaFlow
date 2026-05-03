// =============================================================================
// Plik: services/backend/mod.rs
// Opis: Pula polaczen do backendow LLM — zarzadzanie klientami HTTP.
//       Kazdy backend pool ma skonfigurowane backendy (URL, API key, timeout)
//       i client wysyla do nich requesty.
// =============================================================================

pub mod client;

pub use client::BackendClient;
