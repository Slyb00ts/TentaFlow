// =============================================================================
// File: addon/runtime/language_adapter.rs
// Adapts WASM module exports to the standard addon lifecycle per source language.
// =============================================================================

/// Maps between the standard addon lifecycle and language-specific WASM export
/// conventions. Rust addons use bare names (`on_start`), while .NET NativeAOT
/// and CPython WASI modules use prefixed names (`tentaflow_on_start`) to avoid
/// collisions with their respective runtime internals.
pub trait LanguageAdapter: Send + Sync {
    /// Identifier that matches `manifest.runtime`.
    fn runtime_id(&self) -> &str;

    /// Export name for the start lifecycle callback.
    fn export_on_start(&self) -> &str;

    /// Export name for the stop lifecycle callback.
    fn export_on_stop(&self) -> &str;

    /// Export name for the request handler.
    fn export_on_request(&self) -> &str;

    /// Export name for the tick handler (service mode).
    fn export_on_tick(&self) -> &str;

    /// Export name for the event handler.
    fn export_on_event(&self) -> &str;

    /// Export name for the panel-open handler. Called on a running instance
    /// when the user opens a panel — the addon emits PanelShell/SlotContent
    /// without restarting. Signature: (panel_id_ptr, panel_id_len, epoch) -> i32.
    fn export_on_panel_open(&self) -> &str;

    /// Whether the module needs `_start` / `_initialize` before lifecycle calls.
    fn needs_wasi_start(&self) -> bool;

    /// Extra fuel budget for module-level initialization (GC, interpreter boot).
    /// Zero means no additional fuel beyond the default store budget.
    fn init_fuel_budget(&self) -> u64;
}

// =============================================================================
// Rust adapter (current default — bare export names, no init overhead)
// =============================================================================

pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn runtime_id(&self) -> &str { "wasmtime" }
    fn export_on_start(&self) -> &str { "on_start" }
    fn export_on_stop(&self) -> &str { "on_stop" }
    fn export_on_request(&self) -> &str { "on_request" }
    fn export_on_tick(&self) -> &str { "on_tick" }
    fn export_on_event(&self) -> &str { "on_event" }
    fn export_on_panel_open(&self) -> &str { "on_panel_open" }
    fn needs_wasi_start(&self) -> bool { false }
    fn init_fuel_budget(&self) -> u64 { 0 }
}

// =============================================================================
// .NET NativeAOT WASI adapter (prefixed exports, GC init via _start)
// =============================================================================

pub struct DotnetAdapter;

impl LanguageAdapter for DotnetAdapter {
    fn runtime_id(&self) -> &str { "dotnet" }
    fn export_on_start(&self) -> &str { "tentaflow_on_start" }
    fn export_on_stop(&self) -> &str { "tentaflow_on_stop" }
    fn export_on_request(&self) -> &str { "tentaflow_on_request" }
    fn export_on_tick(&self) -> &str { "tentaflow_on_tick" }
    fn export_on_event(&self) -> &str { "tentaflow_on_event" }
    fn export_on_panel_open(&self) -> &str { "tentaflow_on_panel_open" }
    fn needs_wasi_start(&self) -> bool { true }
    fn init_fuel_budget(&self) -> u64 { 50_000_000 }
}

// =============================================================================
// CPython WASI adapter (prefixed exports, interpreter boot via _start)
// =============================================================================

pub struct PythonAdapter;

impl LanguageAdapter for PythonAdapter {
    fn runtime_id(&self) -> &str { "python" }
    fn export_on_start(&self) -> &str { "tentaflow_on_start" }
    fn export_on_stop(&self) -> &str { "tentaflow_on_stop" }
    fn export_on_request(&self) -> &str { "tentaflow_on_request" }
    fn export_on_tick(&self) -> &str { "tentaflow_on_tick" }
    fn export_on_event(&self) -> &str { "tentaflow_on_event" }
    fn export_on_panel_open(&self) -> &str { "tentaflow_on_panel_open" }
    fn needs_wasi_start(&self) -> bool { true }
    fn init_fuel_budget(&self) -> u64 { 100_000_000 }
}

// =============================================================================
// Factory
// =============================================================================

/// All runtime identifiers accepted by the manifest parser.
pub const KNOWN_RUNTIMES: &[&str] = &["wasmtime", "wasmi", "dotnet", "python"];

/// Returns the appropriate adapter for a manifest `runtime` value.
/// `"wasmi"` maps to `RustAdapter` because wasmi is an alternative engine for
/// Rust-compiled modules on mobile, not a different source language.
pub fn adapter_for_runtime(runtime_id: &str) -> Option<Box<dyn LanguageAdapter>> {
    match runtime_id {
        "wasmtime" | "wasmi" => Some(Box::new(RustAdapter)),
        "dotnet" => Some(Box::new(DotnetAdapter)),
        "python" => Some(Box::new(PythonAdapter)),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_adapter_exports() {
        let a = RustAdapter;
        assert_eq!(a.runtime_id(), "wasmtime");
        assert_eq!(a.export_on_start(), "on_start");
        assert_eq!(a.export_on_stop(), "on_stop");
        assert_eq!(a.export_on_request(), "on_request");
        assert_eq!(a.export_on_tick(), "on_tick");
        assert_eq!(a.export_on_event(), "on_event");
        assert!(!a.needs_wasi_start());
        assert_eq!(a.init_fuel_budget(), 0);
    }

    #[test]
    fn dotnet_adapter_needs_start() {
        let a = DotnetAdapter;
        assert_eq!(a.runtime_id(), "dotnet");
        assert!(a.needs_wasi_start());
        assert!(a.init_fuel_budget() >= 10_000_000);
        assert_eq!(a.export_on_start(), "tentaflow_on_start");
        assert_eq!(a.export_on_stop(), "tentaflow_on_stop");
        assert_eq!(a.export_on_request(), "tentaflow_on_request");
        assert_eq!(a.export_on_tick(), "tentaflow_on_tick");
        assert_eq!(a.export_on_event(), "tentaflow_on_event");
    }

    #[test]
    fn python_adapter_init_fuel() {
        let a = PythonAdapter;
        assert_eq!(a.runtime_id(), "python");
        assert!(a.needs_wasi_start());
        assert!(a.init_fuel_budget() >= 50_000_000);
        assert_eq!(a.export_on_start(), "tentaflow_on_start");
    }

    #[test]
    fn adapter_factory() {
        assert!(adapter_for_runtime("wasmtime").is_some());
        assert_eq!(adapter_for_runtime("wasmtime").unwrap().runtime_id(), "wasmtime");

        assert!(adapter_for_runtime("wasmi").is_some());
        assert_eq!(adapter_for_runtime("wasmi").unwrap().runtime_id(), "wasmtime");

        assert!(adapter_for_runtime("dotnet").is_some());
        assert_eq!(adapter_for_runtime("dotnet").unwrap().runtime_id(), "dotnet");

        assert!(adapter_for_runtime("python").is_some());
        assert_eq!(adapter_for_runtime("python").unwrap().runtime_id(), "python");

        assert!(adapter_for_runtime("unknown").is_none());
        assert!(adapter_for_runtime("").is_none());
    }

    #[test]
    fn known_runtimes_consistent_with_factory() {
        for &rt in KNOWN_RUNTIMES {
            assert!(
                adapter_for_runtime(rt).is_some(),
                "KNOWN_RUNTIMES contains '{}' but adapter_for_runtime returns None",
                rt
            );
        }
    }
}
