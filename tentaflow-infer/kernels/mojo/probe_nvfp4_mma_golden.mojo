# =============================================================================
# Plik: probe_nvfp4_mma_golden.mojo
# Opis: Zloty test natywnego NVFP4 `mma` — pelny kafel przeciw referencji CPU.
# Przyklad: pixi run mojo probe_nvfp4_mma_golden.mojo
# =============================================================================
#
# `probe_nvfp4_mma_layout.mojo` ustalil, gdzie siedza SKALE. Zostaje uklad samych
# wartosci: czterobitowe fragmenty `m16n8k64`. Hipoteza jest naturalnym
# rozszerzeniem ukladu osmiobitowego `m16n8k32` — watek `t` ma grupe `g = t/4`
# i pozycje `q = t%4`:
#
#   A (16 x 64): a0 -> wiersz g,   k = 8q + 0..7
#                a1 -> wiersz g+8, k = 8q + 0..7
#                a2 -> wiersz g,   k = 32 + 8q + 0..7
#                a3 -> wiersz g+8, k = 32 + 8q + 0..7
#   B (64 x 8):  b0 -> kolumna g,  k = 8q + 0..7
#                b1 -> kolumna g,  k = 32 + 8q + 0..7
#
# Test liczy losowy kafel na GPU i te sama arytmetyke na CPU, po czym porownuje
# DOKLADNIE. Wartosci sa tak dobrane, zeby kazdy iloczyn i kazda suma byly w f32
# scisle reprezentowalne (dwojkowe wykladniki, male zakresy), wiec rozbieznosc
# oznacza zly uklad, a nie zaokraglenie.

from std.gpu import block_idx, thread_idx
from std.gpu.host import DeviceBuffer, DeviceContext, HostBuffer
from std.gpu.intrinsics import inlined_assembly
from std.sys import _RegisterPackType

comptime M = 16
comptime N = 8
comptime K = 64
comptime LANES = 32
comptime BLOCKS_K = 4  # 64 / 16 wartosci na blok skali


def _e2m1(code: Int) -> Float32:
    """Dekoduje czterobitowy e2m1; bit 3 to znak."""
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
    """Dekoduje skale ue4m3; uzywamy wylacznie potegi dwojki (mantysa zero)."""
    var e = (code >> 3) & 15
    var value = Float32(1.0)
    if e >= 7:
        for _ in range(e - 7):
            value *= 2.0
    else:
        for _ in range(7 - e):
            value *= 0.5
    return value


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


def kern(
    a_regs: UnsafePointer[UInt32, MutAnyOrigin],
    b_regs: UnsafePointer[UInt32, MutAnyOrigin],
    sa: UnsafePointer[UInt32, MutAnyOrigin],
    sb: UnsafePointer[UInt32, MutAnyOrigin],
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
):
    lane = Int(thread_idx.x)
    var acc = SIMD[DType.float32, 4](0.0)
    var r = mma_nvfp4(
        a_regs[lane * 4 + 0], a_regs[lane * 4 + 1],
        a_regs[lane * 4 + 2], a_regs[lane * 4 + 3],
        b_regs[lane * 2 + 0], b_regs[lane * 2 + 1],
        acc, sa[lane], sb[lane],
    )
    comptime for i in range(4):
        out_ptr[lane * 4 + i] = r[i]


def main() raises:
    var ctx = DeviceContext()

    # --- dane zrodlowe (deterministyczne, bez losowosci sprzetowej) ---
    var a_code = InlineArray[Int, M * K](uninitialized=True)
    var b_code = InlineArray[Int, N * K](uninitialized=True)
    var sa_code = InlineArray[Int, M * BLOCKS_K](uninitialized=True)
    var sb_code = InlineArray[Int, N * BLOCKS_K](uninitialized=True)
    var seed = 12345
    for i in range(M * K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        a_code[i] = (seed >> 7) & 15
    for i in range(N * K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        b_code[i] = (seed >> 7) & 15
    for i in range(M * BLOCKS_K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        sa_code[i] = (5 + ((seed >> 7) % 5)) << 3
    for i in range(N * BLOCKS_K):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        sb_code[i] = (5 + ((seed >> 7) % 5)) << 3

    # --- referencja CPU ---
    var want = InlineArray[Float32, M * N](uninitialized=True)
    for m in range(M):
        for n in range(N):
            var total = Float32(0.0)
            for j in range(BLOCKS_K):
                var block_sum = Float32(0.0)
                for t in range(16):
                    var k = j * 16 + t
                    block_sum += _e2m1(a_code[m * K + k]) * _e2m1(
                        b_code[n * K + k]
                    )
                total += block_sum * _ue4m3(sa_code[m * BLOCKS_K + j]) * _ue4m3(
                    sb_code[n * BLOCKS_K + j]
                )
            want[m * N + n] = total

    # --- pakowanie wedlug hipotezy ---
    var a_regs = ctx.enqueue_create_buffer[DType.uint32](LANES * 4)
    var b_regs = ctx.enqueue_create_buffer[DType.uint32](LANES * 2)
    var sa_buf = ctx.enqueue_create_buffer[DType.uint32](LANES)
    var sb_buf = ctx.enqueue_create_buffer[DType.uint32](LANES)
    var out = ctx.enqueue_create_buffer[DType.float32](LANES * 4)
    var host = ctx.enqueue_create_host_buffer[DType.float32](LANES * 4)

    with a_regs.map_to_host() as v:
        for lane in range(LANES):
            var g = lane // 4
            var q = lane % 4
            comptime for reg in range(4):
                var row = g + 8 * (reg % 2)
                var k0 = 32 * (reg // 2) + 8 * q
                var packed = UInt32(0)
                for i in range(8):
                    packed |= UInt32(a_code[row * K + k0 + i]) << UInt32(4 * i)
                v[lane * 4 + reg] = packed
    with b_regs.map_to_host() as v:
        for lane in range(LANES):
            var col = lane // 4
            var q = lane % 4
            comptime for reg in range(2):
                var k0 = 32 * reg + 8 * q
                var packed = UInt32(0)
                for i in range(8):
                    packed |= UInt32(b_code[col * K + k0 + i]) << UInt32(4 * i)
                v[lane * 2 + reg] = packed
    # Skale: pas 4r niesie wiersz r, pas 4r+1 wiersz r+8, pas 4n kolumne n.
    with sa_buf.map_to_host() as v:
        for lane in range(LANES):
            v[lane] = 0
        for r in range(8):
            var lo = UInt32(0)
            var hi = UInt32(0)
            for j in range(BLOCKS_K):
                lo |= UInt32(sa_code[r * BLOCKS_K + j]) << UInt32(8 * j)
                hi |= UInt32(sa_code[(r + 8) * BLOCKS_K + j]) << UInt32(8 * j)
            v[4 * r] = lo
            v[4 * r + 1] = hi
    with sb_buf.map_to_host() as v:
        for lane in range(LANES):
            v[lane] = 0
        for n in range(N):
            var packed = UInt32(0)
            for j in range(BLOCKS_K):
                packed |= UInt32(sb_code[n * BLOCKS_K + j]) << UInt32(8 * j)
            v[4 * n] = packed

    ctx.enqueue_function[kern](
        a_regs.unsafe_ptr(), b_regs.unsafe_ptr(),
        sa_buf.unsafe_ptr(), sb_buf.unsafe_ptr(), out.unsafe_ptr(),
        grid_dim=1, block_dim=LANES,
    )
    ctx.enqueue_copy(host, out)
    ctx.synchronize()

    # --- porownanie ---
    var bad = 0
    var first = String("")
    for lane in range(LANES):
        for i in range(4):
            var row = (lane // 4) + 8 * (i // 2)
            var col = 2 * (lane % 4) + (i % 2)
            var got = host[lane * 4 + i]
            var expected = want[row * N + col]
            if got != expected:
                bad += 1
                if first == "":
                    first = (
                        "m" + String(row) + "n" + String(col)
                        + " oczekiwane " + String(expected)
                        + ", otrzymane " + String(got)
                    )
    if bad == 0:
        print("ZGADZA SIE — wszystkie", M * N, "elementow co do bitu")
        print("hipoteza ukladu fragmentow A i B jest potwierdzona")
    else:
        print("ROZNICE:", bad, "z", M * N)
        print("pierwsza:", first)
