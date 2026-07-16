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

## Files / OS
- `import std.os as os` → `os.makedirs(String(path), exist_ok=True)`, `os.remove(...)`.
- `from std.pathlib import Path` → `p.read_text()`, `p.write_text(s)`, `/` join.
