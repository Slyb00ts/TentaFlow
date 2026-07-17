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

## Files / OS
- `import std.os as os` → `os.makedirs(String(path), exist_ok=True)`, `os.remove(...)`.
- `from std.pathlib import Path` → `p.read_text()`, `p.write_text(s)`, `/` join.
