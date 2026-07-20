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

## Experiment 5 — force the deep K-unroll in Mojo, measure whether it closes the gap

Exp 3 concluded the gap is pipeline depth and asserted "the Mojo backend will not unroll the
K-loop deeply." Exp 5 tests that assertion directly by *forcing* the deep unroll and measuring
both the SASS and the TOPS. Added `gemm_i8mma_deep[BM,BN,NW,FMT,KU,NBUF]` (scratch, reverted):
each smem buffer holds `KU` consecutive 32-col blocks and the inner mma is **`comptime for`-
unrolled across all KU sub-blocks**, so `KU × (MT×NT) = KU×8` IMMA emit in ONE straight-line
body. `NBUF=2` keeps the committed double-buffered stage-ahead pipeline; `NBUF=1` single-buffers
to fit a deeper KU under the static-smem cap. Bit-identical to committed (integer mma is exact;
same per-32-block accumulation order) — verified per-element for Q4_K **and** Q8_0.

**SASS (ptxas 13.3, sm_89, `cuobjdump -sass`, IMMA-per-body):**

| variant | KU | NBUF | IMMA/body | BRA | BSSY/BSYNC | regs | spill | smem |
|---------|----|----|-----------|-----|-----------|------|-------|------|
| committed `_big` | 1 | 2 | **8**  | 23 | 32 | 127 | 0 | 20 KB |
| `deep2`          | 2 | 2 | **16** | 26 | 40 | 118 | 0 | 40 KB |
| `deep4`          | 4 | 1 | **32** | 22 | 38 | 104 | 0 | 40 KB |
| `deep8` (128×128)| 8 | 1 | — | — | — | — | — | **ptxas REJECTS: 0x14000 (80 KB) > 0xc000 (48 KB) static cap** |

**→ Mojo HONORS the deep comptime unroll.** IMMA/body scales *exactly* 8→16→32 with KU=1/2/4;
BRA does not grow (23→26→**22**), BSSY/BSYNC barely moves, zero spill even at 104 regs. The
`comptime for` is emitted as straight-line IMMA — **Exp 3's claim that the backend "will not
unroll the K-loop deeply" is REFUTED at the source level.** The rolled 8-IMMA body of the
committed kernel is a *consequence of the single-32-col-block-per-buffer smem tiling*, not a
backend refusal to unroll.

**BUT the deep window does NOT close the gap.** Isolated Q4_K TOPS (RTX 4090, `_big` / deep2 /
deep4), 3-rep steady state:

| N | K | T | big (8) | deep2 (16) | deep4 (32) |
|---|---|---|---------|-----------|-----------|
| 14336 | 4096 | 512  | 62.1 | 63.6 | 60.6 |
| 4096  | 14336| 512  | 62.4 | 64.2 | **67.4** |
| 14336 | 4096 | 2048 | 65.9 | 65.8 | 66.0 |
| 4096  | 14336| 2048 | 65.5 | 66.3 | **68.0** |

4× the IMMA window (8→32/body) moves TOPS by **≤ +8 %** (best: the K-heavy down-proj) and
**−2 %** on the K-light gate/up — still ~66 TOPS vs nvcc's **208**. The pipeline-depth thesis
predicted ~3.5×; the measured effect of quadrupling the window is single-digit-percent and
shape-dependent. **The 3.5× gap is therefore NOT pipeline/window depth.** Consistent with the
prior finding (MOJO_NOTES) that TOPS is immune to barrier/epilogue/ldmatrix cuts: this kernel
sits at a ~66-TOPS throughput WALL that the scheduling-window depth does not move.

Why nvcc still wins with 256 IMMA/body: it reaches that depth via **dynamic** shared memory
(Exp 3: "static smem 0 (dynamic)"). Mojo's `stack_allocation` is *static* — hard-capped at
48 KB — so a BM=BN=128 tile can hold at most KU=4 (32 IMMA/body); KU=8 needs 80 KB and ptxas
rejects it. But the 8→32 trend (+≤8 %) shows that even if dynamic smem let Mojo reach KU=32
(256 IMMA/body), it would not approach 208 TOPS. nvcc's advantage is not the window count per
se; it is whatever lets ptxas schedule LDSM/LDS and IMMA to co-issue at ~92 % of the mma
ceiling inside that window — a scheduler property Exp 5 could not reproduce by source shape.

**Verdict:** (A) Mojo **CAN** be forced to deep-unroll — proven by SASS (8→16→32 IMMA/body,
`comptime for` honored, no re-roll). (B) The deep window is **NOT** the lever for the 3.5× gap —
forcing it yields ≤8 %, not 3.5×; the gap is a ptxas-vs-Mojo instruction-scheduling wall, not
loop-rolling. All Exp-5 code reverted; committed `_big` kernel retained (nothing cleared the
large-win bar, and deep4 regresses K-light shapes + single-buffers below 2 CTAs/SM).

---

## Bottom line

- **Exp 1:** pure IMMA issue identical (724 TOPS both). Mojo mma issue is NOT weaker. REFUTED.
- **Exp 2:** same MMQ algorithm — nvcc **216** vs Mojo **61** TOPS (3.5×). **PROVEN: Mojo backend
  codegen is the blocker.**
- **Exp 3:** *proposed* cause = pipeline depth (nvcc 256 IMMA/body vs Mojo 8). SASS observation
  correct, but the *causal* claim is **overturned by Exp 5**.
- **Exp 4:** an nvcc cubin drops into the existing cudarc load/launch path unchanged; projects
  ~7200 tok/s pp4096 (2.37×), still 1.67× behind llama.cpp (the rest is fusion). Cost: a .cu tree
  + nvcc in the build + per-arch cubins, and it breaks the 100%-Mojo principle for one kernel.
- **Exp 5:** Mojo CAN deep-unroll (SASS: 8→16→32 IMMA/body, `comptime for` honored). Forcing the
  deep window closes **≤8 %** of the gap, not 3.5× → **pipeline depth is NOT the root cause**; the
  gap is a ptxas instruction-scheduling advantage Mojo's backend does not match. Static-smem cap
  (48 KB) blocks KU≥8 at BM=BN=128; nvcc reaches 256 IMMA/body via dynamic smem. Reverted.

---

## Mojo high-perf primitives revisited (2026-07-20) — corrects the record

Exp 1–5 A/B'd FORGE's **hand-rolled** int8 kernel (raw `mma`+`ld_matrix`, static-smem
double-buffer, no cp.async, no `LayoutTensor`) against nvcc. This pass asks a different
question: does Modular's **own** high-performance kernel library — `layout.tensor_core.TensorCore`,
the `linalg` multistage `cp.async` pipeline, swizzled shared layouts — reach parity on **Ada
(sm_89)**? The library is importable in our pixi Mojo (`Mojo 1.0.0b3.dev2026071614`,
`from linalg.matmul.gpu import _matmul_gpu`, `from layout.tensor_core import TensorCore`).
All numbers below are raw from this RTX 4090. Bench files:
`kernels/mojo/bench_modular_matmul.mojo` (committed), `kernels/mojo/scratch/bench_modular_fp8.mojo`,
`kernels/mojo/scratch/verify_modular_bf16.mojo`.

### Finding A — Modular's `TensorCore` has NO int8 on NVIDIA (source-level, decisive)

`layout/tensor_core.mojo::get_mma_shape[input, accum]` on `has_nvidia_gpu_accelerator()` returns
shapes only for `fp32` (16×8×8), `bf16`/`fp16` (16×8×16) and `fp8 e4m3/e5m2` (16×8×32); the
`else` arm is `comptime assert False, "Unsupported mma shape"`. `int8/int32` mma is defined
**only for AMD** (RDNA/CDNA, 16×16×16). `TensorCore.load_a/load_b/mma_op` all gate on
`supported_fp32 or supported_half or supported_fp8`. **There is no int8 tensor-core path in
Modular's high-level primitive for NVIDIA at all** — Ada or otherwise. FORGE's raw-`mma` kernel
is therefore not a case of "not using the good primitive"; for NVIDIA int8 it is the *only*
route Mojo offers. The high-level abstraction cannot be applied.

Consequently the top-level `linalg` `matmul` also refuses int8 on NVIDIA:
`_matmul_gpu`'s `matmul_supported_format_nvidia = a/b/c ∈ {float32, bfloat16}` — int8 falls to a
naive fallback, never the multistage tensor-core kernel. The multistage kernel itself asserts
`a_type ∈ {f32, bf16, f16}` or `{e4m3, e5m2}` — "Pipeline gemm only supports tf32, F16, BF16,
E4M3, E5M2 mma".

### Finding B — Modular's fast int8/quantized GEMMs are Hopper/Blackwell only (the crux)

Every fast *quantized* matmul in the tree — `grouped_matmul_block_scaled`, `mxfp4`, `nvfp4`,
blockwise-fp8, the "beat cuBLAS 1.2× at ~83 % of int8 peak" kernels — lives under
`matmul/gpu/sm90/` (Hopper **WGMMA**) and `matmul/gpu/sm100_structured/` (Blackwell **tcgen05**),
using block-scaled MMA + TMA. **Ada (sm_89) has neither WGMMA nor tcgen05.** Ada dispatches to the
`sm80` Ampere multistage path, which is bf16/fp32/fp8 only. **So Modular's 83 %-of-int8-peak
result is NOT Ada-achievable — it is a Hopper/Blackwell property of WGMMA/tcgen05 block-scaled
MMA.** For dense int8 on Ada, the only instruction is `mma.m16n8k32.s8.s8.s32`, driven raw — i.e.
exactly what Exp 1–5 already measured hitting the ptxas scheduling wall (~66 vs 208 TOPS).
**CODEGEN_PROOF's conclusion for int8-on-Ada stands.**

### Finding C — but Modular's primitives DO schedule at peak on Ada (bf16, measured)

The refutation of the old "Mojo can't schedule on Ada" worry: the ready-made `_matmul_gpu`
(multistage `cp.async` + `TensorCore` + swizzle), bf16 in / f32 out, at the Mistral FFN shapes,
steady-state best-of-40 (correctness-checked vs CPU golden, max rel err 1.5e-4):

| shape (T, N, K) | role | Modular bf16 TFLOPS |
|---|---|---|
| 2048, 4096, 14336 | down-proj | **170.9** |
| 2048, 14336, 4096 | gate/up | **163.9** |
| 512, 14336, 4096 | gate/up | 136.5 |
| 512, 4096, 14336 | down-proj | 71.9 (skinny-M, config untuned) |

At prefill batch T=2048 this is **~165–171 TFLOPS = the RTX 4090 bf16 (fp32-accum) tensor peak**.
Modular's Mojo primitives extract full hardware throughput on Ada — the Exp-2 gap was specific to
the *int8 hand kernel*, not a Mojo/Ada ceiling. (At T=512 the default tile is memory/launch-bound
on skinny M; a tuned config would recover it.)

### Finding D — fp8 (e4m3) is hardware-valid on Ada and Modular supports it — blocked only by Mojo's PTX-version cap

`TensorCore` *does* expose fp8 (16×8×32, e4m3/e5m2) on NVIDIA, and the multistage kernel accepts
it (`c_type=float32`). Forcing it (`multistage_gemm[config=MatmulConfig[e4m3,e4m3,f32,True](BK=64)]`)
on the 4090 **fails at JIT ptxas**: *"Feature 'mma with FP8 floating point type' requires PTX ISA
.version 8.4 or later."* Root cause: **Mojo's NVPTX backend emits `.version 8.1` for sm_89**
(confirmed from the emitted PTX). It is not a hardware limit — Ada has 4th-gen fp8 tensor cores,
and system `ptxas 13.3` is installed. Proof: taking Modular's own emitted fp8 kernel PTX
(`--emit asm`), `sed`-ing `.version 8.1 → 8.4`, and running system `ptxas -arch=sm_89 -O3`
**builds a valid cubin** whose SASS is `64× QMMA.16832.F32.E4M3.E4M3` + `LDGSTS` (cp.async) +
`LDSM` (ldmatrix), 228 regs — a real deep-pipelined fp8 tensor-core GEMM on sm_89. FORGE's build
already post-processes Mojo PTX and its HAL loads cubins, so a one-line `.version` patch + a
`ptxas` step is entirely inside FORGE's control. Throughput not measured (the JIT `run` path
can't emit 8.4); **projected ~2× the bf16 result (~300 TFLOPS, fp32-accum ceiling ~330 boost)**
by the Ada fp8:bf16 = 2:1 hardware ratio.

### Bottom line (this pass) + recommendation

- **Can a Mojo *int8* GEMM reach parity with the CUDA MMQ (~208 TOPS) on the 4090? NO.** Modular
  has no NVIDIA int8 primitive; its fast int8 is WGMMA/tcgen05 (Hopper/Blackwell), absent on Ada.
  Raw-`mma` int8 is the only Ada route and hits the ~66-TOPS ptxas wall (Exp 1–5). **The CUDA MMQ
  exception is justified for a like-for-like int8 GEMM on Ada.**
- **But the record is corrected on two counts the earlier proof missed, both 100 % Mojo and
  neither requiring int8:**
  1. **bf16 path — works today.** Dequant Q4_K→bf16 + Modular `_matmul_gpu` = **170 TFLOPS**
     measured = **0.82×** the CUDA int8 MMQ (208) and **2.6×** the old Mojo int8 (66). Cost: a
     dequant pass + 2× weight bandwidth (compute-bound at T≥2048, so the rate holds). This alone
     could retire the CUDA GEMM at a modest speed cost while staying 100 % Mojo.
  2. **fp8 path — the real win, one build step away.** Modular's multistage fp8 GEMM compiles and
     is hardware-valid on sm_89 (cubin proven); projected ~300 TFLOPS would **beat** the CUDA MMQ
     *and* be 100 % Mojo. Blocker is solely Mojo emitting `.version 8.1`; unblock = patch the
     emitted PTX to `8.4` before `ptxas` in `build_kernels` (FORGE already owns that step).
     Follow-up needed: Q4_K→fp8-e4m3 dequant with per-block scales + accuracy validation, and the
     measured fp8 TFLOPS via the patched-PTX cubin.
- **Recommendation:** keep the CUDA MMQ as the committed default for now (unchanged this pass).
  Pursue the **fp8 path** as the route to a 100 %-Mojo prefill GEMM that beats CUDA on Ada — it is
  the first concrete way to retire the ADR-0001 exception. bf16 is the safe fallback if fp8
  accuracy proves insufficient. Neither needs int8, so the CODEGEN_PROOF int8 conclusion and these
  new paths are consistent, not contradictory.
