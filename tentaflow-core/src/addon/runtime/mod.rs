// =============================================================================
// File: addon/runtime/mod.rs
// WASM runtime abstraction — wasmtime on Desktop/Router, wasmi on Mobile.
// Re-exports types and functions from the active backend plus language adapters.
// =============================================================================

pub mod language_adapter;
pub use language_adapter::{
    adapter_for_runtime, DotnetAdapter, LanguageAdapter, PythonAdapter, RustAdapter, KNOWN_RUNTIMES,
};

#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod runtime_wasmtime;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use runtime_wasmtime::*;

#[cfg(any(target_os = "ios", target_os = "android"))]
mod runtime_wasmi;
#[cfg(any(target_os = "ios", target_os = "android"))]
pub use runtime_wasmi::*;
