// =============================================================================
// Plik: lib.rs
// Opis: TentaFlow Core — wspolna biblioteka dla Router, Desktop i Mobile.
// =============================================================================

pub mod config;
pub mod error;
pub mod crypto;
pub mod db;
pub mod net;
pub mod routing;
pub mod flow_engine;
pub mod middleware;
pub mod services;
pub mod metrics;
pub mod prompt_registry;
pub mod intent_analyzer;
pub mod memory_analyzer;
pub mod mesh;
pub mod inference;
pub mod stt;
pub mod hub;

#[cfg(feature = "inference-diarization")]
pub mod diarization;

pub mod auth;
pub mod api;
pub mod audit;

pub mod addon;
pub mod deploy;
pub mod system_check;
