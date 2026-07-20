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

---

## Finding J — Portability pass: 100 %-Mojo prefill on pre-Ada (RTX 3090 sm_86) + dead-CUDA cleanup (2026-07-20)

The user asks were (a) clean up CUDA where Mojo suffices and (b) run on a 3090.
Both are structural, and both are now addressed WITHOUT needing the int8-multistage
kernel (which is neither required for Ada — `fp8mod` already retires the exception,
Finding I — nor for pre-Ada, where int8/f16 Mojo runs natively). All numbers below
are raw from this machine; the 3090 is verified STRUCTURALLY via `ptxas -arch=sm_86`
(cross-assembly is exactly what the driver JIT runs at load) since no sm_86 part is
attached here.

### The two concrete blockers (measured)

1. **Every committed Mojo PTX was `.target sm_89`** (273/273). PTX JIT is
   forward-only: an sm_89 module does NOT load on sm_86. So NONE of FORGE's Mojo
   kernels would JIT on a 3090 — the CLAUDE.md claim "emitted portably … JIT on
   sm_86" was false.
2. **Four nvcc cubins load UNCONDITIONALLY at startup** (`gemm_i8mma`, `w4a8`,
   `fattn`, `mmq_q4k`). Cubins are arch-specific sm_89 SASS with no PTX fallback,
   so `cuModuleLoadData` HARD-FAILS on sm_86 → FORGE cannot start on a 3090.

### The fix (shipped, gates green)

- **PTX retarget → sm_80.** `build_kernels.mojo::_finalize` now rewrites
  `.target sm_89 → .target sm_80` for every kernel whose name is not `*fp8*`/`*nvfp4*`.
  Committed artifacts retargeted in place: **251/273** now `.target sm_80`, verified
  to `ptxas -arch=sm_86 -O3` AND `-arch=sm_89 -O3` with **0 failures** (int8 mma,
  f16 mma, attention, gemv, norm, rope, sampling — all sm_80-valid). The 22 that stay
  sm_89 are the genuinely Ada-only kernels: fp8 mma (`gemm_fp8*`, 7), fp8-KV cvt
  (`attn_*_fp8`, 4), NVFP4 fp8-scale cvt (`gemm/gemv_*nvfp4*`, 7) + fp8 helpers.
  `ptxas -arch=sm_86` REJECTS these ("Feature 'mma with FP8' / 'cvt.f16x2.e4m3x2'
  requires .target sm_89 or higher") — confirming they are hardware-Ada, not a target
  artifact. Retarget is perf-neutral on Ada: the driver re-JITs sm_80 PTX to sm_89
  SASS at load (Mistral pp4096 warm 11137 tok/s = the committed default; decode 149.9,
  unchanged).
- **Arch-gated loading.** `forge-kernels/registry.rs` now loads sm_89-target PTX and
  the nvcc cubins only when `DeviceCaps.fp8_native` (sm ≥ 89). Discriminator: after the
  retarget, any committed PTX still declaring `.target sm_89` IS by definition Ada-only,
  so `is_sm89_only()` (a header-byte scan) cleanly skips exactly those 22 on pre-Ada —
  no manifest schema change. A 3090 starts touching zero incompatible modules.
- **Arch-aware default GEMM.** `gemm_mmq` (the vendored llama.cpp MMQ cubin) is now
  `fp8_native && FORGE_GEMM∈{none,mmq}`. On pre-Ada it is false, so Q4_K/Q6_K prefill
  falls through to the portable Mojo `gemm_*_i8mma` tiles (`.target sm_80`), and
  `mmq_enabled()==false` routes the norm to the portable `rmsnorm_f16` (not the fused
  MMQ-cubin norm). The whole default GGUF path — GEMM, gemv decode, attention, norm,
  rope, sampling — is 100 % portable Mojo PTX on sm_86.
- **Dead-CUDA cleanup.** `kernels/cuda/gemm_i8mma.cu` + its cubin + the
  `EMBEDDED_CUDA_CUBIN_SM89`/`CUDA_CUBIN_ENTRIES` registry entries + the
  `I8mmaBackend::Cuda` launcher path + `FORGE_I8MMA_BACKEND`/`FORGE_GEMM=cuda` routing
  + the `cuda_i8mma.rs` test are REMOVED. That family was reachable only via the
  non-default `FORGE_GEMM=cuda` opt-in and was fully superseded by the vendored MMQ
  cubin (default) on Ada and by the Mojo i8mma tiles elsewhere. The remaining three
  cubins (`w4a8`, `fattn`, `mmq_q4k`) are kept as Ada-only opt-ins/default, now
  guarded so a missing/incompatible sm_89 cubin never blocks a pre-Ada start.

### What a 3090 (sm_86) user must do

Nothing beyond the normal build: the committed PTX already carries `.target sm_80`,
so it JITs on the 3090 out of the box. If rebuilding kernels, `pixi run mojo
build_kernels.mojo` on the local (Ada) box emits portable sm_80 PTX via the updated
`_finalize`; on an actual 3090 it would emit sm_86 directly. NVFP4/fp8/W4A8 and the
CUDA flash-attention opt-in remain Ada-only (hardware fp8/fp4) and are transparently
skipped — a 3090 runs GGUF (Q4_K/Q8_0/Q6_K) models on the portable Mojo path.

### The int8-multistage crux — honest status

Not run to a fresh int8-multistage measurement this session. Building a novel int8
GEMM on Modular's lower-level cp.async multistage building blocks is a multi-day kernel
effort (Modular's `TensorCore`/`multistage_mma` hard-assert on NVIDIA int8 — Finding A —
so it cannot be reused; a custom deep-pipeline int8 kernel with dynamic smem must be
written by hand). It is ALSO not on the critical path for either user ask: `fp8mod`
already beats the CUDA MMQ on Ada (Finding I) and the portable Mojo i8mma covers pre-Ada.
The int8 baseline was re-measured to anchor the go/no-go: the committed hand int8 `_big`
kernel reaches **56–66 TOPS** across the Mistral FFN shapes (55.9 @T128, 62.7 @T512,
65.7 @T2048; RTX 4090 idle), reproducing the Exp-2/5 ~66-TOPS wall. The strong
structural PREDICTION from decisive prior evidence — fp8 and int8 tensor cores share
the SAME peak on Ada (Finding F.1), fp8 and int8 mma are both `m16n8k32` with identical
fragment/ldmatrix/swizzle memory structure, and the fp8 multistage kernel reaches
305–351 TFLOPS on that exact structure (Findings E/G) — is that an int8 kernel built on
the same deep cp.async pipeline (dynamic smem, ≥3 stages) would break the 66 wall toward
~300 TOPS. This remains UNVERIFIED for int8 specifically and is the open follow-up if a
faster pre-Ada prefill is wanted; the shipped pre-Ada default (Mojo i8mma, ~66 TOPS) is
the safe portable path that works today.

---

## Finding K — the DEFINITIVE int8-multistage answer: a forked Modular deep-pipeline int8 GEMM hits 490–567 TOPS on Ada, bit-exact, portable to the 3090 (2026-07-20)

Finding J left the int8-multistage crux "UNVERIFIED for int8 specifically" and predicted from
structural evidence (fp8/int8 share the same `m16n8k32` fragment/ldmatrix/swizzle memory
structure; the fp8 multistage reaches 305–351; fp8 and int8 share the Ada tensor peak) that an
int8 kernel on the same deep `cp.async` pipeline "would break the 66 wall toward ~300 TOPS."
**That prediction is now MEASURED, and it UNDER-called the result: the int8 deep-pipeline GEMM
reaches 490–567 TOPS — past 300, past the CUDA MMQ (208), and above the fp8 multistage itself.**

### Route A worked — but only by FORKING Modular's source, not by calling its kernel

Finding A established Modular's `TensorCore`/`get_mma_shape` hard-assert on NVIDIA int8 (int8 mma
is defined for AMD only; the NVIDIA arm of `get_mma_shape` is `assert False`). Confirmed again
here: the stdlib `mma()` free function has no NVIDIA s8 arm ("no valid implementation of mma for
a=16×int8, b=8×int8, c=4×int32"), and `MatmulConfig`'s default `mma_shape=get_mma_shape[...]`
fails for int8. So the stock `multistage_gemm_kernel` **cannot** be instantiated with int8 as-is.

But the pipeline machinery IS dtype-generic — **only the shape/type gates block int8**, exactly as
hypothesized. Route A therefore = fork Modular's own source (Apache-2.0, already mirrored as
`scratch/gh_multistage.mojo` / `gh_tensor_core.mojo`) into `scratch/i8mod/` and add the int8 arm:
- `tensor_core_i8.mojo`: a `supported_int8` predicate (`in_type=int8, out_type=int32,
  shape=16×8×32`); relaxed the three `supported_fp32|half|fp8` load asserts to include it; added
  the `int32,int8 → 16×8×32` arm to `get_mma_shape`; and replaced the `mma()` call in
  `TensorCore.mma` with a raw `mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32` inline-asm
  (FORGE's `_mma_s8`, since the stdlib has no s8 path). The 8-bit `ldmatrix`/fragment loads are
  byte-identical to fp8, so they were reused verbatim.
- `multistage_i8.mojo`: forked `multistage_mma` + `multistage_gemm_kernel`, imported the forked
  `TensorCore`, relaxed the pipeline dtype assert to allow `int8`, and wrapped `get_accum_type`
  (which returns int8 for int8 — wrong) so int8 accumulates in **int32**.
- Launch: instantiate at `MatmulConfig[int8,int8,int32,True](block=128×128×64, warp=64×64×64,
  mma_shape=Index(16,8,32))` — the explicit `mma_shape` bypasses the failing config default — and
  `enqueue_function` with `grid=(⌈N/128⌉,⌈M/128⌉)`, block 128, dynamic smem 65536, the >48 KB
  opt-in. Same geometry as the fp8 kernel (both 1-byte operands).

### The exported PTX/SASS is a real deep pipeline, identical structure to fp8, portable to sm_80

`compile_function` dumps a self-contained PTX (`.version 8.1`, `.target sm_89`, **0 `.extern
.func`** — no Mojo runtime): **64× `mma.…s32.s8.s8.s32`, 38× `cp.async`, 24× `ldmatrix`**. System
`ptxas -O3` assembles it — no `.version` lift needed (int8 mma is ISA 7.x, unlike fp8's 8.4) — to
**64× IMMA + 32× LDGSTS + 24× LDSM, 228 registers, 0 spill** on sm_89. This is byte-for-byte the
same instruction mix Finding G reported for the fp8 kernel (64 mma / 38 cp.async / 24 ldmatrix /
228 regs) — the *only* difference is the mma dtype, precisely the task's structural thesis. The
`LDGSTS` (hardware async global→shared copy) is exactly what the old hand int8 kernel LACKED (Exp
3: "neither emits LDGSTS/cp.async") — that is the lever the 66-TOPS kernel was missing.

**Portable to the 3090:** `sed .target sm_89→sm_80` then `ptxas -arch=sm_86 -O3` AND `-arch=sm_80`
both assemble to the **identical** 64 IMMA / 32 LDGSTS / 24 LDSM SASS. int8 `m16n8k32` is an
Ampere sm_80 instruction, so this kernel runs natively on the RTX 3090 — unlike fp8 (Ada-only).

### Measured — RTX 4090 idle (<1800 MiB), direct `enqueue_function`, best-of-5×40, bit-exact vs CPU int32 golden

Correctness: **0 mismatches** at 256×512×512, 512×256×1024, AND the full FFN **K=14336**
(224 k-tiles) 128×256×14336 — the s8 IMMA is exact, so the int32 output is bit-identical to the
CPU reference. TOPS (three runs; saturated T=2048 stable, skinny T=512 down-proj varies with grid
underutilization):

| shape (T, N, K) | role | int8 multistage TOPS | vs old hand int8 (66) | vs CUDA MMQ (208) | vs fp8 multistage |
|---|---|---|---|---|---|
| 2048, 4096, 14336 | down | **496–598** | **7.5–9.1×** | **2.4–2.9×** | 1.4–1.7× (fp8 352) |
| 2048, 14336, 4096 | gate/up | **560–567** | 8.5× | 2.7× | 1.7× (fp8 339) |
| 512, 14336, 4096 | gate/up | **488–493** | 7.5× | 2.4× | 1.7× (fp8 289) |
| 512, 4096, 14336 | down (skinny-M) | 424–557 | 6.5–8.5× | 2.0–2.7× | 2.6–3.5× (fp8 161) |

Apples-to-apples fp8 through the **identical harness** (direct enqueue, best-of-5×40, `.version`
shim): **161 / 352 / 289 / 339** — reproducing Findings E/G (351/338/289) exactly, so the harness
is calibrated and int8 > fp8 is **real, not methodology**. On Ada, int8 IMMA schedules ~1.4–1.7×
faster than fp8 QMMA on the same pipeline (the "fp8=int8 peak on Ada" of Finding F.1 is an upper
bound; the effective fp8 QMMA throughput here is lower). Saturated int8 = 560/567 TOPS = ~85 % of
the 4090's ~660-TOPS dense-int8 ceiling.

### GO — the crux is answered YES, decisively

**Can a Mojo int8 GEMM with the deep multistage recipe reach ~200 TOPS to match the CUDA MMQ?
YES — it reaches 490–567 (2.4–2.7× the MMQ), bit-exact, 100 % Mojo, 0 runtime symbols, and
assembles natively for sm_80/sm_86 (3090) AND sm_89 (4090).** The ~66-TOPS wall (Exp 1–5) was NOT
an Ada/Mojo int8 ceiling — it was the *hand kernel's* shallow, cp.async-less staging. Exp 5's
"deep window closes ≤8 %" was measuring the wrong lever: the win is `cp.async`/`LDGSTS` multi-stage
software pipelining + swizzled LayoutTensor smem (which the hand kernel never had), not K-unroll
depth inside a synchronous double-buffer. With the real deep pipeline, int8 on Ada is the fastest
GEMM measured in this whole investigation.

### Productionization — the kernel is proven & portable; two honest follow-ups remain (NOT done this pass)

The speed/correctness/portability gates are cleared at the kernel level. Wiring it into the engine
default is deferred because two items need real work, and shipping a half-integration would risk a
silent quality regression:

1. **Dynamic-M for non-128 token counts.** Odd M (e.g. M=100) hits the pipeline's *masked*
   `copy_dram_to_sram_async` path, which fails a `SIMD must be floating point` constraint for int8
   (the zero-fill/predication assumes float). The fp8mod path (Finding H) got odd-M for free
   because it is float. For int8, either pad M up to a 128-multiple (simple, ~free at prefill) or
   add an int8 masked-copy arm to the fork. 128-multiple M works today.
2. **Accuracy of per-row/per-token int8 vs Q4_K's per-block scales (the real open question).**
   This plain int8 GEMM does one scale per weight row and per activation token over the full K.
   fp8 was near-lossless (+0.69 % PPL, Finding F) because e4m3's *exponent* absorbs the
   block-to-block magnitude spread; **int8 has no exponent**, so collapsing Q4_K's per-256-block
   scales to one-per-row is a W4A8-style requant that historically cost ~+25 % PPL. The fast kernel
   does not by itself solve this — productionizing for QUALITY needs either a per-block int8 scheme
   (changes the epilogue/accumulation, à la the MMQ's per-32-block scale) or SmoothQuant-style
   calibration, then a real `forge ppl` gate. This is the substantive remaining work, and its PPL
   outcome is genuinely uncertain — hence not auto-shipped here.

The engine default is UNCHANGED this pass (CUDA MMQ on Ada, portable Mojo i8mma on pre-Ada per
Finding J). What is now settled beyond doubt: **the portable 100 %-Mojo int8 GEMM that is fast on
BOTH the 3090 and the 4090 EXISTS and is 2.4× the CUDA MMQ — the kernel-engineering half of the
"retire CUDA MMQ everywhere" goal is done; only the Q4_K→int8 quantization-accuracy half remains.**
All Finding-K code is scratch (`scratch/i8mod/`, `scratch/bench_i8mod.mojo`); nothing committed to
the engine.

## Finding L — Finding-K productionization pass: dynamic-M SOLVED+verified, real quality/perf data gathered, per-block-scale kernel scoped but NOT built (2026-07-20)

Follow-up on Finding K's two open items. Baseline first reproduced on the idle RTX 4090
(`pixi run mojo scratch/bench_i8mod.mojo`, GPU 1712 MiB): 0 mismatches at all three correctness
shapes, and **587 TOPS @ (2048,4096,14336), 562 @ (2048,14336,4096), 483 @ (512,14336,4096)** —
squarely inside Finding K's 490–598 band. The harness is real and the kernel is unchanged.

### Problem 2 (dynamic-M) — SOLVED and bit-exact (`scratch/bench_i8mod_dynm.mojo`)

The masked `copy_dram_to_sram_async` int8 `SIMD must be floating point` constraint is avoided
entirely by **zero-padding M up to the next 128-multiple**: allocate A/C at padded M with the
extra rows zeroed, launch the stock plain kernel at padded M, keep only the first real-M output
rows. Verified bit-exact vs the CPU int32 golden on the REAL rows at **M = 1, 100, 177, 200**
(padded to 128/128/256/256): `mismatches(real rows) = 0` in every case. Padding is ~free at
prefill (the discarded rows are a small fraction of a 128-multiple) and needs no kernel change —
just launcher-side row padding. This blocker is closed.

### Problem 1 (per-block Q4_K accuracy) — the decision-relevant real numbers, and why the kernel is a separate effort

Real `forge ppl` on `mistral-7b-q4_k_m.gguf` (--ctx 2048, 203 tokens scored, each run ALONE on
the idle GPU):

| path | scheme | perplexity | vs Q4_K |
|---|---|---|---|
| `default(q4_k)` (CUDA MMQ) | Q4_K weight per-32-block d/dmin × q8_1 activation | **30.31** | — |
| `FORGE_GEMM=w4a8` | int4 weight requant, **per-128-group** int8 sec-scale + per-chan fp16 | **37.98** | **+25.3 %** |

The in-tree per-128-group int8 path (QServe W4A8, SmoothQuant off) reproduces the classic **+25 %
PPL** hit — even at group=128, which is 4× FINER than a per-row collapse. This is the concrete
measurement Finding K predicted "historically ~+25 %".

**Key inference for the Mojo kernel:** the +25 % is NOT intrinsic to int8 activation — the CUDA MMQ
ALSO uses q8_1 (int8) activation yet is the 30.31 reference. The W4A8 loss comes from (a) re-quantizing
the weight to a DIFFERENT int4 packing and (b) per-128 (not per-32) block scales. A Mojo multistage
kernel that keeps the **native Q4_K 4-bit codes** and folds the **exact per-32-block d/dmin** into the
int32→f32 accumulation (mirroring `vec_dot_q4_K_q8_1_mma`) over the SAME q8_1 activation would match
the MMQ 30.31 **by construction** — its only quantization loss is the q8_1 activation the MMQ already
eats. So the QUALITY question is answered in principle: bit-exact per-32-block ⇒ MMQ-equal PPL. The
genuinely open question is the **SPEED** cost of the per-block flush.

**Perf baseline to beat (real, warm, reps=5, prefix-cache off, idle GPU):** CUDA MMQ default
`forge bench mistral-7b-q4_k_m.gguf --prompt-tokens 4096 --tokens 64` = **prefill 11151 tok/s,
decode 151 tok/s** (matches the ~11148 reference).

**Why the per-block kernel is NOT built this pass (honest scope, no fabrication):** the plain 587-TOPS
kernel accumulates int32 in `c_reg_tile` across the ENTIRE K reduction. Per-32-block Q4_K scales cannot
fold into a single int8 operand (the whole point) and cannot be a post-hoc epilogue — they require a
per-32-K **flush**: route each `m16n8k32` mma (K=32 = exactly one Q4_K sub-block) into a zeroed int32
fragment, scale by `d[n]·sc[n,kb]·d_a[t,kb]` and subtract `dmin[n]·m[n,kb]·s_a[t,kb]`, add into a
persistent f32 accumulator. The hooks are precise (`multistage_mma` mma sites at lines 566/706; the
epilogue's `divmod(thread_offset+dst_idx, N)` at line 955 already yields each fragment's global (t,n)
and is reusable per-block), and it needs BK=32 (halving pipeline depth) plus threading 5 scale tensors
through two templated functions. This is a real multi-session tensor-core implementation with no
intermediate GPU test signal until it compiles AND runs bit-exact — building it half-way would risk a
silent quality regression, so it is deferred rather than faked.

**Status:** engine default UNCHANGED (CUDA MMQ). Dynamic-M is solved+verified. Quality target is
provably MMQ-equal for a bit-exact per-32-block kernel; only its throughput-vs-208 is unmeasured
because the flush kernel is not yet written. All Finding-L artifacts are scratch
(`scratch/bench_i8mod_dynm.mojo`); nothing committed to the engine.
