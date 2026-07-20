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

### Finding E — fp8 (e4m3) MEASURED on Ada: 305–326 TFLOPS, beats the CUDA MMQ (2026-07-20)

The projection in Finding D is now a measurement. The unblock is a supported MAX
mechanism, not a hack: `libmax` documents `MODULAR_NVPTX_COMPILER_PATH` ("For older
hardware, set MODULAR_NVPTX_COMPILER_PATH to use an external ptxas binary"). Mojo's
ptxas is otherwise statically embedded in `libNVPTX.so` (hashed `libnvptxcompiler_static_*`
symbols — not LD_PRELOAD-interposable), so this env var is the only interception point.
Pointing it at `kernels/mojo/scripts/ptxas_fp8_shim.sh` — a wrapper that rewrites the
input PTX `.version 8.[0-3]` → `8.4` and forwards every arg to the real `ptxas 13.3` —
makes the JIT `run` path assemble the fp8 mma and execute. The `.version` lift is the
whole fix (Ada has 4th-gen fp8 tensor cores; ptxas 13.3 supports ISA 8.4); no kernel
semantics change. Reproducible: `MODULAR_NVPTX_COMPILER_PATH=$PWD/scripts/ptxas_fp8_shim.sh
pixi run mojo scratch/bench_modular_fp8.mojo`.

Measured (`multistage_gemm`, `MatmulConfig[e4m3,e4m3,f32](block=128×128×64, warp=64×64×64)`,
transpose_b, RTX 4090 idle, best-of-40 steady state, correctness bit-exact vs CPU on
e4m3-representable inputs — max_rel_err 0.0):

| shape (T, N, K) | role | fp8 e4m3 TFLOPS | vs CUDA MMQ 208 | vs bf16 170 |
|---|---|---|---|---|
| 2048, 4096, 14336 | down-proj | **305–326** | **1.47–1.57×** | 1.9× |
| 2048, 14336, 4096 | gate/up | **306–314** | 1.47–1.51× | 1.9× |
| 512, 14336, 4096 | gate/up | 227–245 | — | 1.7× |
| 512, 4096, 14336 | down-proj (skinny-M) | 80–162 | — | ~1× (both tile-bound) |

**Decisive go/no-go: fp8 CLEARS the CUDA MMQ (~208) by ~1.5× at prefill batch T=2048,
while staying 100 % Mojo.** This is the first concrete path to retire the ADR-0001 CUDA
GEMM exception AND go faster — the int8-on-Ada conclusion (Exp 1–5) is unchanged and
consistent; fp8 simply uses a different, hardware-superior instruction (`QMMA.16832.E4M3`)
that the int8 path never had. Q4_K→fp8 requant fidelity (offline, 6.4 M weights): relative
L2 = 2.15 %, max per-weight error < 0.5 of one Q4_K level — fp8 preserves an already-4-bit
source to sub-level precision, so it should avoid W4A8's +25 % PPL (that came from the
QServe per-row int8 stage-1, not bit-width). Full engine integration + PPL/prefill gates
are the remaining work (need the Mistral Q4_K GGUF).

### Finding F — fp8 SHIPPED as a real FORGE kernel: near-lossless quality, but SLOWER e2e than the CUDA MMQ on Ada (2026-07-20)

Findings D/E measured Modular's `multistage_gemm` in isolation and projected a 1.5× win.
Phase-2 wired fp8 into FORGE's prefill as a real, committed kernel and ran the end-to-end
gates. **The isolated 1.5× does NOT survive to the engine — fp8 is 19–26 % SLOWER e2e than
the CUDA MMQ default across every shape.** The contradiction is now fully explained.

What shipped (`FORGE_GEMM=fp8`, dense GGUF only): `kernels/mojo/src/gemm_fp8.mojo` — a
single-PTX e4m3 tensor-core GEMM (m16n8k32 `mma.…f32.e4m3.e4m3.f32`, per-row weight scale +
per-token activation scale, f32 accumulate over full K, scale at the epilogue), plus
`quantize_act_fp8`. Committed PTX is self-contained: `build_kernels.mojo` bumps the fp8
kernels' `.version` to 8.4 (`_finalize_fp8`), so the driver JIT (CUDA 13.3) accepts them with
NO runtime shim — the shim is only for `mojo run` of scratch/tests. Kernel correctness: matches
an exact CPU fp8 reference to max_rel_err **0.0012**, all three tile shapes bit-identical
(`kernels/mojo/test_gemm_fp8.mojo`).

**Quality — PASS (decisive), Mistral-7B Q4_K_M, held-out passage:**

| backend | perplexity | mean_nll | Δ vs default |
|---|---|---|---|
| default (Q4_K MMQ) | 30.3113 | 3.41152 | — |
| fp8 (e4m3) | 30.5211 | 3.41842 | **+0.69 %** |

Near-lossless, matching the 2.15 % relL2 fidelity prediction and nothing like W4A8's +25 %.
Coherence identical to the default (Eiffel→"Paris, France"; on a degenerate greedy code prompt
fp8 and default emit byte-identical output). **The per-row fp8 weight scale + per-token
activation scale scheme is validated — e4m3's exponent absorbs the block-to-block spread, so
no per-block scale or SmoothQuant calibration is needed.**

**Perf — fp8 LOSES e2e on Ada (`forge bench …--prefix-cache off`, RTX 4090 idle):**

| shape (pp/dec) | default prefill tok/s | fp8 prefill tok/s | fp8 vs default | decode (unchanged) |
|---|---|---|---|---|
| 512 / 128 | 3014.7 | 2241.9 | **−26 %** | 148 → 175 |
| 4096 / 2048 | 8027.6 | 6471.6 | **−19 %** | 146.3 (=) |
| 8192 / 1024 | 7953.2 | 6301.1 | **−21 %** | 130.6 (=) |

**Root cause (nsys `cuda_gpu_kern_sum`, pp4096 tokens=2):**
- The fp8 GEMM itself is the regression, NOT the added activation quant. Per-launch median:
  **fp8 GEMM `_big` 671 µs vs CUDA MMQ `mmq_sk_q4k_x128` 267 µs — the fp8 kernel is ~2.5×
  slower.** `quantize_act_fp8` is only **3 %** of GPU time (15 µs median/launch), so fusing it
  into the preceding norm (the RMSNorm→q8_1 trick) would recover ~3 %, nowhere near the 19–26 %
  gap. Suspects (a) act-quant overhead and (d) an fp8 scale/f16 epilogue are ruled out; the gap
  is pure GEMM throughput.
- **Two structural reasons the fp8 GEMM is slow, both specific to this pass:**
  1. **On Ada, fp8 and int8 tensor cores run at the SAME peak rate** (RTX 4090: ~660 dense
     TOPS int8 = ~660 dense TFLOPS fp8). The 2× fp8 advantage only exists on Hopper/Blackwell.
     So fp8 gives **zero hardware throughput edge over the int8 MMQ on this GPU.**
  2. The **305 TFLOPS in Finding E was Modular's `multistage_gemm`** — a deeply-tuned kernel
     (multi-stage `cp.async` pipeline, swizzled shared layouts, tuned tile) — vs the CUDA MMQ
     (208). That 1.5× was a **kernel-engineering gap between those two specific kernels**, not
     an fp8-vs-int8 property. `multistage_gemm` is a **host-side dispatcher that enqueues an
     inner kernel**; it CANNOT be AOT-compiled into FORGE's committed-PTX-only model (ADR-0001)
     without shipping the Mojo runtime, or reproducing its launch geometry (grid/block/dynamic
     smem/split-K) in Rust by hand (fragile, deep). The shippable single-PTX kernel written here
     mirrors the simpler hand int8-MMQ structure (synchronous smem staging + barriers, no
     cp.async multistage, no stream-K) — which is exactly why the **Mojo int8 MMQ already loses
     to ggml's stream-K CUDA MMQ**, and the fp8 twin loses for the same reason.

**Verdict: fp8 does NOT beat the MMQ default end-to-end on the 4090, at any shape — keep it
non-default; do NOT retire the CUDA MMQ exception.** Removing the activation-quant overhead
(fuse into the norm) cannot close a 2.5× GEMM gap. fp8 would win only (i) on Hopper/Blackwell
(2× fp8 rate), or (ii) if the FORGE fp8 kernel is rewritten with cp.async multistage + stream-K
to match/beat ggml MMQ — substantial kernel work with an uncertain Ada payoff given (1). fp8
stands as a **proven-correct, near-lossless alternative backend** and the quality/build-wiring
groundwork for those future GPUs; the CUDA MMQ remains the justified default on Ada.

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
  2. **fp8 path — the real win, now MEASURED (Finding E).** Modular's multistage fp8 GEMM runs on
     sm_89 at **305–326 TFLOPS = 1.5× the CUDA MMQ** and 1.9× bf16, 100 % Mojo, bit-exact. Unblock
     is the supported `MODULAR_NVPTX_COMPILER_PATH` external-ptxas hook + a `.version 8.1→8.4`
     rewrite (`scripts/ptxas_fp8_shim.sh`). Q4_K→fp8 requant fidelity 2.15 % rel-L2 (< 0.5 Q4_K
     level) indicates it dodges W4A8's +25 % PPL. Follow-up needed: wire the Q4_K→fp8-e4m3 weight
     pack + per-token activation quant + scaled fp8 GEMM behind `FORGE_GEMM=fp8` in the forward
     pass, and run `forge ppl`/`forge bench` (needs the Mistral Q4_K GGUF).
- **Recommendation:** keep the CUDA MMQ as the committed default for now (unchanged this pass).
  Pursue the **fp8 path** as the route to a 100 %-Mojo prefill GEMM that beats CUDA on Ada — it is
  the first concrete way to retire the ADR-0001 exception. bf16 is the safe fallback if fp8
  accuracy proves insufficient. Neither needs int8, so the CODEGEN_PROOF int8 conclusion and these
  new paths are consistent, not contradictory.

---

## Finding G — Modular's `multistage_gemm` INNER kernel AOT-exported + launched from cudarc: 288–351 TFLOPS, no Mojo runtime (2026-07-20)

Finding F concluded that Modular's 305-TFLOPS `multistage_gemm` "CANNOT be AOT-compiled into
FORGE's committed-PTX-only model (ADR-0001) without shipping the Mojo runtime, or reproducing
its launch geometry ... in Rust by hand (fragile, deep)", and shipped instead a **hand-written**
single-PTX fp8 kernel (`gemm_fp8.mojo`) that — being synchronous smem staging, no cp.async
multistage — lost e2e to the CUDA MMQ. **That "CANNOT" is now REFUTED by direct measurement.**
The inner kernel `multistage_gemm` enqueues IS AOT-exportable and DOES keep peak throughput
when launched from FORGE's cudarc/PTX path with zero Mojo runtime.

**What the inner kernel is.** `multistage_gemm` (host dispatcher, `linalg/matmul/gpu/__init__`)
on the NVIDIA "standard GEMM (no split-K)" branch enqueues exactly one bare GPU kernel:
`multistage_gemm_kernel[c_type, CLT, a_type, ALT, b_type, BLT, transpose_b, …, config]`
(`linalg/matmul/gpu/_multistage_gemm_gpu`). It is a normal `@__llvm_metadata`/`@__name` GPU
function — the multi-stage `cp.async` pipeline, `TensorCore` mma, and swizzled shared layouts
all live inside it. No TMA (Ada has none; it uses `cp.async`/LDGSTS), no cluster dims, no
split-K for this config (`num_k_partitions=1`).

**AOT export — the SAME mechanism `build_kernels.mojo` already uses.** Instantiate the inner
kernel at the fp8 FFN config and `ctx.compile_function[kernel, dump_asm=Path(...)]()` — compiles
to PTX WITHOUT executing, identical to how every committed FORGE kernel is emitted. The kernel is
parameterized on the operand `LayoutType`/`linear_idx_type` (taken off `TileTensor` values exactly
as the host dispatcher does). fp8 needs the `.version 8.1→8.4` lift (the embedded ptxas gate);
under `MODULAR_NVPTX_COMPILER_PATH=scripts/ptxas_fp8_shim.sh` `compile_function` completes and
dumps the PTX, then a `.version` rewrite (same as `_finalize_fp8`) makes it self-contained.
Exporter: `kernels/mojo/scratch/build_modular_fp8_gemm.mojo`; PTX in
`kernels/mojo/scratch/modular_ptx/`.

**The exported PTX is fully self-contained — NO Mojo runtime symbol.** `.version 8.4`,
one `.visible .entry`, **zero `.extern .func`** (no runtime calls), no `_mojo_*`/`KGEN`/`malloc`;
the only `.extern` is the standard `.extern .shared` dynamic-smem declaration. 64×
`mma.sync.aligned.m16n8k32.row.col.f32.e4m3.e4m3.f32`, 38 `cp.async.cg`, 24 `ldmatrix`. System
`ptxas -arch=sm_89 -O3` assembles it to a 228-reg cubin with 64× `QMMA.16832.F32.E4M3.E4M3` SASS.
Three parameters, each an 8-byte pointer slot (`c`, `a`, `b`) — the static layout collapses each
`TileTensor` to a bare pointer.

**Launch geometry (replicated in cudarc, NOT fragile).** For `config` block=128×128×64,
warp=64×64×64, stages=4, k_part=1: `num_threads = (128/64)(128/64)(1)·32 = 128`;
`grid = (⌈N/128⌉, ⌈M/128⌉, 1)`; dynamic smem `= 2 · 128·64·4·1 B = 65536 B`. That is the entire
host side — three lines of arithmetic, not a "deep" reproduction. The >48 KB dynamic-smem opt-in
(`CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES`) already exists in `forge-hal` for the MMQ path.

**Measured — standalone cudarc harness, no Mojo runtime** (`scratch/modular_fp8_launch/`,
`CudaContext` + `result::{module,launch_kernel,…}`, RTX 4090 idle-gated, steady-state best-of-40,
correctness bit-exact vs CPU golden on e4m3-representable inputs — **max_rel_err = 0.0**):

| shape (T, N, K) | role | cudarc TFLOPS | vs CUDA MMQ 208 | vs old Mojo int8 66 | Finding E (in-Mojo) |
|---|---|---|---|---|---|
| 2048, 4096, 14336 | down-proj | **350** | 1.68× | 5.3× | 305–326 |
| 2048, 14336, 4096 | gate/up | **338** | 1.63× | 5.1× | 306–314 |
| 512, 14336, 4096 | gate/up | **289** | 1.39× | 4.4× | 227–245 |
| 512, 4096, 14336 | down-proj | **349** | 1.68× | 5.3× | 80–162 (host-overhead-bound) |

**The isolated 305–326 TFLOPS SURVIVES the cudarc launch — 288–351, matching or exceeding
Finding E.** The T=512 skinny shapes are actually FASTER via cudarc than through Mojo's
`DeviceContext.enqueue_function` (349 vs 80–162): for a ~0.17 ms kernel, Mojo's runtime enqueue
overhead (module-manager lookup per launch) dominated; cudarc's thin `cuLaunchKernel` removes it.
This is the opposite of a runtime advantage — the runtime was a tax the AOT path avoids.

**Verdict: the Finding-F wall is false for this kernel on Ada.** A 100 %-Mojo GEMM that BEATS the
CUDA MMQ by 1.4–1.7× is loadable and launchable through FORGE's *existing* cudarc/PTX/HAL path
(load_module already takes cubin or PTX; the smem opt-in already exists), with the kernel PTX
carrying **no** Mojo runtime dependency. Finding F's e2e loss was a property of the *hand-written*
`gemm_fp8.mojo` (synchronous, no cp.async multistage), NOT of "AOT + cudarc" and NOT of fp8 —
using Modular's own multistage kernel removes that gap at the GEMM level.

**Productionization path (prototype proven; engine default unchanged this pass):**
1. **Per-shape PTX vs dynamic-M.** The exported entry bakes M,N,K statically (layout-hashed
   symbol), so prefill T buckets each need a PTX, OR export with a runtime (UNKNOWN) M dimension —
   the kernel already reads `M = c.dim[0]()` at runtime, so a dynamic-M `TileTensor` layout should
   yield one PTX per (N,K); this is the main open item to verify.
2. **Scaling.** This kernel is a plain e4m3×e4m3→f32 GEMM (no scale epilogue). Wiring it into the
   forward pass needs the Q4_K→fp8-e4m3 weight pack (fidelity 2.15 % rel-L2, PPL +0.69 % per
   Findings E/F) + per-token activation quant (`quantize_act_fp8`, already committed, ~3 % of GPU
   time) + a per-row×per-token scale applied via the kernel's `elementwise_lambda_fn` epilogue hook
   (also AOT-exportable) or a fold-after pass.
3. **Build wiring.** Add an `_export`-style call for the inner kernel to `build_kernels.mojo` under
   the same `_finalize_fp8` `.version` lift, emit its per-arch PTX into `build/<arch>/`, and route
   the FFN prefill GEMM to it in `forge-kernels` behind a flag — retiring the ADR-0001 CUDA-cubin
   exception with a faster, 100 %-Mojo kernel.

Everything in this finding is scratch (nothing committed to the engine default); the committed
CUDA MMQ default is untouched.

---

## Finding H — Modular fp8 GEMM PRODUCTIONIZED (`FORGE_GEMM=fp8mod`): committed, near-lossless, but STILL not a clean e2e win on Ada (2026-07-20)

Finding G proved the inner `multistage_gemm_kernel` AOT-exports + launches from cudarc at
288–351 TFLOPS with no Mojo runtime. This pass turns that prototype into a real, committed FORGE
kernel behind `FORGE_GEMM=fp8mod` and runs every gate. **The three Finding-G productionization
items are all resolved; the isolated 1.3–1.5× GEMM win is real and committed — but it does NOT
survive to end-to-end prefill on the 4090, for a reason that is now the ACTIVATION-QUANT
INTEGRATION, not the GEMM.**

### What shipped (all three Finding-G items)

1. **Dynamic-M wrapper — one PTX per (N,K), not per (N,K,T).** The exported static kernel baked
   M,N,K (per-shape symbol). Finding-G item #1 asked whether a runtime-M export is possible. It
   is, cleanly: `src/gemm_fp8_modular.mojo` defines `gemm_fp8_mod[N,K]`, a thin GPU wrapper that
   takes BARE pointers + an `i64 m` (so every kernel param is one 8-byte slot — FORGE's HAL
   contract; the fully-dynamic RuntimeLayout packs `{ptr, M}` into a 16-byte param the 8-byte-slot
   HAL cannot feed) and builds the operand `TileTensor`s device-side with `Coord(m, Idx[K])`
   (dynamic M, static K/N), then calls `multistage_gemm_kernel`. `M` is read at runtime
   (`c.dim[0]()`), so ONE PTX per (N,K) serves ANY token count T. Verified bit-exact incl. odd
   M=100 (not a 128-multiple); T-buckets are therefore UNNECESSARY.
2. **Scale via the epilogue hook — no extra HBM pass.** Finding-G item #2. The kernel's
   `elementwise_lambda_fn` (signature `[dtype, width: SIMDSize, *, alignment](IndexList[2],
   SIMD[dtype,width])`) receives each (row, col) + the f32 accumulator; the wrapper's lambda
   applies `xs[t]·ws[col]` and casts to f16, writing the output directly. So the per-token ×
   per-row scale + f16 downcast are FUSED into the GEMM store — no separate scale/downcast kernel.
   (The kernel still requires `c_type=f32`, so the f32→f16 happens only inside the lambda; the c
   pointer is never dereferenced when a lambda is set, so `y` is reused for it.)
3. **Build-wired + registered.** `build_kernels.mojo` compiles four committed instances
   (`gemm_fp8_mod_{4096_4096, 1024_4096, 14336_4096, 4096_14336}` — Mistral-7B Q/O, K/V, gate/up,
   down) under the same `_finalize_fp8` `.version 8.1→8.4` lift as the hand fp8 kernel, emitting
   self-contained PTX into `build/sm_89/` (0 `.extern .func`, 64× `mma…e4m3`, 38 `cp.async`).
   Registered in `forge-kernels/registry.rs`; launched by `Kernels::gemm_fp8_modular`
   (grid `(⌈N/128⌉, ⌈T/128⌉)`, block 128, dynamic smem 65536 — the >48 KB opt-in the HAL already
   sets); routed by `Model::gemm_fp8` when `weights.fp8_modular` (set for `FORGE_GEMM=fp8mod`).
   The old `fp8` slow path and the CUDA MMQ default are untouched.

### Isolated GEMM — the win is real and committed (dynamic-M + fused scale, RTX 4090, best-of-40)

| shape (T, N, K) | role | fp8mod TFLOPS (dyn-M, no scale) | fp8mod TFLOPS (fused scale, f16 out) | vs CUDA MMQ 208 |
|---|---|---|---|---|
| 2048, 4096, 14336 | down | 304 | 289 | 1.39× |
| 2048, 14336, 4096 | gate/up | 313 | 262 | 1.26× |
| 512, 4096, 14336 | down | 298 | 279 | 1.34× |
| 512, 14336, 4096 | gate/up | 244 | 213 | 1.02× |

Dynamic-M costs ~10 % vs the static 350 (the compiler can't specialize the K-loop bound); the
fused f16 scale-epilogue costs another ~5–15 % over raw f32 out (extra scale reads + f16 cast in
the store). Net still **1.0–1.4× the CUDA MMQ**, 100 % Mojo, and correct (raw max_rel_err = 0.0;
with the f16 scale epilogue 4.2e-4 = f16 rounding).

### Quality — PASS (decisive), Mistral-7B Q4_K_M, held-out passage

| backend | perplexity | mean_nll | Δ vs default |
|---|---|---|---|
| default (Q4_K MMQ) | 30.3113 | 3.41152 | — |
| **fp8mod** | 30.5211 | 3.41842 | **+0.69 %** |

Byte-identical fp8 numerics to the hand `fp8` path (+0.69 %). Coherence PASS on 3 varied prompts
(Eiffel→"Paris, France"; capital of Japan→"Tokyo"; water→"hydrogen and oxygen"). NVFP4 Bielik
golden bit-exact on 1 and 4 lanes (default untouched).

### PERF — fp8mod does NOT beat the CUDA MMQ default e2e, at any shape (RTX 4090, warm best)

Prefill tok/s, `forge bench …--prefix-cache off`. **Both measured warm** (the 4090 idles at
210 MHz; the first post-idle launch is throttled ~2×, so cold single-shots are meaningless — the
default's own pp512 swung 2946→6909 cold→warm). Default = best-of-5 warm; fp8mod = best-of-3 warm
(its 120 s requant leaves the GPU warm, so each launch's single prefill is already steady-state):

| shape (pp/dec) | default (CUDA MMQ) | fp8mod | fp8mod vs default | decode |
|---|---|---|---|---|
| 512 / 128  | 6909 | 2920 | **0.42×** | unchanged (150→174, noise) |
| 4096 / 2048 | 8549 | 8101 | **0.95×** | 146.2 = 146.2 |
| 8192 / 1024 | 8130 | 7640 | **0.94×** | 130.3 = 130.4 |

llama.cpp pp4096 (reconfirmed idle, `-fa 1`): **11991** tok/s. fp8mod 8101 = 0.68× llama.cpp,
0.95× the FORGE default. **Decode never regresses** (separate gemv path; unchanged).

### Why the isolated 1.4× GEMM win evaporates e2e — it is the activation quant, not the GEMM

Contrary to Finding G's optimism ("using Modular's own multistage kernel removes that gap at the
GEMM level" → e2e win), the e2e prefill is flat-to-slower. The GEMM IS faster now (Finding H
isolated table); the loss is elsewhere and specific to the `fp8mod` *integration*:

- **The `fp8mod` prefill re-quantizes the activation PER PROJECTION.** `Model::gemm_fp8` bundles
  `quantize_act_fp8` into every call, so q/k/v quantize the SAME attn-norm output **three times**
  and gate/up quantize the SAME ffn-norm output **twice** — 7 activation-quant passes per layer.
  The **CUDA MMQ default fuses the quant into the RMSNorm** (`rmsnorm_q8_1_ds4`) and **shares one
  quantized activation across q/k/v** (and one across gate/up) — ~4 passes, no redundancy, no f16
  HBM round-trip. At small T the GEMM is tiny and this fixed per-projection quant overhead
  dominates → pp512 collapses to 0.42×; at large T it is a smaller but still net-negative tax.
- **Prefill is only ~81 % GEMM** (Exp 4): even a GEMM at ∞ TFLOPS caps the prefill speedup, and
  the remaining attention/quant/norm tail is un-fused in the `fp8mod` path.
- **On Ada fp8 and int8 tensor cores share the same peak** (Finding F): the multistage kernel's
  edge over MMQ is scheduling, worth 1.3–1.5× in isolation — not enough headroom to also absorb
  the quant redundancy above.

### Verdict + recommendation

> **SUPERSEDED by Finding I (2026-07-20).** Two things below turned out wrong once the
> activation quant was fused into the RMSNorm AND the prefill was measured *truly* warm
> (in-process best-of-N, not separate process launches): (1) the pp512 "0.42×" and pp4096
> "0.95×" were a **cold-measurement artifact** — separate `forge bench` launches let the 4090
> idle back to 210 MHz between runs, so the reported "warm best" never reached steady state;
> (2) with the fusion, fp8mod **beats** the CUDA MMQ default warm. Read Finding I.

- **The Finding-G "AOT + cudarc + dynamic-M + fused scale" mechanism is fully productionized,
  committed, correct, and near-lossless (+0.69 % PPL).** The isolated Modular fp8 GEMM beats the
  CUDA MMQ by 1.0–1.4× as a committed 100 %-Mojo kernel. This is real and reusable — and on
  Hopper/Blackwell (2× fp8 rate) it should win outright.
- **The precise, single blocker to the e2e win** (now RESOLVED in Finding I): de-duplicate the
  activation quant. Split `quantize_act_fp8` out of `gemm_fp8_modular`, quantize the attn-norm
  output ONCE and share `xq/xs` across q/k/v, quantize the ffn-norm output once and share across
  gate/up — fusing the quant into the RMSNorm exactly as the MMQ path's `rmsnorm_q8_1_ds4` does.

## Finding I — fused RMSNorm→fp8 lands the e2e win: fp8mod BEATS the CUDA MMQ default (warm, RTX 4090, 2026-07-20)

Finding H's blocker was fixed exactly as prescribed. New Mojo kernels `rmsnorm_fp8` /
`rmsnorm_residual_fp8` (`kernels/mojo/src/norm.mojo`, mirrors the CUDA `forge_rmsnorm_q8_1_ds4`
fusion) compute rmsnorm(_residual) AND emit ONE per-token e4m3 activation (codes [T,K] +
per-token f32 scale) in the layout `gemm_fp8_mod` consumes. `prefill_forward` (`fp8mod_fuse`
path) now shares that activation: q/k/v read the attn-norm's emit, gate/up read the ffn-norm's
emit — via `gemm_fp8_modular_prequant` (no per-projection requant). o-proj + down keep their own
`quantize_act_fp8` (their input is attention/SwiGLU output, not a norm). Numerics are bit-identical
to the standalone rmsnorm_f16 → quantize_act_fp8 pair (same f16 round point, same absmax/448
scale), so PPL is unchanged (below).

### nsys — the per-projection quant launches dropped exactly as designed (pp4096, 2 reps × 8 chunks)

| kernel | before (per-projection) | after (fused) |
|---|---|---|
| `quantize_act_fp8` | **1792** (7/layer: q,k,v,o,gate,up,down) | **512** (2/layer: o,down only) |
| `rmsnorm_fp8` + `rmsnorm_residual_fp8` | 0 | 8 + 504 (replace the f16 norms) |
| `rmsnorm_f16` + `rmsnorm_residual_f16` | 8 + 512 | 0 |

The 1280 eliminated launches = exactly the q/k/v (3) + gate/up (2) requant passes × 32 layers ×
8 prefill chunks. The fused norm does slightly more work per launch (the quant), but there are
1280 fewer standalone quant kernels and no shared-activation f16 HBM round-trip.

### PERF — the win, measured TRULY warm (in-process best-of-N, `forge bench --reps N`)

The decisive methodology fix: `forge bench` now takes `--reps N`, re-submitting the identical
request through the **already-loaded** engine (prefix-cache off → each rep re-runs full prefill).
Rep 1 is the cold launch (4090 at 210 MHz, throttled ~2×) and is discarded; reps 2..N are genuine
steady state. This is what Finding H's "separate process launch per rep" could never reach — and
it changes the answer.

Prefill tok/s, `--prefix-cache off`, best-of reps 2..N, RTX 4090 idle (<1800 MiB):

| shape (pp/dec) | CUDA MMQ default | fp8mod PRE-fusion | fp8mod POST-fusion | post vs default |
|---|---|---|---|---|
| 512 / 128  | 13289 | ~13.3k (floor) | 13252 | **~1.00×** (launch-bound tie) |
| 4096 / 2048 | 11050 | 11792 | **12036** | **1.089×** |
| 8192 / 1024 | 9029 | 9474 | **9633** | **1.067×** |

- **fp8mod POST-fusion beats the CUDA MMQ default warm: +8.9 % @4096, +6.7 % @8192**, tie @512
  (prefill = 0.039 s there, dominated by launch + the ≥1 decode step, so the GEMM washes out).
- **The fusion itself adds +1.7–2.1 %** over pre-fusion fp8mod (12036 vs 11792 @4096; 9633 vs
  9474 @8192). Notably, pre-fusion fp8mod ALREADY beat the default *warm* (11792 vs 11050) — so
  Finding H's "0.95× loss" was primarily the cold-measurement artifact, and the fusion is the
  clean top-off that also removes the small-T penalty.
- **Cold reproduces Finding H's numbers**: rep-1 (cold) fp8mod @4096 = 8072, default = 8507 →
  0.949× — this is the "0.94×" Finding H reported. It is a first-launch artifact, not steady state.
- **Decode never regresses** (separate graph-replay gemv path): 146.0/146.2 @4096, 130.5/130.4
  @8192, 175.3/175.2 @512 — identical default vs fp8mod.
- llama.cpp pp4096 `-fa 1` reconfirmed **11991**; fp8mod post-fusion 12036 now **matches/edges
  llama.cpp** at pp4096 and beats the FORGE CUDA default.

### Quality — unchanged (+0.69 %), Mistral-7B Q4_K_M held-out passage

| backend | perplexity | Δ vs default |
|---|---|---|
| default (Q4_K MMQ) | 30.3113 | — |
| fp8mod (fused) | 30.5211 | **+0.69 %** |

Identical to the pre-fusion fp8mod PPL — the fusion moves WHERE the quant runs, not the fp8
numerics. Coherence PASS (Eiffel→"Paris, France…"). NVFP4 Bielik golden bit-exact on 1 and 4
lanes (default path untouched).

### Verdict + recommendation (Finding I)

- **The e2e win landed: a 100 %-Mojo fp8 GEMM (Modular multistage + FORGE's fused RMSNorm→fp8)
  now BEATS the committed CUDA MMQ default warm on Ada** — +6.7–8.9 % at the prefill-dominated
  shapes, tie at the launch-bound small shape, near-lossless, decode unaffected. **The technical
  justification for the ADR-0001 CUDA-MMQ exception is retired**: the reason it existed ("no Mojo
  kernel matches the CUDA MMQ on Ada") is no longer true.
- **On flipping the runtime default for Q4_K/Q6_K models: recommended for Ada+ deployments that
  can pay the load cost, but NOT auto-flipped here.** The honest tradeoffs: (1) `fp8mod` requant
  costs **~120 s at model load** and holds the e4m3 packs **in ADDITION to** the Q4_K weights
  (extra VRAM), (2) the very first (cold) request still runs ~0.91–0.95× until kernels warm, (3)
  the win needs a prefill-dominated shape (small pp is a tie). For a long-running server these are
  amortized and the warm win is what matters; for short-lived / VRAM-tight runs the CUDA default
  is still the safer pick. Recommendation: keep the CUDA MMQ as the shipped default, promote
  `FORGE_GEMM=fp8mod` from "isolated-win / e2e-loss" to **"e2e-win on Ada, opt-in for latency-
  insensitive serving"**, and treat the ADR-0001 exception as retired-on-merit (the CUDA kernel
  is now a pragmatic default, not a necessity).
