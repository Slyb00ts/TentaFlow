// ===== File: lib.rs — forge-kernels: kernel artifact registry and typed launchers =====
// Kernels are authored in Mojo and shipped as AOT PTX + manifest.json
// (ADR-0001). This crate loads those artifacts onto a HAL device and exposes
// typed launch wrappers so the engine never touches raw entry symbols.

#[cfg(any(feature = "metal", feature = "metal-check"))]
pub mod cpu_matmul;
#[cfg(any(feature = "metal", feature = "metal-check"))]
mod dense_exec;
mod launchers;
// Wzorzec hostowy jest DOSTĘPNY WSZĘDZIE i to jest jego sens: kontrakt, który
// da się uruchomić bez akceleratora, można sprawdzić na każdej maszynie.
mod host_exec;
pub use host_exec::HostExec;
// Wykonawca CUDA nie ma flagi: cały stoi na `dyn Device` i na katalogu PTX,
// więc kompiluje się wszędzie i odmawia dopiero tam, gdzie nie ma artefaktów.
mod cuda_exec;
pub use cuda_exec::CudaExec;
#[cfg(any(feature = "metal", feature = "metal-check"))]
pub use dense_exec::MetalExec;
#[cfg(any(feature = "metal", feature = "metal-check"))]
pub mod msl;
// Czesc wspolna rejestru (Problem, Variant, Registry) kompiluje sie wszedzie;
// listy form sa per platforma, bo to platforma decyduje, ktore w ogole istnieja.
mod registry;
pub mod variant;

pub use launchers::{
    nvfp4_ct_physical_m, DeltaStateLayout, DensePrefillLogitsKind, Kernels, MixedQuant,
    Nvfp4CtProjection, Nvfp4CtS0View, Nvfp4GgufLayout, Nvfp4GgufQ8Projection, Q8ActPrepared,
    Q8PreparedProjection, SAMPLE_MAX_TOPK, SAMPLE_MAX_VOCAB, SAMPLE_SCRATCH_PAIRS,
};
pub use launchers::gemm::mxf4::{MmaKind, MMA_RATE_OPS};
pub use registry::{KernelArtifacts, Manifest};
