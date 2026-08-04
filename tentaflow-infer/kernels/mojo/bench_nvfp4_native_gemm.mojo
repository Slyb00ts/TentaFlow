# =============================================================================
# Plik: bench_nvfp4_native_gemm.mojo
# Opis: Rdzen GEMM-u na natywnym NVFP4 `mma` — poprawnosc przeciw CPU i pomiar.
# Przyklad: pixi run mojo bench_nvfp4_native_gemm.mojo
# =============================================================================
#
# Pierwsza wersja liczaca PRAWDZIWY GEMM na
# `kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64` — uklad operandow jest
# zweryfikowany w `probe_nvfp4_mma_layout.mojo` i `probe_nvfp4_mma_golden.mojo`.
#
# Swiadomie NIE MA tu jeszcze pamieci wspoldzielonej ani `cp.async`: jeden warp
# liczy kafel 16x8 i czyta fragmenty wprost z pamieci globalnej. Chodzi o to,
# zeby najpierw miec rdzen, ktory zgadza sie z referencja co do bitu, i dopiero
# na nim budowac kafelkowanie. Pomiar ponizej jest wiec PODLOGA, nie sufitem.
#
# Uklad danych wejsciowych jest taki, jaki ma model: A i B trzymaja kody e2m1
# po dwa na bajt (mlodsza polbajtowka to mniejsze k), a skale ue4m3 po jednej na
# 16 wartosci K. Dzieki temu kazdy fragment jest JEDNYM odczytem 4-bajtowym:
#
#   a0 -> A[wiersz][8q .. 8q+7]        = u32 pod indeksem row*(K/8) + 8*kb + q
#   a2 -> A[wiersz][32+8q .. ]         = ten sam indeks + 4
#   skale wiersza dla calego kroku K64 = u32 pod indeksem row*(K/64) + kb

from std.gpu import block_idx, thread_idx
from std.gpu.host import DeviceBuffer, DeviceContext, HostBuffer
from std.gpu.intrinsics import inlined_assembly
from std.sys import _RegisterPackType
from std.time import perf_counter_ns

comptime LANES = 32
comptime ROUNDS = 5
comptime ITERS = 10


def mma_nvfp4(
    a0: UInt32, a1: UInt32, a2: UInt32, a3: UInt32,
    b0: UInt32, b1: UInt32,
    c: SIMD[DType.float32, 4],
    sa: UInt32, sb: UInt32,
) -> _RegisterPackType[Float32, Float32, Float32, Float32]:
    return inlined_assembly[
        (
            "mma.sync.aligned.kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue4m3"
            " {$0, $1, $2, $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12,"
            " $13}, {$14}, {$16, $17}, {$15}, {$16, $17};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r,h,h",
        has_side_effect=False,
    ](
        a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb,
        UInt16(0), UInt16(0),
    )


def gemm_nvfp4[
    N: Int, K: Int, NT: Int = 8
](
    y: UnsafePointer[Float32, MutAnyOrigin],
    a: UnsafePointer[UInt32, MutAnyOrigin],
    sa: UnsafePointer[UInt32, MutAnyOrigin],
    b: UnsafePointer[UInt32, MutAnyOrigin],
    sb: UnsafePointer[UInt32, MutAnyOrigin],
):
    """Y[M,N] = A[M,K] * B[N,K]^T; jeden warp liczy kafel 16 x (8*NT).

    Fragmenty A czyta si e RAZ na krok K64 i przepuszcza przez wszystkie NT
    podkafli kolumnowych — to jedyne ponowne uzycie, jakie da sie miec bez
    pamieci wspoldzielonej, i kosztuje 4*NT rejestrow akumulatora.
    """
    comptime A_ROW = K // 8      # u32 na wiersz A
    comptime S_ROW = K // 64     # u32 skal na wiersz
    comptime CHUNKS = K // 64

    lane = Int(thread_idx.x)
    g = lane // 4
    q = lane % 4
    tile_n = Int(block_idx.x) * NT
    tile_m = Int(block_idx.y)

    # Pas 4r niesie skale wiersza r, pas 4r+1 wiersza r+8; pas 4n skale kolumny n.
    # Pasy, ktore skal nie niosa, i tak musza podac rejestr — dajemy im poprawny
    # adres, bo instrukcja go zignoruje.
    scale_row = tile_m * 16 + g + (8 if (q == 1) else 0)

    var acc = InlineArray[SIMD[DType.float32, 4], NT](
        fill=SIMD[DType.float32, 4](0.0)
    )
    var a_lo = a + (tile_m * 16 + g) * A_ROW + q
    var a_hi = a + (tile_m * 16 + g + 8) * A_ROW + q
    var sa_base = sa + scale_row * S_ROW

    for kb in range(CHUNKS):
        var a0 = a_lo[kb * 8]
        var a1 = a_hi[kb * 8]
        var a2 = a_lo[kb * 8 + 4]
        var a3 = a_hi[kb * 8 + 4]
        var sa_reg = sa_base[kb]
        comptime for t in range(NT):
            var col = (tile_n + t) * 8 + g
            var b_base = b + col * A_ROW + q
            var r = mma_nvfp4(
                a0, a1, a2, a3,
                b_base[kb * 8], b_base[kb * 8 + 4],
                acc[t],
                sa_reg, sb[col * S_ROW + kb],
            )
            acc[t] = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])

    comptime for t in range(NT):
        comptime for i in range(4):
            var row = tile_m * 16 + g + 8 * (i // 2)
            var col = (tile_n + t) * 8 + 2 * q + (i % 2)
            y[row * N + col] = acc[t][i]


def _e2m1(code: Int) -> Float32:
    var m = code & 7
    var mag = Float32(6.0)  # kod 7; ponizsze galezie pokrywaja 0..6
    if m == 0:
        mag = 0.0
    elif m == 1:
        mag = 0.5
    elif m == 2:
        mag = 1.0
    elif m == 3:
        mag = 1.5
    elif m == 4:
        mag = 2.0
    elif m == 5:
        mag = 3.0
    elif m == 6:
        mag = 4.0
    if (code & 8) != 0:
        return -mag
    return mag


def _ue4m3(code: Int) -> Float32:
    var e = (code >> 3) & 15
    var value = Float32(1.0)
    if e >= 7:
        for _ in range(e - 7):
            value *= 2.0
    else:
        for _ in range(7 - e):
            value *= 0.5
    return value


def _check[M: Int, N: Int, K: Int](ctx: DeviceContext) raises:
    """Maly ksztalt liczony rownolegle na GPU i szeregowo na CPU."""
    comptime A_U32 = M * K // 8
    comptime B_U32 = N * K // 8
    comptime SA_U32 = M * K // 64
    comptime SB_U32 = N * K // 64

    var a_code = List[Int](capacity=M * K)
    var b_code = List[Int](capacity=N * K)
    var sa_code = List[Int](capacity=M * K // 16)
    var sb_code = List[Int](capacity=N * K // 16)
    var seed = 987654321
    for _ in range(M * K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        a_code.append((seed >> 7) & 15)
    for _ in range(N * K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        b_code.append((seed >> 7) & 15)
    for _ in range(M * K // 16):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        sa_code.append((6 + ((seed >> 7) % 3)) << 3)
    for _ in range(N * K // 16):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        sb_code.append((6 + ((seed >> 7) % 3)) << 3)

    var a = ctx.enqueue_create_buffer[DType.uint32](A_U32)
    var b = ctx.enqueue_create_buffer[DType.uint32](B_U32)
    var sa = ctx.enqueue_create_buffer[DType.uint32](SA_U32)
    var sb = ctx.enqueue_create_buffer[DType.uint32](SB_U32)
    var y = ctx.enqueue_create_buffer[DType.float32](M * N)
    var host = ctx.enqueue_create_host_buffer[DType.float32](M * N)

    with a.map_to_host() as v:
        for i in range(A_U32):
            var packed = UInt32(0)
            for j in range(8):
                packed |= UInt32(a_code[i * 8 + j]) << UInt32(4 * j)
            v[i] = packed
    with b.map_to_host() as v:
        for i in range(B_U32):
            var packed = UInt32(0)
            for j in range(8):
                packed |= UInt32(b_code[i * 8 + j]) << UInt32(4 * j)
            v[i] = packed
    with sa.map_to_host() as v:
        for i in range(SA_U32):
            var packed = UInt32(0)
            for j in range(4):
                packed |= UInt32(sa_code[i * 4 + j]) << UInt32(8 * j)
            v[i] = packed
    with sb.map_to_host() as v:
        for i in range(SB_U32):
            var packed = UInt32(0)
            for j in range(4):
                packed |= UInt32(sb_code[i * 4 + j]) << UInt32(8 * j)
            v[i] = packed

    ctx.enqueue_function[gemm_nvfp4[N, K]](
        y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
        b.unsafe_ptr(), sb.unsafe_ptr(),
        grid_dim=(N // 64, M // 16), block_dim=LANES,
    )
    ctx.enqueue_copy(host, y)
    ctx.synchronize()

    var bad = 0
    var first = String("")
    for m in range(M):
        for n in range(N):
            var total = Float32(0.0)
            for j in range(K // 16):
                var block_sum = Float32(0.0)
                for t in range(16):
                    var k = j * 16 + t
                    block_sum += _e2m1(a_code[m * K + k]) * _e2m1(b_code[n * K + k])
                total += block_sum * _ue4m3(sa_code[m * (K // 16) + j]) * _ue4m3(
                    sb_code[n * (K // 16) + j]
                )
            if host[m * N + n] != total:
                bad += 1
                if first == "":
                    first = (
                        "m" + String(m) + "n" + String(n) + " oczekiwane "
                        + String(total) + ", otrzymane " + String(host[m * N + n])
                    )
    if bad == 0:
        print(
            "poprawnosc M=" + String(M) + " N=" + String(N) + " K=" + String(K)
            + ": ZGADZA SIE co do bitu (" + String(M * N) + " elementow)"
        )
    else:
        print("poprawnosc: ROZNICE", bad, "z", M * N, "| pierwsza:", first)


def _time[M: Int, N: Int, K: Int](ctx: DeviceContext, name: String) raises:
    var a = ctx.enqueue_create_buffer[DType.uint32](M * K // 8)
    var b = ctx.enqueue_create_buffer[DType.uint32](N * K // 8)
    var sa = ctx.enqueue_create_buffer[DType.uint32](M * K // 64)
    var sb = ctx.enqueue_create_buffer[DType.uint32](N * K // 64)
    var y = ctx.enqueue_create_buffer[DType.float32](M * N)

    for _ in range(50):
        ctx.enqueue_function[gemm_nvfp4[N, K]](
            y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
            b.unsafe_ptr(), sb.unsafe_ptr(),
            grid_dim=(N // 64, M // 16), block_dim=LANES,
        )
    ctx.synchronize()

    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var t0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_nvfp4[N, K]](
                y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
                b.unsafe_ptr(), sb.unsafe_ptr(),
                grid_dim=(N // 64, M // 16), block_dim=LANES,
            )
        ctx.synchronize()
        var dt = Float64(perf_counter_ns() - t0) / Float64(ITERS)
        if dt < best:
            best = dt
    var flops = 2.0 * Float64(M) * Float64(N) * Float64(K)
    print(name, best / 1000.0, "us |", flops / best / 1000.0, "TFLOPS")


def main() raises:
    var ctx = DeviceContext()
    _check[32, 64, 128](ctx)
    _check[64, 128, 256](ctx)
    _check[128, 64, 512](ctx)
    print("")
    _time[1024, 4096, 4096](ctx, "q/o     (1024x4096x4096):")
    _time[1024, 11264, 4096](ctx, "gate/up (1024x11264x4096):")
    _time[1024, 4096, 11264](ctx, "down    (1024x4096x11264):")
