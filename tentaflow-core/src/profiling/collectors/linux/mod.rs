// =============================================================================
// File: collectors/linux/mod.rs — Linux no-priv profile collectors (CPU util,
// RAM, disk I/O, RAPL power, nvidia-smi GPU). Each submodule pairs a
// ProfileCollector with a CollectorParser. Polling-based collectors run on a
// dedicated std::thread; the nvidia-smi collector wraps a child process.
// =============================================================================

pub mod cpu_util;
pub mod disk;
pub mod netdev;
pub mod nvsmi_gpu;
pub mod perf_sampling;
pub mod ram;
pub mod rapl_power;
