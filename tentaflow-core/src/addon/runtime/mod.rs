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

// Exposed on Desktop ONLY for the wasmi WASI-shim regression test
// (`tests/addon_dotnet_e2e.rs`), behind the `wasmi-runtime-test` feature.
// It is a plain `pub mod` (NOT glob re-exported), so the Desktop runtime stays
// wasmtime — this only makes the mobile shim reachable from a Desktop test that
// proves a `wasm32-wasip1` .NET module instantiates under the wasmi interpreter.
#[cfg(all(
    feature = "wasmi-runtime-test",
    not(any(target_os = "ios", target_os = "android"))
))]
pub mod runtime_wasmi;
