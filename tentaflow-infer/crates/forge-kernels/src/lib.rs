// ===== File: lib.rs — forge-kernels: kernel artifact registry and typed launchers =====
// Kernels are authored in Mojo and shipped as AOT PTX + manifest.json
// (ADR-0001). This crate loads those artifacts onto a HAL device and exposes
// typed launch wrappers so the engine never touches raw entry symbols.

mod registry;
mod launchers;

pub use launchers::{
    Kernels, Nvfp4GgufQ8Projection, Q8ActPrepared, SAMPLE_MAX_TOPK, SAMPLE_MAX_VOCAB,
    SAMPLE_SCRATCH_PAIRS,
};
pub use registry::{KernelArtifacts, Manifest};
