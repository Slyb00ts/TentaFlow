// =============================================================================
// Plik: lib.rs
// Opis: TentaFlow Core — wspolna biblioteka dla Router, Desktop i Mobile.
// =============================================================================

// Self-alias umozliwia proc-macrom z `tentaflow-macros` uzywac bezwzglednej
// sciezki `::tentaflow_core::dispatch::*` zarowno w zewnetrznych crate'ach jak
// i w kodzie samego tentaflow-core (handlerom).
extern crate self as tentaflow_core;

pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod flow_engine;
pub mod hub;
pub mod inference;
pub mod memory;
pub mod mesh;
pub mod metrics;
pub mod middleware;
pub mod net;
pub mod prompt_registry;
pub mod routing;
pub mod services;
pub mod stt;

#[cfg(feature = "inference-diarization")]
pub mod diarization;

pub mod api;
pub mod audit;
pub mod auth;
pub mod dispatch;

pub mod addon;
pub mod deploy;
pub mod license;
pub mod lifecycle_signal;
pub mod meeting;
pub mod paths;
pub mod system_check;
