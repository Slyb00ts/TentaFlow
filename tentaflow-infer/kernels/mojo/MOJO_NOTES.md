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
