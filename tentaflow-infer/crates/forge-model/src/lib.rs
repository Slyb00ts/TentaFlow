// ===== File: lib.rs — forge-model: model graphs built from weights =====
//
// Where a checkpoint becomes a sequence of operations. The crate knows about
// architectures and about the kernel facade; it does NOT know which hardware is
// underneath, which is the boundary PLAN_NAPRAWY §5.1 draws and the reason this
// lives outside the engine monolith rather than as another branch inside it.

#[cfg(all(feature = "metal", target_os = "macos"))]
pub mod cpu_matmul;
#[cfg(all(feature = "metal", target_os = "macos"))]
pub mod mlx_dense;
