# Mojo 1.0.0b (nightly) — API notes for FORGE kernels

Verified empirically on this machine (RTX 4090, `modular` 26.5 nightly via pixi).
Published tutorials mostly show pre-1.0 syntax — trust these notes over old docs.

## Toolchain
- `cd kernels/mojo && pixi run mojo <file>.mojo` (env pinned by pixi.toml).
- Build artifacts: `pixi run mojo build_kernels.mojo` → `build/<arch>/*.ptx` + `manifest.json`.
- Quick numeric sanity: `pixi run mojo test_kernels.mojo`.

## Language (1.0 beta breaking changes)
- `fn` is REMOVED → use `def` everywhere (kernels included). `alias` → `comptime`.
- `raises` is explicit; `DeviceContext()` and file I/O raise.
- Strings: no `len(s)` / `s[i]` / `s[a:b]` — use `s.byte_length()`,
  `s[byte=i]`, `s[byte=a:b]`, `s.find(needle, start)`.
- `ref` is a reserved keyword (don't use as identifier).
- Pointer args in kernels: `UnsafePointer[Float16, MutAnyOrigin]`
  (`MutableAnyOrigin` is gone; unbound `...` origin makes stores non-mutable).

## GPU stdlib layout
- `from std.gpu import block_dim, block_idx, thread_idx, global_idx`
- `from std.gpu.sync import barrier`
- `from std.gpu.primitives import warp` → `warp.sum(v)` etc. (there is no `std.gpu.warp`)
- Shared memory: `from std.memory import stack_allocation`,
  `from std.gpu.memory import AddressSpace` →
  `stack_allocation[N, Float32, address_space = AddressSpace.SHARED]()`
- Math: `from std.math import rsqrt, exp, sqrt`
- `thread_idx.x` etc. are unsigned — wrap in `Int(...)` before signed arithmetic.

## Host / compile API
- `ctx.enqueue_create_buffer[DType.float16](n)`, `buf.map_to_host()` context
  manager, `buf.unsafe_ptr()`.
- `ctx.enqueue_function[kernel](args..., grid_dim=…, block_dim=…)` (kernel is a
  compile-time parameter).
- PTX dump: `ctx.compile_function[kernel, dump_asm=Path("name.ptx")]()`.
  `dump_asm` is a compile-time parameter — literal `Path("…")` works; dynamic
  paths do NOT (a capturing `def() -> Path` is accepted by the signature but
  capture-convention inference currently fails). Hence build_kernels.mojo dumps
  to static filenames and relocates at runtime.
- `ctx.arch_name()` → `"sm_89"`; entry symbol is mangled — parse
  `.visible .entry <name>(` from the PTX (build_kernels.mojo does this for the
  manifest).
- Generic helper wrapping `compile_function`/`enqueue_function` over a
  `kt: TrivialRegisterPassable` kernel parameter fails `signature_func`
  inference — register kernels with explicit per-kernel calls instead.

## Tensor cores (verified on sm_89)
- `from std.gpu.compute.mma import mma, ld_matrix, st_matrix` — the package
  exists and works; there is no `wgmma`/`mma_arrive`/`TensorCore` in it.
- `mma(d, a, b, c)` with `d/c: SIMD[f32, 4]`, `a: SIMD[f16, 8]`,
  `b: SIMD[f16, 4]` lowers to `mma.sync.aligned.m16n8k16.row.col.f32.f16.f16.f32`
  with the standard PTX fragment layout (lane = group*4+tid4; a pairs along k,
  m rows group/group+8; d pairs along n). Raw throughput measured ~116 TFLOP/s
  on RTX 4090 (f32 accumulate is NOT half-rate through mma.sync).
- `ld_matrix[8](ptr)` = ldmatrix.x4 (f16: 4 8x8 tiles), `ld_matrix[4](ptr)` =
  x2. Lane→address mapping: lanes 0-7 rows of tile 0, 8-15 tile 1, 16-23
  tile 2, 24-31 tile 3; each address is a 16-byte row start.
  - A (m16k16) from row-major [m][k] smem: tiles ordered (m0-7,k0-7),
    (m8-15,k0-7), (m0-7,k8-15), (m8-15,k8-15) — non-transposed.
  - A from k-major [k][m] smem: use `transpose=True` with tile rows = k rows.
  - B (k16n8) from **n-major** [n][k] smem: plain `ld_matrix[4]`
    NON-transposed is already the B fragment (a W row IS the fragment);
    `transpose=True` here is wrong (pairs land along n instead of k).
- `from std.gpu.memory import async_copy, async_copy_commit_group,
  async_copy_wait_group` — `async_copy[16](src, dst)` needs a
  `.address_space_cast[AddressSpace.GLOBAL]()` source and a SHARED dest
  (16-byte aligned both sides). Classic double-buffered pipeline works and is
  MUCH faster than LDG→register→STS staging (which stalls the whole block at
  the stage barrier).
- Mojo `load/store` on shared pointers scalarizes to `ld.shared.b16` unless an
  explicit `alignment=` is given — always pass it (b32/v4 otherwise).
- Perf pitfalls that masked real numbers: an idle RTX 4090 sits at ~210 MHz
  and short benches never reach boost — spin ~300 kernel launches before
  timing (run-to-run swings of 6x otherwise). Guard-heavy inner loops
  (ISETP/BSSY per load) starve the tensor pipe; clamp out-of-range
  tokens/rows instead of branching and zero-fill only the W k-tail.

## Matrix units on RDNA3 / gfx1100 (verified on RX 7900 XT)

- **WMMA is reachable straight from Mojo** — no HIP, no `inlined_assembly`:
  `llvm_intrinsic["llvm.amdgcn.wmma.i32.16x16x16.iu8.v8i32.v4i32", ...]`
  (`i1 signA, <4 x i32> a, i1 signB, <4 x i32> b, <8 x i32> c, i1 clamp`) and
  `llvm.amdgcn.wmma.f32.16x16x16.f16.v8f32.v16f16`. Wrapped in `src/arch_wmma.mojo`.
- **Wave32 fragment layout** (probed exhaustively over all 256 output positions,
  `bench-amd/bench_wmma_gfx11.mojo` guards it):
  - A: lane L carries the WHOLE row `m = L % 16`, 16 bytes of k. Lanes 16-31
    duplicate rows 0-15 (the instruction needs 2x redundancy in wave32).
  - B: lane L carries the whole column `n = L % 16`, 16 bytes of k.
  - C/D: element `(m, n)` lives in lane `16*(m % 2) + n` at index `m // 2`.
  Because a lane wants 16 CONSECUTIVE bytes of its own row, A and B can be read
  straight from global memory when the source is row-major — no LDS staging.
- **Measured ceilings, 7900 XT vs 6900 XT** — RDNA3 demoted the packed-dot
  instructions in favour of WMMA, so the newer card is SLOWER on the old path:

  | primitive | 6900 XT (gfx1030) | 7900 XT (gfx1100) |
  |---|--:|--:|
  | `v_dot4_i32_i8` int8 | **97 TOPS** | **43 TOPS** |
  | `v_dot2_f32_f16` f16 | 48 TFLOPS | 31 TFLOPS |
  | WMMA int8 16x16x16 | brak jednostki | **98 TOPS** |
  | WMMA f16 16x16x16 | brak jednostki | **102 TFLOPS** |
  | DRAM read | 221 GB/s | 674 GB/s |

- **TRAP — `v_dot4_i32_i8` silently computes UNSIGNED on gfx11.** The assembler
  accepts the RDNA2 mnemonic, executes it as `v_dot4_i32_iu8` with `neg_lo`
  cleared, and returns garbage for any negative byte: measured on the card,
  `(-1,-2,-3,-4)·(4,3,2,1)` gave **2540 instead of -20**. Nothing warns. The
  signed form is `v_dot4_i32_iu8 ... neg_lo:[1,1]` (see `src/arch_dot.mojo`);
  `__builtin_amdgcn_sdot4` is NOT available on gfx11 (`needs target feature
  dot1-insts`). Every int8 microbenchmark must include a NEGATIVE case — a
  throughput-only benchmark passed happily while the instruction was wrong.
- **`v_dot2_f32_f16` is 1 ULP inexact on gfx11** where RDNA2 is exact
  (`(-2,0.5)·(8,-16)` → -24.0000019 instead of -24). `v_fma_mix_f32` gives the
  exact result but costs two instructions. Reproduced in plain HIP, so it is the
  hardware, not Mojo.

## Tensor-core flash-attention prefill IS competitive in Mojo (unlike int8-GEMM)
- **The FA counter-example to the int8-GEMM codegen wall.** `attn_prefill_fa_mma`
  (`src/prefill.mojo`, `FORGE_ATTN=fa_mojo`) is a straight Mojo port of the CUDA
  `fattn_prefill.cu`: f16 `m16n8k16` mma for QK^T and P·V, online softmax in
  registers, paged KV + GQA + causal, BQ=64/BK=32/4-warp tiling, byte-identical
  I/O contract. It schedules to **within ~2.7–4.4 % of the nvcc cubin** — NOT the
  3.5× wall the int8-MMQ GEMM hit. Measured (RTX 4090, Mistral-7B Q4_K, nsys
  isolated attention-kernel GPU time): 4096 prefill CUDA 97.9 ms → Mojo 102.2 ms
  (+4.4 %); 8192 CUDA 349.8 ms → Mojo 359.2 ms (+2.7 %). End-to-end prefill tok/s
  is at parity (warm): 512 5727/6053, 4096 7585/8523, 8192 8231/8104 (fa/fa_mojo).
  Decode untouched (separate kernel). **Why FA works where the GEMM did not:** FA's
  hot loop is a SHORT online-softmax reduction (running max/sum, a handful of mma
  per KV tile, immediate f32 epilogue), not a deep-K-unrolled IMMA pipeline whose
  throughput depends on ptxas LDS/mma dual-issue scheduling — the exact lever Mojo's
  backend loses on (Exp 5 / `CODEGEN_PROOF.md`). The pure `m16n8k16` f16 mma was
  already proven full-rate in Mojo, and FA leans on that mma plus scalar softmax,
  so there is no scheduling wall to hit.
- **Mojo mma-FA implementation notes (all worked first-shot):**
  - `from std.gpu.compute.mma import mma, ld_matrix` + `from std.gpu.primitives.warp
    import shuffle_xor`.
  - **QK^T** maps 1:1 onto the GEMM's fragment convention: A = Q from row-major
    `qs[query][head_dim]` via `ld_matrix[8]` (non-transposed, preloaded once/warp);
    B = K from n-major `ks[key][head_dim]` via `ld_matrix[4]` (non-transposed — a K
    row IS the B fragment, same as a W row in the GEMM). `mma(s[nt], qf[kc], bf,
    s[nt])` accumulates over head-dim chunks.
  - **Online softmax:** the mma D-layout has each lane own query rows ra=lane/4 and
    rb=lane/4+8, with the 4 keys of a row spread across `lane&3`. Reduce max/sum over
    those 4 lanes with `max(v, shuffle_xor(v, 1)); max(v, shuffle_xor(v, 2))`
    (`shuffle_xor(v, k)` xor's the lane id — lanes 0-3 all converge, 4-7 converge …).
  - **P·V:** the S-accumulator D-layout equals the mma **A-operand** layout, so P
    needs NO ldmatrix repack — just pack the f32 probs of two adjacent 8-key subtiles
    into one `SIMD[DType.float16, 8]` (elements `[s0_0,s0_1,s0_2,s0_3,s1_0…]` = the
    4 h2 registers a[0:2],a[2:4],a[4:6],a[6:8]). B = V stored **transposed** in smem
    (`vs[head_dim][key]`, scalar scatter on stage) so `ld_matrix[4]` reads it
    non-transposed. `mma(acc[hn], pf, bv, acc[hn])`.
  - Same guard-light staging as the GEMM: `.load/.store[width=8, alignment=16]`,
    clamp out-of-range query rows (masked at write-out) instead of branching.
  - `shuffle_xor` handles Float32 directly. `Float32(HD) ** 0.5` does NOT compile
    (unsupported SIMD pow) — use `sqrt(Float32(HD))`.
- **Verdict:** Mojo FA is a portable-default CANDIDATE (one Mojo source →
  PTX + AMDGPU + Metal per ADR-0001, vs the NVIDIA-only `fattn_prefill.cu`). The
  default stays `fa`=CUDA for now; `fa_mojo` is wired and proven. FA does NOT need
  the CUDA exception the int8-GEMM does.

## int8 MMQ prefill Q4_K = CUDA kernel (nvcc), the ONE ADR-0001 exception
- **Why raw CUDA:** `docs/CODEGEN_PROOF.md` proves the Mojo backend caps the
  int8-MMQ `gemm_i8mma_impl` at ~66 TOPS on the RTX 4090 while nvcc/ptxas
  schedules the *bit-identical* `mma.sync.m16n8k32.s8` algorithm past ~200. Exp 5
  showed the deep K-unroll is NOT the lever (≤8 %) — it's a ptxas instruction
  scheduler advantage Mojo does not match. So the hot Q4_K prefill GEMM is the
  single kernel family that leaves Mojo.
- **Source + build:** `kernels/cuda/gemm_i8mma.cu` (self-contained; Q4_K/Q8_0
  `mma.sync.m16n8k32.row.col.s32.s8.s8.s32` via inline asm + `ldmatrix.x4/x2`,
  fragment addressing + f32 scale/min epilogue mirror `gemm_i8mma_impl` 1:1).
  Build: `kernels/cuda/build.sh [sm_arch]` → `nvcc -arch=sm_89 -cubin` into the
  committed `kernels/mojo/build/sm_89/gemm_i8mma_cuda.cubin` (nvcc must be on
  PATH). Runs BESIDE `pixi run mojo build_kernels.mojo`; does NOT touch the
  Mojo-owned `manifest.json`.
- **Load path unchanged:** the cubin loads through the SAME `Device::load_module`
  → `cuModuleLoadData` used for Mojo PTX (`crates/forge-kernels/src/registry.rs`
  `load_cuda_cubin`, embedded via `include_bytes!`, entries = the `extern "C"`
  symbols). The launcher (`gemm_i8mma_run`) keeps the identical `quantize_act_q8_1`
  prepass + grid/args contract and only swaps the GEMM handle.
- **Result (RTX 4090, same card, A/B via `FORGE_I8MMA_BACKEND`):** isolated Q4_K
  GEMM Mojo 55–65 → CUDA 65–107 TOPS (1.6–1.9×); output **bit-identical to Mojo**.
  Mistral-7B Q4_K prefill 512 2497→3334, 4096 2956→3536, 8192 2477→2930; decode
  unchanged (dp4a GEMV untouched). **Q8_0 stays on Mojo** — its committed i8mma is
  ~120 TOPS, faster than this CUDA kernel on Q8_0, so routing sends only Q4_K
  prefill to CUDA (no regression). ~107 TOPS is ~half llama.cpp's tuned MMQ (208);
  the rest of the gap to llama.cpp (11965 pp4096) is Phase-2 fusion, not this GEMM.

## int8 matmul: dp4a vs tensor cores (prefill GEMM investigation)
- **dp4a works and is used** (`llvm.nvvm.idp4a.s.s` via `llvm_intrinsic`, see
  decode_dp4a.mojo). A tiled int8-MMQ *prefill* GEMM (`gemm_i8_dp4a_impl` in
  gemm.mojo: q8_1-quantize the activation tile, keep weights as native codes,
  accumulate int32 with dp4a, scale per 32-block → llama.cpp's `mul_mat_q`)
  is implemented for Q8_0 + Q4_K and is numerically correct (matches an exact
  CPU dp4a reference to ~5e-4; test_gemm_dp4a.mojo).
- **BUT dp4a does NOT beat the f16 tensor-core GEMM on Ada.** Measured
  (bench_gemm_dp4a.mojo, RTX 4090, Q4_K, T=512): f16 dequant+mma ≈ **60
  TFLOP/s** vs dp4a ≈ **33 TFLOP/s** — dp4a is ~1.8x SLOWER. dp4a issues on the
  CUDA/INT32 pipe; the f16 path offloads the MACs to tensor cores AND does the
  per-element dequant on the CUDA cores in the shadow of the tensor pipe, so it
  wins on large-batch prefill. The gap narrows at small T (T=128: 32 vs 29
  TFLOP/s) but prefill is the large-T regime. Conclusion: the f16-dequant GEMM
  is NOT the prefill bottleneck it was assumed to be; the dp4a MMQ path is kept
  (correct, tested, registered) but the engine deliberately keeps prefill on the
  f16 tensor-core GEMM (routing in forge-engine model.rs `gemm_rows`).
- **int8 TENSOR cores ARE reachable from Mojo — CRACKED.** The blocker (the
  mma's 4x s32 aggregate output `{i32,i32,i32,i32}`) is solved with
  `std.sys._RegisterPackType`, the SAME multi-output marshalling the stdlib
  `mma_nvidia.mojo` uses for f16/fp8 (`_RegisterPackType[Float32,Float32,
  Float32,Float32]` with `constraints="=f,=f,=f,=f,..."`). The previous attempt
  failed because it used a plain `SIMD[int32,4]`/`Tuple` result_type instead of
  `_RegisterPackType`. Working s8 mma (src/gemm.mojo `_mma_s8`):
  `inlined_assembly["mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32 {...}",
  _RegisterPackType[Int32,Int32,Int32,Int32], constraints="=r,=r,=r,=r,r,r,r,r,
  r,r,r,r,r,r", has_side_effect=False](a0..a3, b0..b1, c0..c3)` → read `r[0..3]`.
  Proven bit-exact vs a CPU int8 matmul on ALL FOUR output registers (16x8 tile).
  A fragment = 4x b32 (16 s8), B = 2x b32 (8 s8), C/D = 4x s32, standard
  m16n8k32 layout. Fragments load via `ld_matrix` (b16 view, 32 s8/row = 16 b16;
  the f16 kernel's ldmatrix.x4/x2 addressing works unchanged for s8).
- **Pure s8 mma ceiling ≈ 184 TOPS** on the 4090 via this inline-asm path
  (probe: 8 independent accumulators, back-to-back) — ~3x f16's realizable
  GEMM throughput.
- **int8-MMQ prefill GEMM shipped** (`gemm_i8mma_impl` in gemm.mojo,
  `gemm_q4_k_i8mma`/`gemm_q8_0_i8mma` + bm64). Same MMQ contract as the old
  dp4a GEMM (q8_1 activation quant, native weight codes, per-32-block scale/min)
  but the K=32 MAC runs on the int8 tensor cores. Software-pipelined (1 barrier/
  stage, next-stage quant/unpack overlaps compute), 2-m-tile x N-n-tile warp
  blocking, vectorized SIMD[f32,4] scale epilogue. Correct to ~5e-4 vs an exact
  CPU MMQ reference (test_gemm_i8mma.mojo). The old dp4a GEMM
  (`gemm_i8_dp4a_impl`) is DELETED — i8mma replaced it (routing in forge-engine
  model.rs `gemm_rows`). Decode still uses the dp4a GEMV (unchanged).
- **Perf (RTX 4090, end-to-end prefill tok/s, prefix-cache off):** Mistral-7B
  Q4_K 512: f16 1560 -> i8mma 2110 (1.35x); 4096: 2013 -> 2646 (1.31x); 8192:
  1787 -> 2239 (1.25x). Qwen3-0.6B Q8_0 4096: 15692 -> 19027 (1.21x). Decode
  bit-unchanged (uses dp4a GEMV). NOTE the ISOLATED micro-bench
  (bench_gemm_i8mma.mojo) shows i8mma ~50 vs f16 ~60 TFLOP/s at the big FFN
  shapes — misleading: both are ~30% of their mma ceilings (memory/launch
  bound), and the real mixed-shape prefill (attention + FFN, varied batch)
  wins because i8mma reads half the weight smem and issues half the mma. Still
  ~4x below llama.cpp's fused MMQ (11-12.8k); the remaining gap is staging
  (cp.async / pre-quantized global X) + kernel fusion, not the intrinsic.
- **Pre-quantized global X (q8_1 pre-pass) — SHIPPED, ~+5-7% Q4_K prefill.**
  `quantize_act_q8_1` (gemm.mojo) writes the activation tile as q8_1 ONCE into a
  grow-only global scratch (int8 codes `[T,K]` + per-32-block f32 scale/`d*Σq`),
  then `gemm_i8mma_impl` reads int8 X directly instead of loading f16 X and
  requantizing it in every one of the grid's `ceil(rows/64)` weight-row blocks.
  This halves X read bandwidth and removes the redundant requant. Rust side: the
  scratch + both launches live in `Kernels::gemm_i8mma_run` (launchers.rs), from
  `Pool::Activations` (never reset in the engine → grow-only is safe, no aliasing).
  - **CRITICAL layout gotcha (cost a full regression first):** the per-token
    scale buffers MUST be **block-major `[K/32, T]`**, not `[T, K/32]`. In the
    GEMM consecutive lanes (tid<BM) map to consecutive tokens, so a `[T,K/32]`
    scale load strides by `nb` per lane → 32 uncoalesced 4-byte transactions per
    stage, which made the whole change ~12% SLOWER than the in-register scale it
    replaced. Block-major makes those loads coalesce (consecutive tokens are
    contiguous) and flips it to a win. The int8 code buffer stays `[T,K]`
    row-major (the ld_matrix staging load is per-token-contiguous either way).
  - The pre-pass is negligible (nsys: `quantize_act_q8_1` = 0.0% of prefill,
    ~11 us/launch; the i8mma GEMM is ~74%). Numeric bound unchanged: rel err
    ~4.6e-4 vs exact CPU MMQ (test_gemm_i8mma.mojo, updated to pre-quant + pass
    xq/xd/xsm). bm64 still bit-identical to bm128.
  - **Why only ~+5-7% despite halving the dominant (X) traffic:** for gate/up
    (K=4096, N=14336) X is re-read `ceil(N/64)=224x` = ~0.94 GB int8 vs weights
    ~0.24 GB, so X *is* the bulk of the bytes — yet cutting total traffic ~40%
    gained only single digits. The existing software pipeline (gl 2-stage-ahead
    LDG + sw STS) already hides the load latency, so the GEMM is
    occupancy/mma-issue bound, not memory-stall bound. Corollaries measured/
    reasoned, NOT yet done: (2) cp.async for X (now int8 in global) mostly frees
    a 32-byte register + one STS — small, since the pipeline already hides the
    copy; (3) BN=128 (128 rows/block, halves X re-reads) needs 2-pass W staging
    (256 thr / 4-per-row = only 64 rows/pass) — a real rewrite of the bit-exact
    W path for an expected few-% traffic win that the occupancy limit would cap.
    Both deprioritized: high regression risk on a proven kernel, low expected
    payoff given the profile. The lever for the 4.5x is raising mma-issue
    efficiency / occupancy (register pressure, warp scheduling), not X staging.
  - **Occupancy via `.maxnreg` REGRESSES — do not retry (measured 2026-07-19).**
    `gemm_q4_k_i8mma` (bm128) = 126 regs → 2 CTAs/SM; `gemm_q8_0_i8mma` = 100 regs
    → 2 CTAs/SM. `.maxnreg 85` (ptxas-verified) lifts both to 3 CTAs/SM (q8_0
    spill-free, q4_k 64 B spill), but Mistral 4096 prefill drops 2800 → ~2400 tok/s.
    The kernel hides ld_matrix + f32-scale-epilogue + mma-issue latency with
    per-thread ILP at 2 CTAs/SM; fewer regs kill that ILP. So it is mma-issue/ILP
    bound, NOT occupancy bound — more CTAs/SM is counter-productive. nsys also
    confirmed the whole prefill is compute-bound (kernel GPU time = 99.8% of wall
    at P=4096), so CUDA-graphing the prefill and widening the chunk are both
    no-ops. The remaining real levers: (a) Q6_K down-proj via int8-mma (`FMT==2`;
    care: Q6_K has 16-wide scale sub-blocks vs mma k=32 → two scales per stage),
    (b) fewer `barrier()` (2 k-stages/barrier), (c) fused/persistent prefill.
  - **Barrier / ILP / ldmatrix levers all measured — NONE move large-T prefill
    (2026-07-19).** Implemented and verified bit-identical (integer mma is exact):
    (1) separate mma-issue from the f32 epilogue, (2) 2 k-stages/barrier `CK=2`
    (448→224 barriers, doubled smem 15→30 KB + register prefetch), (3) unroll the
    CK stage loop for cross-stage scheduling, (4) paired B `ld_matrix.x4` loading
    2 n-tiles/instruction (halves B ldmatrix). Isolated microbench (Q4_K bm128):
    all four help ONLY small T (T=128 28.8→37.9 TOPS) and are **flat within ±1%
    at T≥512** (57→59 TOPS = the same ~31% of the 184-TOPS ceiling). Diagnostic:
    deleting the ENTIRE q4_k min-correction epilogue is free (57.1→57.3) — the f32
    epilogue is fully hidden. TOPS is constant across T=512→2048 and immune to
    barrier/epilogue/ldmatrix cuts → large-T is at a throughput/bandwidth WALL for
    this tile shape, not issue/latency bound. End-to-end the combined kernel is
    flat at 4096/8192 and **REGRESSES Mistral 512 prefill −25%** (2346→1754): the
    extra smem/regs drop it below 2 CTAs/SM, invisible at the large-T wall but
    costly for the many short GEMMs of a 512-prefill (same occupancy sensitivity
    as the `.maxnreg` finding). **All reverted; committed kernel kept.** The only
    lever left for the 4.3× gap is architectural: BN=128 rows/block to halve the
    dominant X re-read traffic (X re-read `ceil(N/64)` times) + larger per-warp
    register tiles for a higher mma:load-byte ratio (needs 2-pass W staging + full
    bit-exact reval) — a multi-day rewrite, not a low-risk pass.
    `bench_gemm_i8mma.mojo` was stale (wrong 6-arg signature, didn't compile);
    rewritten to the pre-quant path + INT8-TOPS report.
  - **BN=128 reblock SHIPPED — the tile-shape lever paid off (2026-07-19).**
    `gemm_i8mma_impl` is now parametrized `[BM, BN, NW, FMT]`; the new `_big`
    variant is **BM=128 x BN=128, NW=16 (512 threads / 16 warps)**. The trick
    to enlarge the BMxBN tile WITHOUT exploding registers (which killed every
    prior occupancy attempt): keep the per-warp accumulator FIXED at
    MT=2 x NT=4 = 8 (`SIMD[f32,4]`) and add WARPS, not n-tiles/warp. So the big
    kernel is **127 regs → 1 CTA/SM = 16 warps**, exactly the committed
    2x256-thread = 2 CTAs x 8 warps = 16-warp footprint. Same occupancy, but BN
    doubled 64→128 so X (re-read `ceil(rows/BN)` times) is fetched HALF as often
    and smem is 20 KB x1 CTA vs 15 KB x2 = 30 KB (less). W staging generalized to
    `W_PASSES = BN/(NW*8)` row-passes (here 1); scale/code prefetch regs became
    `InlineArray[..., W_PASSES]`. **Bit-identical to the committed BM=128 kernel**
    (integer mma is exact — proven per-element in `test_gemm_i8mma.mojo`:
    `m[idx] == mbig[idx]` on random Q4_K and Q8_0 tiles). Isolated microbench
    (Q4_K bm128 → big, RTX 4090): T=2048 58 → 65 TOPS (**31% → 35% of the
    184-TOPS ceiling**), wall 4.15 → 3.70 ms; T=512 57 → 61; T=128 35 → 58.
    Wins EVERY point. End-to-end (nsys, Mistral 4096 prefill, total Q4_K i8mma
    GEMM GPU time): **968.8 → 863.2 ms = −10.9 %**.
  - **`_big` is perf-gated, not universal — the coarse block underfills small
    GEMMs.** A 512-thread block halves a GEMM's block count, so it only wins when
    there are enough blocks to keep the ~128 SMs busy at 1 CTA/SM. Two
    tripwires measured: (a) a 512-token prefill chunk (n_tokens=512) regressed
    Mistral prefill **−11 %** (2094 vs 2346) — the whole prefill is tiny and the
    small attention projections underfill; (b) Qwen3-0.6B Q8_0 (rows ≤ 3072)
    regressed its 4096 prefill **−19 %** (15.7k vs 19.4k) — too few row-blocks.
    `gemm_i8mma_tile` (launchers.rs) therefore gates `_big` on BOTH
    `n_tokens >= 1024` (full `MAX_PREFILL_CHUNK`) AND
    `ceil(rows/128)*ceil(n_tokens/128) >= 256` (≥ 2 full waves); else the
    committed 256-thread BM=128 (2 CTAs/SM) / BM=64 kernel. Net: Mistral-class
    large models get big on gate/up/down/q/o (kv-proj rows=1024 stays committed),
    Qwen-0.6B stays 100 % committed (0 regression). Mistral-7B Q4_K prefill
    (3-rep A/B, big-disabled vs gated-big): 4096 **2588 → 2827 (+9 %)**, 8192
    **2246 → 2343 (+4 %)**; 512 stays committed; decode (gemv, untouched)
    bit-unchanged ~146. The remaining ~4× gap to llama.cpp is still GEMM
    mma-issue efficiency at this 35%-of-ceiling wall, not X traffic.
  - **Read llama.cpp's MMQ source and replicated its scheme — the gap is Mojo
    codegen, NOT algorithm (2026-07-19).** Studied `scratch/bench/llama.cpp`
    `mmq{.cuh,-vec-dot.cuh,-load-tiles.cuh}`, `mma.cuh`, `mmq-config-ampere.cuh`
    (build `571d0d5`). Their Ada Q4_K/Q8_0 MMQ: `mma.sync.m16n8k32.s8.s8.s32`
    (== our `_mma_s8`), tile I=128 rows × J=128 tokens, **8 warps (256 thr)**,
    occupancy=1, **64 f32 acc/thread** (rows_per_warp=32, ntx=2, J/(ntx*8) j-tiles),
    smem row-major + pad (NO exotic repack on the NV mma path), per-32-block
    `sum += C*dA*dB` scaling, B via plain `load_generic` (comment: "faster than
    load_ldmatrix"), plus **stream-K** K-splitting with a fixup reduction. Design
    point is IDENTICAL to ours except (a) 8w×64acc vs our 16w×32acc, (b) stream-K.
    Replicated (a) directly — our kernel is already `[BM,BN,NW,FMT]`-parametrized so
    `[128,128,8]` IS their 8-warp/64-acc shape (`llt`): measured **~7% SLOWER**
    (T=2048 61 vs `_big` 66 TOPS) — fewer warps/higher ILP loses on this codegen.
    Also tried a full mma-burst (preload all B, MT×NT mma back-to-back, deferred
    f32-epilogue burst): **bit-identical TOPS to the interleaved loop** (65.98 vs
    65.97) — ptxas already overlaps the epilogue; SASS shows `.reuse` flags, the
    I2FP/FFMA epilogue interleaved into the IMMA stream, 127 regs, 0 spills. BN=256
    (only remaining X-traffic lever) **can't launch** — 64acc×512thr > 65536 regs
    (`LAUNCH_OUT_OF_RESOURCES`). Verified NOT DRAM-bound: correct traffic for
    14336/4096/2048 ≈ 1.03 GB → ~1.0 ms at 1 TB/s vs actual 3.65 ms → IMMA-issue
    bound. **Conclusion: same MMQ design nvcc/ptxas schedules to ~92% of the mma
    ceiling (169 TOPS), Mojo's backend reaches 36% (66 TOPS); no source-level
    restructuring (this round + all prior) moves it.** All experiments REVERTED
    (tree byte-identical to HEAD); committed `_big`-gated kernel retained. Honest
    numbers this machine: FORGE Mistral-7B Q4_K prefill 512 **1857**, 4096 **3032**,
    8192 **2473** tok/s (decode 146–175); Qwen Q8_0 4096 **19493** (decode 496);
    llama.cpp pp4096 **12018** → **~4.0× behind** at 4096. Genuinely untried levers
    both hit the same register wall or are outside our control: stream-K (global
    fixup+atomics, kernel+launcher rewrite) and a Mojo compiler mma/LDS dual-issue
    scheduling fix.
  - **Deep comptime K-unroll TESTED — Mojo unrolls, but it is NOT the lever
    (2026-07-19, `docs/CODEGEN_PROOF.md` Exp 5).** The codegen proof blamed the
    3.5× gap on Mojo emitting a ROLLED K-loop (8 IMMA/body) vs nvcc's deep-
    unrolled 256 IMMA/body. Tested directly: `gemm_i8mma_deep[...,KU,NBUF]` holds
    KU consecutive 32-col blocks per smem buffer and `comptime for`-unrolls the
    inner mma across all KU, so KU×8 IMMA emit straight-line. **SASS proves Mojo
    HONORS it** (`cuobjdump -sass` IMMA/body): KU=1→**8**, KU=2→**16**, KU=4→**32**,
    exactly linear; BRA does NOT grow (23→26→22), 0 spill at 104 regs. So the
    committed 8-IMMA body is a consequence of the 1-block-per-buffer smem tiling,
    NOT a backend refusal to unroll (refutes the proof's implied claim). **But
    TOPS barely moves:** Q4_K RTX 4090, big(8)/deep2(16)/deep4(32): down-proj
    N=4096 K=14336 T=2048 65.5/66.3/**68.0** (+3.8 %), T=512 62.4/64.2/**67.4**
    (+8 %); gate/up N=14336 K=4096 flat-to-−2 % (deep4 60.6 vs big 62.1). 4× the
    window = ≤+8 % (best) and negative on K-light shapes — not the 3.5× predicted.
    Still ~66 TOPS vs nvcc 208. **The deep window is not the bottleneck; the gap is
    a ptxas LDS/IMMA co-issue scheduling advantage Mojo does not match.** Ceiling:
    KU=8 at BM=BN=128 needs 80 KB smem, ptxas rejects (`0x14000 > 0xc000` — static
    `stack_allocation` is capped at 48 KB); nvcc reaches 256 IMMA/body via DYNAMIC
    smem. deep2/deep4 are bit-identical to committed (Q4_K + Q8_0, integer mma
    exact). All reverted; committed `_big` kept (no large win + deep4 single-buffers
    below 2 CTAs/SM, the documented 512-prefill occupancy tripwire). **Do not retry
    deep K-unroll for perf** — Mojo emits it faithfully, it just doesn't pay.

## FP8 prefill GEMM on GB10: a hand-written multistage kernel LOSES to Modular
- **Measured 2026-08-03, sm_121a. Do not retry the "more warps" lever.** The
  shipped prefill GEMM is Modular's `multistage_gemm_kernel` at block 128x256x64
  with a 64x64 warp tile: 8 warps, 224 regs, 98.3 KB smem, 1 CTA/SM = 16.67 %
  occupancy, and it plateaus at ~150 TFLOPS against a **251 TFLOPS** e4m3 mma
  ceiling (`bench_fp8_ceiling.mojo`, independent accumulators). The obvious
  hypothesis — shrink the warp tile to 32x64 so the accumulator drops 128 -> 64
  regs and 16 warps fit — was implemented in full (cp.async multistage, 3 stages,
  ldmatrix fragments, fused scale epilogue, LDY column slices) and **it is
  slower, not faster**:

  | shape | warp tile | warps | ours | Modular |
  |---|---|--:|--:|--:|
  | q/o (4096,4096) | 64x64 | 8 | 306.0 us | **258.0 us** |
  | q/o (4096,4096) | 32x64 | 16 | 327.5 us | **244.7 us** |
  | down (4096,11264) | 64x64 | 8 | 997.0 us | **865.6 us** |
  | down (4096,11264) | 32x64 | 16 | 1026.8 us | **878.6 us** |

  Output was **bit-identical** to Modular's on every case, so this is a pure
  perf result, not a correctness artifact.
- **Why more warps loses, quantitatively.** Per k-step the block issues
  `BM*BN/(16*WN)` A-ldmatrix and `BM*BN/(8*WM)` B-ldmatrix instructions, so A
  traffic scales as 1/WN and B traffic as 1/WM. Halving WM from 64 to 32 leaves
  A alone and **doubles** the shared-memory reads of B. The kernel is smem-
  bandwidth bound before it is occupancy bound, which is exactly why Modular
  rejects any warp tile other than 64x64. Registers were never the binding
  constraint. (The opposite result on the int8 MMQ path — 16 warps x 32 acc
  beating 8 warps x 64 — does NOT carry over: that kernel spends its smem
  budget on unpacking, not on ldmatrix.)
- **32x32 (32 warps, 1024 threads) does not launch at all** —
  `CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES`; 1024 threads x 64 acc regs is the whole
  65536-register file before operands.
- Even at the IDENTICAL 64x64 tiling our kernel trails by 15-19 %, i.e. the gap
  is implementation quality (smem swizzle vs our padding, paired B ldmatrix.x4,
  pipeline scheduling), the same ptxas-scheduling advantage `CODEGEN_PROOF.md`
  documents for int8. Closing it is a multi-day rewrite against a known wall.
  **Both files were reverted; the committed Modular path is retained.**

## FP8 (e4m3) — verified on sm_89
- `DType.float8_e4m3fn` works end-to-end: buffers, kernel pointers, and
  `Scalar[DType.float8_e4m3fn](f32)` casts (RN, satfinite ±448, denormals and
  -0.0 correct — matches a bit-level RNE oracle for every f16 input).
- BUT float8→float casts on the GPU lower to 64-bit bit-math EMULATION
  (~15 extra instructions per value) — never put them in a hot loop. Use the
  hardware pair conversion via inline PTX instead (src/kv_fp8.mojo):
  `from std.sys import inlined_assembly` →
  `inlined_assembly["cvt.rn.f16x2.e4m3x2 $0, $1;", UInt32, constraints="=r,h", has_side_effect=False](u16_pair)`
  (low byte → low f16 half; e4m3→f16 is exact). `has_side_effect=False` lets
  LLVM schedule loads across the asm.
- Bit reinterpretation: `x.to_bits[DType.uint8]()` and
  `from std.memory import bitcast` → `bitcast[DType.float16, 2](u32)`
  (there is no `SIMD.from_bits` / member `bitcast` on scalars).
- `comptime if cond:` works inside `def` for parameter-dependent codegen
  (e.g. branching a kernel body on a `kv_dtype: DType` parameter).
- Perf regime note: single-sequence decode attention is LATENCY-bound (heads
  × splits blocks ≈ 1-2 per SM, never DRAM-saturated), so halving KV bytes
  does not pay there — the pack+cvt chain costs more than the bandwidth
  saves. Graph-replay profile (nsys, Bielik 32q/8kv hd128, ctx 2048):
  attn_decode_split 23.0 us f16 vs 33.3 us fp8 per layer. Beware standalone
  micro-benches at one layer's shape: the KV slab fits in Ada's 72 MB L2
  after the first rep and can show the opposite sign. fp8's decode win needs
  a memory-bound attention (large batch / much longer ctx); today its value
  is 2x KV capacity at parity prefill.

## Files / OS
- `import std.os as os` → `os.makedirs(String(path), exist_ok=True)`, `os.remove(...)`.
- `from std.pathlib import Path` → `p.read_text()`, `p.write_text(s)`, `/` join.

## Natywne NVFP4 `mma` na sm_121a — uklad operandow skal

Sprzet liczy NVFP4 wprost, bez rekwantyzacji do MXFP4:

```
mma.sync.aligned.kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue4m3
  {d0..d3}, {a0..a3}, {b0,b1}, {c0..c3}, {sa}, {bid_a, tid_a}, {sb}, {bid_b, tid_b};
```

Typ skali nazywa sie **`ue4m3`**, nie `e4m3` — ta druga nazwa jest odrzucana
przez `ptxas` i wlasnie ona kazala nam wczesniej sadzic, ze natywne NVFP4 jest
niedostepne. `kind::mxf4nvf4` oraz jawne `scale_vec::4X` sa obowiazkowe.

Kodowanie: e2m1 1.0 = `0x2` (bajt dwoch jedynek = `0x22`), ue4m3 1.0 = `0x38`,
ue4m3 2.0 = `0x40`, ue8m0 1.0 = `0x7F`.

Operand skali to rejestr `.b32` W KAZDYM watku, ale licza sie tylko niektore.
Zmapowane empirycznie przez `probe_nvfp4_mma_layout.mojo` (A i B same jedynki,
jedna skala podniesiona do 2.0, obserwacja ktore wyjscie drgnelo):

| co | gdzie |
|---|---|
| skala wiersza `r` macierzy A (r = 0..7) | pas `4r`, przy `tid_a` parzystym |
| skala wiersza `r+8` macierzy A | pas `4r+1` |
| skala kolumny `n` macierzy B (n = 0..7) | pas `4n` |
| blok K `j` (k = 16j..16j+15) | bajt `j` rejestru, po obu stronach |

`tid` NIEPARZYSTY przesuwa pare pasow w kwadzie: licza wtedy `4r+2` i `4r+3`
(odwzorowane tak samo). `bid` przy `scale_vec::4X` **nie ma znaczenia** — caly
rejestr jest zuzywany; sprawdzone dla wszystkich czterech wartosci.

Kontrola poprawnosci: A i B same jedynki, wszystkie skale 1.0, `m16n8k64` daje
64.0 w kazdym elemencie wyjscia (4 bloki po 16). Podniesienie jednej skali A do
2.0 daje 80.0 w calym wierszu — po 16 na blok.

### Uklad fragmentow danych

Watek `t` ma grupe `g = t/4` i pozycje `q = t%4`; kazdy rejestr `.b32` niesie
osiem wartosci czterobitowych, mlodsza polbajtowka to mniejsze `k`:

| rejestr | co niesie |
|---|---|
| `a0` | wiersz `g`, `k = 8q .. 8q+7` |
| `a1` | wiersz `g+8`, `k = 8q .. 8q+7` |
| `a2` | wiersz `g`, `k = 32 + 8q .. 32+8q+7` |
| `a3` | wiersz `g+8`, `k = 32 + 8q .. 32+8q+7` |
| `b0` | kolumna `g`, `k = 8q .. 8q+7` |
| `b1` | kolumna `g`, `k = 32 + 8q .. 32+8q+7` |

Akumulator jak w kazdym `m16n8`: element `i` watku `t` to
`wiersz = t/4 + 8*(i/2)`, `kolumna = 2*(t%4) + i%2`.

`probe_nvfp4_mma_golden.mojo` liczy pelny kafel `16x8x64` z losowymi wartosciami
i skalami, i porownuje z referencja CPU **dokladnie** — wartosci sa dobrane tak,
zeby kazdy iloczyn i kazda suma byly w f32 scisle reprezentowalne, wiec test
wykrywa zly uklad, a nie zaokraglenie. Przechodzi na wszystkich 128 elementach.

To komplet potrzebny do napisania GEMM-u: wartosci, skale i akumulator.

### Warstwa kafelkowania nad tym rdzeniem (GB10, sm_121a, 2026-08-04)

`bench_nvfp4_native_gemm.mojo` trzyma dwa jadra liczace BITOWO to samo: naiwne
(jeden warp na kafel 16x64, prosto z pamieci globalnej) i kafelkowe. Kolejnosc
sumowania po K jest w obu identyczna, wiec kazdy krok optymalizacji sprawdza sie
testem na ROWNOSC CO DO BITU wzgledem CPU, nie na tolerancje.

Sufity tej maszyny (`bench_fp8_ceiling.mojo`): **497 TFLOPS** dla samej
instrukcji `mxf4nvf4/ue4m3` (mierzone osobno od `mxf4/ue8m0` — wychodzi tyle
samo) i **222 GB/s** odczytu strumieniowego. 48 SM, 24 MiB L2, LPDDR5X 256 bit.

Co dalo wynik, w kolejnosci wagi:

| krok | q/o | gate/up | down |
|---|--:|--:|--:|
| naiwny | 1104 us | 3413 us | 3233 us |
| + kafel BM x BN, `cp.async`, smem | 251 | 1699 | 583 |
| + przenumerowanie siatki pod L2 | 236 | 635 | 497 |
| + wyjscie f16 zamiast f32 | 213 | 558 | 505 |
| + `ldmatrix` na fragmenty | **197** | **500** | **484** |

- **Przenumerowanie siatki to najwiekszy pojedynczy skok** (gate/up 2,7x).
  Siatka jest 1-D i liczona grupami po GM kafli M, wiec bloki lecace obok siebie
  maja wspolne `n0` i dziela plat B w L2. Przy naturalnej kolejnosci (n
  najszybsze) fala bloków przemiata cale B dla jednego m i czyta je z DRAM-u od
  nowa dla kazdego kolejnego.
- **Dtype wyjscia to nie kosmetyka.** Przy M=1024 wagi NVFP4 to 0,5625 B na
  element, a wyjscie f32 az 4 B: dla gate/up wyjscie (46 MB) wazylo WIECEJ niz
  wagi (26 MB). Stad `OUT` jest parametrem jadra — sprawdzenie idzie na f32
  (mocniejsze porownanie), a mierzona sciezka pisze f16.
- **`LDA = 8*KC + 4`** u32 rozklada 32 pasy fragmentu na 32 rozne banki; bez tego
  odczyt A zderza sie wielodroznie. Ten sam odstep jest bezkonfliktowy dla
  `ldmatrix` (osiem wierszy po 16 B na plytke).
- **Fragment `mma` k64 to dokladnie to, co oddaje `ldmatrix`**: `ld_matrix[8]`
  (x4) na ukladzie wierszowym daje `a0..a3` bez zadnego przepakowania, a
  `ld_matrix[4]` (x2) daje `b0,b1`. Adresy podaje sie w innym rozbiciu pasa
  (osiem wierszy na plytke) niz to, w ktorym instrukcja oddaje dane. Warte 2-4%.
- Statyczne `stack_allocation` ma sufit 48 KiB, wiec potok glebszy niz 2 etapy
  wymaga `external_memory` + `shared_mem_bytes` (opt-in na tej maszynie: 99 KiB).

Czego NIE robic (zmierzone, bez zysku):

- **Potok glebszy niz 3 etapy szkodzi.** 4 etapy przy 128x256x64 to 78 KiB smem,
  1 CTA/SM i **-35%** (q/o 197 -> 268 us). 2 i 3 etapy sa w granicach szumu.
- **Parowanie B w `ldmatrix.x4`** (jeden `.x4` na dwa podkafle N zamiast dwoch
  `.x2`) — polowa ladowan B, zmiana **w granicach szumu** (197,0 vs 196,7 us;
  ksztalt L2-rezydentny 150,5 vs 151,1). Wycofane: zlozonosc bez zysku.
  To ten sam wniosek, co w sekcji int8/FP8 wyzej — liczba instrukcji w zrodle
  nie jest tu dzwignia.
- **Nie ma jednego najlepszego kafla.** q/o i gate/up wola 128x128x128 na 4
  warpach, down wola 128x256x64 na 8 warpach. Roznica ~8%.

### Podwojne buforowanie fragmentow w rejestrach i JEDNA bariera na etap

**Wczesniejszy wniosek w tym pliku — ze reszta luki to sciana codegenu Mojo — byl
BLEDNY i zostal wycofany.** Powstal z porownania do wlasnego zlego kodu. Lektura
`src/modular_i8/multistage_i8.mojo` (wendorowana kopia jadra Modulara) pokazala
trzy konkretne bledy w naszej petli:

1. **Opoznienie `ldmatrix` stalo odsloniete.** Nasza petla to bylo
   `bariera -> ldmatrix -> mma -> bariera`, czyli 12 ladowan i dopiero potem 32
   `mma`, z pelna zaleznoscia miedzy nimi. Przy 1 CTA na SM nie ma innych warpow,
   ktore by to zaslonily. Modular trzyma fragmenty w DWOCH kompletach rejestrow
   (`a_reg_tiles[next]`, `num_reg_tiles = 2 * k_group_size`) i wystawia
   `ldmatrix` kroku k+1 PRZED `mma` kroku k.
2. **Dwie bariery na etap zamiast jednej.** Wystawialismy `cp.async` PRZED bariera
   etapu, wiec zapis scigal sie z odczytami poprzedniego etapu i trzeba bylo
   bariery zamykajacej. Modular wystawia prefetch W SRODKU petli po k, juz za
   bariera — wtedy bufor `(s-1) % NS` jest z definicji wolny i wystarczy JEDNA.
3. Przy okazji: `wait_group` liczy sie inaczej w tej strukturze — `NS-2`, nie
   `NS-1`, i przy `NS=2` nie ma juz zadnego nakladania, wiec **NS=3 jest teraz
   ustawieniem domyslnym**, a nie NS=2.

PULAPKA, ktora to wprowadzilo (zlapana testem na rownosc co do bitu): skale nie
moga byc czytane z pamieci wspoldzielonej dopiero przy `mma`. Bariera etapu
ogradza tylko to, co przed nia, a `cp.async` nadpisujacy bufor `(s-1) % NS`
rusza zaraz po niej — odczyt skal za bariera scigal sie z tym zapisem
(512 blednych elementow na 65536). Skale czyta sie na POCZATKU kroku K, przed
awansem potoku. Trzymanie ich w drugim komplecie rejestrow tez dziala, ale
kosztuje `2*(MT+NT)` rejestrow i wpycha jadro w spille przy 255.

Wynik (128x256x64, 8 warpow, potok 3 etapy, wyjscie f16):

| | q/o | gate/up | down | 2048^3 w L2 |
|---|--:|--:|--:|--:|
| przed | 196,6 us | 500,7 us | 484,2 us | 118,8 us |
| po | **183,6** | **506,7** | **462,9** | **108,6** |

`ptxas`: 230-254 rejestrow, zero spilli. Ksztalt L2-rezydentny poprawil sie o 9%,
czyli poprawa jest w samej petli, nie w ruchu pamieci.

### Co zostalo niezrobione: swizzle zamiast wypelnienia

`LDA = 8*KC + SMEM_PAD` jest bezkonfliktowy dla ODCZYTU `ldmatrix` (osiem
wierszy po 16 B trafia w 32 rozne banki), ale **nie dla ZAPISU `cp.async`**:
adres `(tid/2)*LDA + (tid%2)*4` przy LDA=12 daje kilkudrozne kolizje. Wiekszego
wypelnienia nie da sie tu uzyc — LDA=20 przekracza limit pamieci wspoldzielonej
przy NS=4. Wlasciwe rozwiazanie to XOR-swizzle (`layout.swizzle`,
`make_ldmatrix_swizzle`), ktory naprawia obie strony I ZMNIEJSZA zajetosc
(LDA=8*KC bez wypelnienia). Warunek: wiersz musi miec 128 B, czyli **KC=4**
(przy KC=1 wiersz ma 32 B, osiem wierszy to 256 B i dwudrozna kolizja zostaje
niezaleznie od permutacji). Przy KC=4, `chunk ^ (r % 8)` rozklada osiem wierszy
na wszystkie 32 banki.

Stan na dzis: 187-204 TFLOPS przy suficie 497, czyli 38-41%. Wlasne jadro FP8
Modulara na tej samej maszynie osiaga 60% swojego sufitu, wiec zapas nadal jest
i NIE jest to ograniczenie jezyka.
