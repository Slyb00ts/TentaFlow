# ADR-0002: Kernel artifact loading & caching strategy

Date: 2026-07-16 · Status: accepted

## Context

Kernels arrive as AOT PTX (ADR-0001). The engine must start fast, support many
(op × dtype × quant × arch × bucket) variants, and never JIT in the hot path.

## Decision

1. PTX artifacts + `manifest.json` are embedded in the `forge-kernels` crate
   via `include_bytes!` for the default arch set, and can be overridden from a
   directory (`FORGE_KERNEL_DIR`) for development iteration without rebuilds.
2. Module loading (`cuModuleLoadData`) happens lazily per (module, device),
   memoized in a registry-owned map; first-use latency is hidden behind engine
   warmup, which touches every kernel the loaded model's plan needs.
3. The driver-level JIT cache (PTX→SASS) is additionally pinned by shipping
   PTX targeted at the exact `sm_XX` of supported arches when known; unknown
   arches fall back to the closest lower PTX target.
4. Autotune results (variant choice per shape-bucket) persist in
   `~/.cache/forge/autotune/<gpu-model>.json`, shipped defaults for known GPUs.

## Consequences

- Zero compilation of any kind at request time.
- Kernel development loop: edit .mojo → `pixi run mojo build_kernels.mojo` →
  restart server with `FORGE_KERNEL_DIR=kernels/build`.
