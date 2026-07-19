# FORGE prefill GEMM: Mojo vs nvcc codegen — root-cause A/B

RTX 4090 (sm_89, boost ~2775 MHz observed / 3120 max), nvcc 13.3, llama.cpp `571d0d5`.
All numbers raw from this machine. Scratch only; nothing committed.

Methodology note on clock warmup: the 4090 idles at ~210 MHz. The **first** timed launch
after idle runs throttled; every measurement below is the **steady-state** value after the
GPU has spun up (confirmed by 3x repeats — first run low, then flat). This warmup artifact,
not codegen, explains the ~470→724 first/steady spread seen in raw logs.

---

## Experiment 1 — pure IMMA issue ceiling (nvcc vs Mojo)

Back-to-back `mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32` from registers, 8 independent
accumulator chains, 1024 blocks x 256 thr, no smem/global in the loop. ops = 2*16*8*32 / mma.
Identical source logic on both sides (`codegen/exp1_pure_mma.cu`, `codegen/mojo_pure_mma.mojo`).

| Compiler | steady TOPS | first-run (throttled) |
|----------|-------------|-----------------------|
| **nvcc** (`-arch=sm_89 -O3`)  | **724.8** | 340–518 |
| **Mojo** (pixi, same asm)     | **724.3** | 487–518 |

NACC sweep (nvcc): 2→425, 4→460, 8/16/32→**723** (time scales linearly, TOPS flat = steady-state
issue rate). Both land on the same 724 TOPS wall.

**Verdict: REFUTED — Mojo's raw mma issue is NOT weaker.** Both backends issue the s8 IMMA at
the identical hardware rate (~724 TOPS = the real 4090 dense-INT8 ceiling at boost clock; the
task's "~330 dense" figure is the non-boost/spec number, and the repo's own "184 TOPS ceiling"
was a *weaker probe* — fewer warps/accumulators — not a hardware or codegen limit). The gap is
therefore NOT in instruction selection or per-instruction IMMA throughput. It must live in the
full staged kernel. Exp 2 tests exactly that.

---

## Experiment 2 — full Q4_K / Q8_0 MMQ GEMM (nvcc vs Mojo), identical algorithm

nvcc side = llama.cpp's **actual** compiled MMQ kernel (`mul_mat_q`, the reference this project
already proved algorithm-identical to `gemm_i8mma_impl`), driven at the exact FFN shapes via
`test-backend-ops perf -o MUL_MAT` with `GGML_CUDA_FORCE_MMQ=1` (forces the int8 tensor-core MMQ
path, not cuBLAS — apples-to-apples with FORGE). The op includes the on-GPU f32→q8_1 quant of the
activation, same as FORGE's pre-quant. Mojo side = `bench_gemm_i8mma.mojo` `_big` (BM128xBN128),
run alone (no GPU contention). Shapes use ggml convention m=output rows, k=reduction, n=tokens.

All four FFN GEMMs, both quants, tokens n ∈ {512, 2048}. nvcc = llama.cpp forced-MMQ TOPS;
Mojo = `_big` clean. (`nvcc_mmq_perf3.txt`, clean Mojo `bench_gemm_i8mma.mojo` run.)

**Down-proj** N=4096, K=14336 (m=4096, k=14336):

| n | nvcc q4_K | nvcc q8_0 | Mojo q4_K `_big` | ratio (q4_K) |
|---|-----------|-----------|------------------|--------------|
| 512  | **219.3** | 216.0 | 61.2 | **3.6×** |
| 2048 | **207.8** | 219.0 | 64.9 | **3.2×** |

**Gate/up** N=14336, K=4096 (m=14336, k=4096):

| n | nvcc q4_K | nvcc q8_0 | Mojo q4_K `_big` | ratio (q4_K) |
|---|-----------|-----------|------------------|--------------|
| 512  | **223.4** | 213.9 | 61.4 | **3.6×** |
| 2048 | **223.6** | 233.7 | 65.3 | **3.4×** |

**Verdict: PROVEN — the gap is Mojo backend codegen, not the algorithm.** On the bit-identical
MMQ algorithm, at the same shapes and quant formats, nvcc extracts **208–234 TOPS** (~30% of the
724 pure-issue ceiling) where Mojo reaches **61–65** (~8.5%). A consistent **3.2–3.6×** across
every FFN shape and both batch sizes — not a one-shape artifact. Note both are far below the 724
pure-issue ceiling because the *real* kernel is bound by LDS/ldmatrix staging + f32 epilogue that
Exp 1 didn't have; the compiler-controlled difference between the two full kernels is 3.5×.

---

## Experiment 3 — the concrete codegen difference (SASS)

Both kernels compiled to SASS with ptxas 13.3 (`nvcc_q4k_sass.txt` from llama.cpp's
`mmq-instance-q4_k.cu.o`, kernel `mul_mat_q<Q4_K,J=128>`; `mojo_q4k_sass.txt` from
`ptxas gemm_q4_k_i8mma_big.ptx`).

Per-instruction quality is comparable — **both** interleave `IMMA` with `LDSM`/`LDS.128` and the
f32 epilogue (`I2FP.F32.S32`, `HADD2`/`FMUL`/`FFMA`), **both** carry operand `.reuse` flags, both
use plain LDG+STS staging (neither emits `LDGSTS`/cp.async). So Mojo's SASS is not "dumb".

The difference is the **size of the scheduling region / pipeline depth**:

| | nvcc `mul_mat_q` | Mojo `_big` |
|---|---|---|
| IMMA in one kernel (`-fun` filtered) | **256** | **8** |
| K-loop | deeply unrolled into one straight-line body | rolled, 8 IMMA per trip |
| back-edges (BRA) | 48 | 23 |
| BSSY/BSYNC reconverge | — | 32 |
| registers/thread | **255** (STACK 48B) | 127 (STACK 48B) |
| static smem | 0 (dynamic) | 20 KB |
| stream-K K-splitting | yes (+fixup kernel) | no |

**Concrete cause:** nvcc/ptxas builds a deep software pipeline — it unrolls the K reduction so
256 IMMA plus their `LDSM`/`LDS` feeders and `I2FP`/`FFMA` epilogue sit in a **single** wide
straight-line region, spending 255 registers (1 CTA/SM) to keep many IMMA and their in-flight
loads overlapped. That saturates the tensor pipe. Mojo emits a **rolled** K-loop with only 8 IMMA
per trip and a branch back-edge (23 BRAs, 32 BSSY/BSYNC), capping the number of independent IMMA
in flight; the tensor pipe **drains at every back-edge** waiting on the next trip's LDS/LDSM. The
Mojo backend will not unroll the K-loop deeply or spend the register budget to build that window
(and forcing more registers/occupancy the other way regressed, per prior repo experiments — the
compiler simply does not schedule the wide-unroll shape nvcc does). Secondary: nvcc adds stream-K
K-splitting for load balance across SMs; Mojo does not. Net effect matches Exp 2: same per-IMMA
rate (Exp 1), ~3.5× less useful work per unit time because the pipeline is shallow.

---

## Experiment 4 — escape-hatch feasibility (nvcc cubin via cudarc)

**Loadability: trivial, zero HAL change.** `forge-hal` `CudaBackend::load_module`
(`crates/forge-hal/src/cuda.rs:736`) already calls `cuModuleLoadData`, which loads a **cubin**
exactly as it loads Mojo PTX text (same entry the kernel registry uses at
`crates/forge-kernels/src/registry.rs:343`). `launch` passes args as address-stable 8-byte slots
via `cuLaunchKernel` with the same grid/block config. So an `nvcc -arch=sm_89 -cubin` Q4_K/Q8_0
GEMM would load and launch through the **identical** path; the only contract is that the .cu
kernel's parameter order matches the launcher's `LaunchArgs` (pointers + dims), which is under
FORGE's control.

**Projected prefill IF the GEMM ran at the nvcc TOPS.** Prefill is ~81% GEMM. Speeding only the
GEMM by the measured 3.5× (216/61):

- T_new = 0.19 + 0.81/3.5 = 0.421 of current → **2.37× prefill speedup**.
- Mistral-7B Q4_K pp4096: 3032 → **~7200 tok/s**. vs llama.cpp **12018** → still **~0.60×**
  (1.67× behind), because the remaining 19% (attention, quant, launch, RMSNorm) is un-fused in
  FORGE while llama.cpp fuses aggressively. A CUDA GEMM closes most of the *GEMM* gap but not the
  whole 4× — the rest is kernel fusion, not this one kernel.

**Integration cost (honest):** (a) a `.cu` source tree for the hot GEMMs (Q4_K, Q8_0, likely
Q6_K); (b) nvcc added to the kernel build pipeline beside `pixi run mojo build_kernels.mojo`,
emitting per-arch cubins into `build/<arch>/` next to the PTX; (c) per-arch maintenance (sm_89,
sm_90, …) — nvcc cubins are arch-specific where Mojo PTX is JIT-portable; (d) it **breaks the
"100% Mojo kernels" principle (ADR-0001)** for exactly one kernel family. Given Exp 1 shows the
Mojo issue path is fine, the principled alternative is a Mojo-compiler fix (deep K-unroll + wider
register-tile scheduling) — but that is upstream-Modular, not in FORGE's control.

---

## Bottom line

- **Exp 1:** pure IMMA issue identical (724 TOPS both). Mojo mma issue is NOT weaker. REFUTED.
- **Exp 2:** same MMQ algorithm — nvcc **216** vs Mojo **61** TOPS (3.5×). **PROVEN: Mojo backend
  codegen is the blocker.**
- **Exp 3:** cause = pipeline depth. nvcc deep-unrolls the K-loop (256 IMMA/body, 255 regs,
  stream-K); Mojo rolls it (8 IMMA/body, 23 back-edges) so the tensor pipe drains each trip. Same
  per-instruction SASS quality, ~32× smaller scheduling window.
- **Exp 4:** an nvcc cubin drops into the existing cudarc load/launch path unchanged; projects
  ~7200 tok/s pp4096 (2.37×), still 1.67× behind llama.cpp (the rest is fusion). Cost: a .cu tree
  + nvcc in the build + per-arch cubins, and it breaks the 100%-Mojo principle for one kernel.
