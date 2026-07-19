// ===== File: lib.rs — forge-types: shared scalar, quant, shape and device types =====

pub mod dtype;
pub mod error;
pub mod quant;
pub mod shape;

pub use dtype::DType;
pub use error::{ForgeError, Result};
pub use quant::QuantKind;
pub use shape::Shape;

use serde::{Deserialize, Serialize};

/// Static capability report for a compute device (spec §3.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCaps {
    pub name: String,
    pub vendor: Vendor,
    /// Compute capability / arch identifier, e.g. "sm_89", "gfx942", "apple-m3".
    pub arch: String,
    pub total_memory: usize,
    pub max_shared_mem_per_block: usize,
    pub max_threads_per_block: u32,
    pub warp_size: u32,
    /// Streaming-multiprocessor count. Drives the stream-K MMQ grid (one CUDA
    /// block per SM). 0 on hosts that do not expose it (CPU backend).
    pub sm_count: u32,
    /// Native FP8 (E4M3/E5M2) matrix pipeline available.
    pub fp8_native: bool,
    /// Native FP4 tensor cores (NVIDIA Blackwell+). Absent => NVFP4 uses the
    /// software fused-dequant path.
    pub fp4_native: bool,
    pub bf16_native: bool,
    pub supports_p2p: bool,
    pub supports_graph_capture: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Cpu,
}

/// Memory kinds a `Device` can allocate (spec §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemKind {
    /// Device-local (VRAM).
    Device,
    /// Page-locked host memory for async transfers.
    PinnedHost,
    /// Unified/managed memory where supported.
    Managed,
}
