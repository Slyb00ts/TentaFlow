// =============================================================================
// File: collectors/windows/mod.rs — Windows no-Admin profile collectors backed
// by PDH (Performance Data Helper). Each submodule pairs a `ProfileCollector`
// with a `CollectorParser`. Structs compile on every host so the registry can
// list them; PDH calls are gated to `target_os = "windows"`. Probes on other
// hosts return `Unavailable` and `start` errors out.
// =============================================================================

pub mod pdh_cpu_util;
pub mod pdh_disk;
pub mod pdh_gpu;
pub mod pdh_ram;

#[cfg(target_os = "windows")]
pub(crate) mod pdh_sys;
