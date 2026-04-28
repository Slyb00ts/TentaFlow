// =============================================================================
// File: collectors/macos/mod.rs — macOS-only profile collectors. Each submodule
// pairs a `ProfileCollector` (child-process driver) with a `CollectorParser`.
// The structs themselves are always compiled (so the registry can list them on
// any host); their probes report `Unavailable` and `start` errors out on
// non-macOS targets.
// =============================================================================

pub mod iostat_disk;
pub mod powermetrics_gpu;
pub mod powermetrics_power;
pub mod vm_stat_ram;
