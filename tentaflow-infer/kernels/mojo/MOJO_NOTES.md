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
