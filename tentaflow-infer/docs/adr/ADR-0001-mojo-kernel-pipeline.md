# ADR-0001: Mojo kernel pipeline (AOT PTX + manifest, Rust-owned launch)

Date: 2026-07-16 · Status: accepted

## Context

The spec mandates 100% of GPU kernels in Mojo (one codebase, multi-target) with
Rust owning the entire hot path (streams, graphs, launches). Mojo 1.0.0b3
nightly is provisioned via pixi in `kernels/mojo/` and verified working on the
RTX 4090 (SM 8.9): GPU kernels execute, and
`DeviceContext.compile_function[kernel, dump_asm=Path(...)]()` emits clean PTX
with a deterministic `.visible .entry` symbol.

Mojo 1.0 beta syntax notes (differs from most published examples):
`fn` is removed (use `def`), `comptime` replaces `alias`, stdlib paths are
`std.gpu.host` / `std.gpu`, mutable-origin pointers are
`UnsafePointer[T, MutAnyOrigin]`, tensors are `TileTensor` from `layout`.

## Decision

1. Kernel sources live in `kernels/mojo/src/` — one file per op family,
   compile-time parameterized (dtype, quant, tile, arch capability).
2. `kernels/mojo/build_kernels.mojo` (driver run via `pixi run mojo`) compiles
   every (kernel, specialization) pair and dumps PTX per target arch into
   `kernels/build/<arch>/<op>__<variant>.ptx`, plus `manifest.json` with:
   entry symbol (parsed from PTX), param layout, block/smem hints.
3. Rust (`forge-kernels`) loads PTX via the CUDA driver API (cudarc) at startup,
   caches modules, and launches on Rust-owned streams/graphs. No Mojo runtime
   is linked into the server binary.
4. The kernel registry keys `(op, dtype, quant, arch, shape-bucket)` map to PTX
   entries. A vendor-lib slot (cuBLASLt) exists solely as the sanctioned GEMM
   performance safety net; a CUDA-C/NVRTC baseline slot may hold a registry key
   only until its Mojo port lands and passes golden + perf gates.
5. ROCm/Metal targets reuse the same Mojo sources with different dump targets
   (AMDGPU/AIR) when those backends open (plan chunks 9+).

## Consequences

- The Mojo toolchain is a build-time dependency only; deployment artifacts are
  PTX + JSON, versioned and reproducible.
- Kernel ABI = plain C pointer/scalar params; golden tests compare Mojo-kernel
  output vs CPU reference dequant/math from `forge-formats`.
- Nightly Mojo breakage cannot take down the runtime — regenerated artifacts
  are committed, and the previous PTX keeps working.
