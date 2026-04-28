// =============================================================================
// File: collectors/linux_gpu/mod.rs — Linux GPU vendor collectors (AMD ROCm,
// Intel iGPU). Each submodule pairs a ProfileCollector with a CollectorParser.
// Sampler collectors spawn vendor CLIs (rocm-smi, intel_gpu_top) and stream
// their stdout into a CSV/JSON artifact; the rocprof attach-mode collector
// shells out per session and parses the resulting kernel-stats CSV.
// =============================================================================

pub mod intel_gpu_top;
pub mod rocprof_kernels;
pub mod rocsmi_util;
