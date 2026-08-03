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
    /// Block-scaled FP4 MMA with UE8M0 (power-of-two) scales — the MXFP4
    /// instruction. Doubles K per instruction against FP8.
    pub fp4_block_scale_ue8m0: bool,
    /// Block-scaled FP4 MMA with E4M3 scales — NVFP4 computed natively, with
    /// no repack and no second copy of the weights.
    pub fp4_block_scale_e4m3: bool,
    /// `wgmma` warp-group MMA — the core of FlashAttention-3.
    pub wgmma: bool,
    /// `tcgen05` plus tensor memory — the core of FlashAttention-4.
    pub tcgen05: bool,
    /// Tensor Memory Accelerator (`cp.async.bulk.tensor`).
    pub tma: bool,
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
