# =============================================================================
# Plik: probe_nvfp4_mma_layout.mojo
# Opis: Mapuje uklad operandow skal w natywnym NVFP4 `mma` na sm_121a.
# Przyklad: pixi run mojo probe_nvfp4_mma_layout.mojo
# =============================================================================
#
# `mma.sync.aligned.kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64...ue4m3`
# bierze skale przez trzy rzeczy naraz: rejestr `.b32` w KAZDYM watku, oraz pare
# `{byte-id, thread-id}` wybierajaca, ktore bajty ktorych watkow faktycznie licza.
# Zanim cokolwiek z tego trafi do GEMM-u, trzeba wiedziec, ktory bajt ktorego
# watku odpowiada za ktory wiersz A (i kolumne B).
#
# Metoda jest odporna na to, czego jeszcze nie wiemy o ukladzie SAMYCH danych:
# A i B sa w calosci jedynkami (e2m1 1.0 = 0x2, wiec bajt 0x22), a skale B sa
# jedynkami (ue4m3 1.0 = 0x38). Wtedy
#
#     out(m, n) = sum_j 16 * sA[m, j] * sB[n, j] = 16 * sum_j sA[m, j]
#
# czyli wynik zalezy WYLACZNIE od skal A i jest staly wzdluz n. Podnosimy jedna
# skale do 2.0 (ue4m3 0x40) i patrzymy, ktory wiersz drgnal. Baza to 4*16 = 64.
#
# WYNIK (sm_121a, zweryfikowany tym probem):
#
#   skale A — pas `4*r + q` niesie skale wiersza:
#       q = 0 -> wiersz r        (r = 0..7)
#       q = 1 -> wiersz r + 8
#       q = 2, 3 -> nie licza sie
#     `thread-id` NIEPARZYSTY przesuwa te pare: wtedy licza pasy q = 2, 3,
#     odwzorowane tak samo (q=2 -> wiersz r, q=3 -> wiersz r+8).
#
#   skale B — pas `4*n` niesie skale kolumny n (n = 0..7); pozostale pasy nie
#     licza sie. `thread-id` dziala tak samo jak dla A.
#
#   bajt rejestru = blok K. Bajt j odpowiada wartosciom k = 16*j .. 16*j+15,
#     po obu stronach. Caly rejestr `.b32` jest zuzywany, wiec `byte-id` NIE MA
#     ZNACZENIA przy `scale_vec::4X` — sprawdzone dla bid = 0..3.
#
# Stad uklad do zapakowania w GEMM-ie: skale A ida do pasow 4r i 4r+1 (wiersze
# r i r+8), skale B do pasow 4n, oba po cztery bajty na 64 wartosci K.

from std.gpu import block_idx, thread_idx
from std.gpu.host import DeviceBuffer, DeviceContext, HostBuffer
from std.gpu.intrinsics import inlined_assembly
from std.sys import _RegisterPackType

comptime ONES_E2M1 = UInt32(0x22222222)  # osiem wartosci 1.0
comptime ONE_UE4M3 = UInt32(0x38383838)  # cztery skale 1.0
comptime LANES = 32


def mma_nvfp4(
    a0: UInt32, a1: UInt32, a2: UInt32, a3: UInt32,
    b0: UInt32, b1: UInt32,
    c: SIMD[DType.float32, 4],
    sa: UInt32, sb: UInt32, bid: UInt16, tid: UInt16,
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
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb, bid, tid)


def kern(
    scales_a: UnsafePointer[UInt32, MutAnyOrigin],
    scales_b: UnsafePointer[UInt32, MutAnyOrigin],
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    bid: Int,
    tid: Int,
):
    lane = Int(thread_idx.x)
    var acc = SIMD[DType.float32, 4](0.0)
    var r = mma_nvfp4(
        ONES_E2M1, ONES_E2M1, ONES_E2M1, ONES_E2M1,
        ONES_E2M1, ONES_E2M1,
        acc,
        scales_a[lane], scales_b[lane],
        UInt16(bid), UInt16(tid),
    )
    comptime for i in range(4):
        out_ptr[lane * 4 + i] = r[i]


def _rows_touched(
    ctx: DeviceContext,
    mut sa: DeviceBuffer[DType.uint32],
    mut sb: DeviceBuffer[DType.uint32],
    mut out: DeviceBuffer[DType.float32],
    host: HostBuffer[DType.float32],
    side_b: Bool,
    lane: Int,
    byte: Int,
    bid: Int,
    tid: Int,
) raises -> String:
    """Zwraca wspolrzedne wynikow, ktore odpowiedzialy na jedna podniesiona skale."""
    # 0x40 to ue4m3 2.0 (wykladnik 8 przy obciazeniu 7)
    var bumped = (ONE_UE4M3 & ~(UInt32(0xFF) << UInt32(byte * 8))) | (
        UInt32(0x40) << UInt32(byte * 8)
    )
    with sa.map_to_host() as v:
        for i in range(LANES):
            v[i] = ONE_UE4M3
        if not side_b:
            v[lane] = bumped
    with sb.map_to_host() as v:
        for i in range(LANES):
            v[i] = ONE_UE4M3
        if side_b:
            v[lane] = bumped
    ctx.enqueue_function[kern](
        sa.unsafe_ptr(), sb.unsafe_ptr(), out.unsafe_ptr(), bid, tid,
        grid_dim=1, block_dim=LANES,
    )
    ctx.enqueue_copy(host, out)
    ctx.synchronize()

    var touched = String("")
    for lane_i in range(LANES):
        for i in range(4):
            var value = host[lane_i * 4 + i]
            if value != 64.0:
                # uklad akumulatora m16n8: wiersz = groupID + 8*(i/2)
                var row = (lane_i // 4) + 8 * (i // 2)
                var col = 2 * (lane_i % 4) + (i % 2)
                touched += (
                    " m" + String(row) + "n" + String(col) + "=" + String(value)
                )
    if touched == "":
        return "(nic nie drgnelo)"
    return touched


def main() raises:
    var ctx = DeviceContext()
    var sa = ctx.enqueue_create_buffer[DType.uint32](LANES)
    var sb = ctx.enqueue_create_buffer[DType.uint32](LANES)
    var out = ctx.enqueue_create_buffer[DType.float32](LANES * 4)
    var host = ctx.enqueue_create_host_buffer[DType.float32](LANES * 4)

    print("--- A: thread-id=1 ma brac druga pare pasow w kwadzie ---")
    for lane in range(4):
        print(
            "  lane " + String(lane) + ":",
            _rows_touched(ctx, sa, sb, out, host, False, lane, 0, 0, 1),
        )

    print("")
    print("--- B: ktory pas niesie skale ktorej kolumny (thread-id=0) ---")
    for lane in range(LANES):
        var r = _rows_touched(ctx, sa, sb, out, host, True, lane, 0, 0, 0)
        if r != "(nic nie drgnelo)":
            print("  lane " + String(lane) + ":", r)

    print("")
    print("--- B: bajt rejestru wobec bloku K (pas 0) ---")
    for byte in range(4):
        print(
            "  bajt " + String(byte) + ":",
            _rows_touched(ctx, sa, sb, out, host, True, 0, byte, 0, 0),
        )
