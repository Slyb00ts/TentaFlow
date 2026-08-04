// ===== File: lib.rs — forge-kernels: kernel artifact registry and typed launchers =====
// Kernels are authored in Mojo and shipped as AOT PTX + manifest.json
// (ADR-0001). This crate loads those artifacts onto a HAL device and exposes
// typed launch wrappers so the engine never touches raw entry symbols.

mod launchers;
#[cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]
pub mod cpu_matmul;
#[cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]
mod dense_exec;
// Wzorzec hostowy jest DOSTĘPNY WSZĘDZIE i to jest jego sens: kontrakt, który
// da się uruchomić bez akceleratora, można sprawdzić na każdej maszynie.
mod host_exec;
pub use host_exec::HostExec;
#[cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]
pub use dense_exec::MetalExec;
#[cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]
pub mod msl;
// Czesc wspolna rejestru (Problem, Variant, Registry) kompiluje sie wszedzie;
// listy form sa per platforma, bo to platforma decyduje, ktore w ogole istnieja.
pub mod variant;
mod registry;

pub use launchers::{
    nvfp4_ct_physical_m, DeltaStateLayout, DensePrefillLogitsKind, Kernels, Nvfp4CtProjection, Nvfp4CtS0View,
    MixedQuant, Nvfp4GgufLayout, Nvfp4GgufQ8Projection, Q8ActPrepared, Q8PreparedProjection, SAMPLE_MAX_TOPK,
    SAMPLE_MAX_VOCAB, SAMPLE_SCRATCH_PAIRS,
};
pub use registry::{KernelArtifacts, Manifest};
