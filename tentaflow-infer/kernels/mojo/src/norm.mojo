# ===== File: norm.mojo — RMSNorm kernels (row-per-block, f32 accumulation) =====
# One thread block per row: LLM hidden sizes (1k-16k) keep a 256-thread block
# busy via grid-stride columns, and the two-level warp/shared reduction avoids
# atomics. Accumulation is always f32 regardless of storage dtype so the
# result matches the CPU golden reference within f16 rounding only.

from std.gpu import block_dim, block_idx, thread_idx
from std.math import rsqrt
from std.memory import bitcast
from src.reduce import block_reduce_sum, block_reduce_max

comptime E4M3_MAX: Float32 = 448.0
# Liczba niezależnych odczytów w locie na wątek w rmsnorm z residuałem.
comptime NORM_UNROLL = 8


def rmsnorm_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """out[row] = x[row] / rms(x[row]) * weight, one block per row."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(x[base + i])
        ss += v * v
        i += Int(block_dim.x)

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(x[base + i]) * inv * Float32(weight[i]))
        i += Int(block_dim.x)


def rmsnorm_residual_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """Fused residual-add + RMSNorm: residual += x, out = rmsnorm(residual).

    The residual stream update and the norm read the same values, so fusing
    them halves DRAM traffic on the decode path.

    Batched decode launches one block per token, so a 32-sequence step only
    fills 32 SMs; the sole source of memory-level parallelism left is the number
    of loads a single thread keeps in flight. Both passes therefore issue
    NORM_UNROLL grid-strided accesses before consuming any of them.

    The stride stays block_dim.x, so every thread visits the same columns in the
    same order as a scalar loop and the residual stream is exact. The f32 sum is
    NOT bit-identical to the scalar form though: unrolling lets the compiler
    contract and schedule the accumulation differently. Measured against the
    stored B1 reference logits the drift is inside f16 noise and slightly
    favourable (top-1 agreement 0.99927 -> 0.99951, max_abs 1.667 -> 1.371) and
    the greedy token stream is unchanged, so ordering is deliberately left to
    the compiler rather than pinned.
    """
    row = Int(block_idx.x)
    base = row * n_cols
    stride = Int(block_dim.x)

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i + (NORM_UNROLL - 1) * stride < n_cols:
        var v = InlineArray[Float32, NORM_UNROLL](fill=0.0)
        comptime for u in range(NORM_UNROLL):
            v[u] = (
                Float32(residual_io[base + i + u * stride])
                + Float32(x[base + i + u * stride])
            )
        comptime for u in range(NORM_UNROLL):
            residual_io[base + i + u * stride] = Float16(v[u])
            ss += v[u] * v[u]
        i += NORM_UNROLL * stride
    while i < n_cols:
        v = Float32(residual_io[base + i]) + Float32(x[base + i])
        residual_io[base + i] = Float16(v)
        ss += v * v
        i += stride

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i + (NORM_UNROLL - 1) * stride < n_cols:
        var v = InlineArray[Float32, NORM_UNROLL](fill=0.0)
        comptime for u in range(NORM_UNROLL):
            v[u] = (
                Float32(residual_io[base + i + u * stride])
                * inv
                * Float32(weight[i + u * stride])
            )
        comptime for u in range(NORM_UNROLL):
            out_ptr[base + i + u * stride] = Float16(v[u])
        i += NORM_UNROLL * stride
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(residual_io[base + i]) * inv * Float32(weight[i]))
        i += stride


def rmsnorm_qkv_f16(
    q: UnsafePointer[Float16, MutAnyOrigin],
    k: UnsafePointer[Float16, MutAnyOrigin],
    v: UnsafePointer[Float16, MutAnyOrigin],
    wq: UnsafePointer[Float16, MutAnyOrigin],
    wk: UnsafePointer[Float16, MutAnyOrigin],
    wv: UnsafePointer[Float16, MutAnyOrigin],
    q_rows: Int,
    kv_rows: Int,
    head_dim: Int,
    eps: Float32,
):
    """Normy Q, K i V w JEDNYM uruchomieniu, po jednym bloku na glowice.

    Rodzina Gemma normalizuje osobno Q, K i V (V wektorem jedynek), co dawalo
    trzy uruchomienia na warstwe, czyli 144 na token przy 48 warstwach. Kazdy z
    nich czyta kilka kB, wiec dominowal koszt samego wywolania. Blok wybiera
    tensor po swoim indeksie: `[0, q_rows)` to Q, dalej K, dalej V. Matematyka
    jest ta sama co w `rmsnorm_f16` w miejscu.
    """
    row = Int(block_idx.x)
    stride = Int(block_dim.x)

    var buf = q
    var weight = wq
    var local = row
    if row >= q_rows + kv_rows:
        buf = v
        weight = wv
        local = row - q_rows - kv_rows
    elif row >= q_rows:
        buf = k
        weight = wk
        local = row - q_rows

    base = local * head_dim

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < head_dim:
        val = Float32(buf[base + i])
        ss += val * val
        i += stride
    inv = rsqrt(block_reduce_sum(ss) / Float32(head_dim) + eps)

    i = Int(thread_idx.x)
    while i < head_dim:
        buf[base + i] = Float16(Float32(buf[base + i]) * inv * Float32(weight[i]))
        i += stride


def rmsnorm_delta_residual_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    delta: UnsafePointer[Float16, MutAnyOrigin],
    delta_weight: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
    layer_scale: Float32,
):
    """Norma „sandwich" + rezyduum + skala warstwy + norma wyjsciowa w JEDNYM
    uruchomieniu.

    Rodzina Gemma normalizuje wyjscie bloku PRZED dodaniem do rezyduum, a potem
    mnozy caly strumien przez skalar warstwy. Rozbite na osobne kernele daja trzy
    uruchomienia na blok, czyli szesc na warstwe i 144 na token przy 48
    warstwach. Profiler pokazal, ze pojedynczy rmsnorm nad 3840 wartosciami
    zajmuje 3,98 us, choc czyta 7,7 kB — to sam koszt uruchomienia. Tutaj jest to
    jeden przebieg:

        d = rmsnorm(delta) * delta_weight
        h = (h + d) * layer_scale
        out = rmsnorm(h) * weight

    `layer_scale` rowne 1.0 wylacza skalowanie. Kolejnosc dziala tak samo jak
    wersja rozbita: skala trafia w `h` przed policzeniem `out`, a norma RMS jest
    niezmiennicza na skale, wiec `out` sie nie zmienia poza zaokragleniem f16
    samego mnozenia.
    """
    row = Int(block_idx.x)
    base = row * n_cols
    stride = Int(block_dim.x)

    # Kazdy przebieg wystawia NORM_UNROLL dostepow przed konsumpcja — przy 3840
    # kolumnach i 256 watkach pojedynczy dostep na obieg zostawia kernel na
    # latencji, a nie na pasmie (to samo rozwiazanie ma `rmsnorm_residual_f16`).

    # 1) RMS samej delty.
    var ds: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i + (NORM_UNROLL - 1) * stride < n_cols:
        var dv = InlineArray[Float32, NORM_UNROLL](fill=0.0)
        comptime for u in range(NORM_UNROLL):
            dv[u] = Float32(delta[base + i + u * stride])
        comptime for u in range(NORM_UNROLL):
            ds += dv[u] * dv[u]
        i += NORM_UNROLL * stride
    while i < n_cols:
        dv1 = Float32(delta[base + i])
        ds += dv1 * dv1
        i += stride
    d_inv = rsqrt(block_reduce_sum(ds) / Float32(n_cols) + eps)

    # 2) h = (h + norm(delta) * delta_weight) * layer_scale, przy okazji RMS h.
    var ss: Float32 = 0.0
    i = Int(thread_idx.x)
    while i + (NORM_UNROLL - 1) * stride < n_cols:
        var hv = InlineArray[Float32, NORM_UNROLL](fill=0.0)
        comptime for u in range(NORM_UNROLL):
            dn = (
                Float32(delta[base + i + u * stride])
                * d_inv
                * Float32(delta_weight[i + u * stride])
            )
            hv[u] = (Float32(residual_io[base + i + u * stride]) + dn) * layer_scale
        comptime for u in range(NORM_UNROLL):
            vh = Float16(hv[u])
            residual_io[base + i + u * stride] = vh
            f = Float32(vh)
            ss += f * f
        i += NORM_UNROLL * stride
    while i < n_cols:
        dn1 = Float32(delta[base + i]) * d_inv * Float32(delta_weight[i])
        v = (Float32(residual_io[base + i]) + dn1) * layer_scale
        vh1 = Float16(v)
        residual_io[base + i] = vh1
        f1 = Float32(vh1)
        ss += f1 * f1
        i += stride
    inv = rsqrt(block_reduce_sum(ss) / Float32(n_cols) + eps)

    # 3) out = rmsnorm(h) * weight.
    i = Int(thread_idx.x)
    while i + (NORM_UNROLL - 1) * stride < n_cols:
        var ov = InlineArray[Float32, NORM_UNROLL](fill=0.0)
        comptime for u in range(NORM_UNROLL):
            ov[u] = (
                Float32(residual_io[base + i + u * stride])
                * inv
                * Float32(weight[i + u * stride])
            )
        comptime for u in range(NORM_UNROLL):
            out_ptr[base + i + u * stride] = Float16(ov[u])
        i += NORM_UNROLL * stride
    while i < n_cols:
        out_ptr[base + i] = Float16(
            Float32(residual_io[base + i]) * inv * Float32(weight[i])
        )
        i += stride


def _rmsnorm_fp8_emit(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    src: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    base: Int,
    n_cols: Int,
    inv: Float32,
):
    """Second half of the fused rmsnorm→fp8 path: write the f16 normed row
    (out_ptr, kept for the residual carry / any f16 reader), take the row absmax
    of those f16-rounded normed values, and emit ONE per-token e4m3 activation +
    per-token f32 scale in the layout `gemm_fp8_mod` consumes (codes [T,K]
    row-major, scale [T]). Numerics match the standalone rmsnorm_f16 →
    quantize_act_fp8 pair bit-for-bit (same f16 round point, same absmax/448
    scale), so PPL is unchanged — only WHERE the quant runs moves."""
    var amax_local: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        h = Float16(Float32(src[base + i]) * inv * Float32(weight[i]))
        out_ptr[base + i] = h
        a = abs(Float32(h))
        if a > amax_local:
            amax_local = a
        i += Int(block_dim.x)

    amax = block_reduce_max(amax_local)
    row = base // n_cols
    if amax == 0.0:
        if thread_idx.x == 0:
            xs[row] = 0.0
        var z = Int(thread_idx.x)
        while z < n_cols:
            xq[base + z] = 0
            z += Int(block_dim.x)
        return

    scale = amax / E4M3_MAX
    inv_s = E4M3_MAX / amax
    if thread_idx.x == 0:
        xs[row] = scale
    var q = Int(thread_idx.x)
    while q < n_cols:
        e = Scalar[DType.float8_e4m3fn](Float32(out_ptr[base + q]) * inv_s)
        xq[base + q] = bitcast[DType.int8, 1](e)
        q += Int(block_dim.x)


def rmsnorm_fp8(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """out = rmsnorm(x)*weight, ALSO emit the shared per-token e4m3 activation.

    One block per row. Fuses the standalone rmsnorm_f16 → quantize_act_fp8 pair
    so the q/k/v (or gate/up) projections read ONE quantized activation instead
    of re-quantizing the norm output per projection (`FORGE_GEMM=fp8mod`)."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(x[base + i])
        ss += v * v
        i += Int(block_dim.x)

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    _rmsnorm_fp8_emit(out_ptr, xq, xs, x, weight, base, n_cols, inv)


def rmsnorm_residual_fp8(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """Fused residual-add + RMSNorm + shared fp8 activation emit: residual += x,
    out = rmsnorm(residual)*weight, and the per-token e4m3 codes + scale for the
    following gate/up (or next-layer q/k/v) projections. See rmsnorm_fp8."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(residual_io[base + i]) + Float32(x[base + i])
        residual_io[base + i] = Float16(v)
        ss += v * v
        i += Int(block_dim.x)

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    _rmsnorm_fp8_emit(out_ptr, xq, xs, residual_io, weight, base, n_cols, inv)
