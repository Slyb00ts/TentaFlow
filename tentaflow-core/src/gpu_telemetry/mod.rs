// ===== File: gpu_telemetry/mod.rs — vendor-neutral GPU telemetry from the OS =====
//
// Windows keeps the only per-process GPU accounting that works for every vendor
// and under WDDM: nvidia-smi and NVML report per-process memory as N/A there,
// and AMD/Intel have no CLI at all. This module samples it in the background
// (DXGI for the adapter list and dedicated VRAM, PDH for usage) and serves the
// latest snapshot to the node metrics, the VRAM hint of services and the
// profiler. Other platforms return `None` and keep their vendor tools.

use std::sync::Arc;
use std::time::Instant;

#[cfg(target_os = "windows")]
mod windows;

/// One physical GPU as the OS sees it.
#[derive(Debug, Clone)]
pub struct AdapterUsage {
    /// Adapter LUID — stable for the boot, links processes to adapters.
    pub luid: u64,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_total_bytes: u64,
    /// Dedicated VRAM in use by all processes; `None` until the first sample.
    pub dedicated_used_bytes: Option<u64>,
    /// Busiest engine (3D, compute, copy, video) in percent; `None` until the
    /// second sample, because utilization is a rate.
    pub utilization_percent: Option<f32>,
}

/// GPU use of one process on one adapter.
#[derive(Debug, Clone)]
pub struct ProcessUsage {
    pub pid: u32,
    pub luid: u64,
    pub dedicated_bytes: u64,
    pub utilization_percent: f32,
}

#[derive(Debug, Clone)]
pub struct GpuTelemetry {
    pub adapters: Vec<AdapterUsage>,
    pub processes: Vec<ProcessUsage>,
    pub sampled_at: Instant,
}

impl GpuTelemetry {
    /// Dedicated VRAM a process uses, summed over adapters.
    pub fn process_dedicated_bytes(&self, pid: u32) -> u64 {
        self.processes
            .iter()
            .filter(|p| p.pid == pid)
            .map(|p| p.dedicated_bytes)
            .sum()
    }

    /// The adapter whose driver-reported name matches `name`; with several
    /// identical cards, `ordinal` picks the n-th one in DXGI order.
    pub fn adapter_by_name(&self, name: &str, ordinal: usize) -> Option<&AdapterUsage> {
        let wanted = name.trim();
        self.adapters
            .iter()
            .filter(|a| a.name.trim().eq_ignore_ascii_case(wanted))
            .nth(ordinal)
    }
}

/// Latest telemetry snapshot. Starts the sampler on first use; `None` until it
/// has produced a sample and on platforms without OS-level GPU accounting.
pub fn snapshot() -> Option<Arc<GpuTelemetry>> {
    #[cfg(target_os = "windows")]
    {
        windows::snapshot()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}
