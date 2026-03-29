// =============================================================================
// Plik: addon/runtime.rs
// Opis: Abstrakcja runtime WASM — wasmtime na Desktop/Router, wasmi na Mobile.
//       Re-eksportuje typy i funkcje z odpowiedniego backendu.
// =============================================================================

#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod runtime_wasmtime;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use runtime_wasmtime::*;

#[cfg(any(target_os = "ios", target_os = "android"))]
mod runtime_wasmi;
#[cfg(any(target_os = "ios", target_os = "android"))]
pub use runtime_wasmi::*;
