# =============================================================================
# Plik: bench_v_transpose.mojo
# Opis: Izoluje koszt transpozycji V przy wstawianiu kafla KV w uwadze prefillu.
# Przyklad: pixi run mojo bench_v_transpose.mojo
# =============================================================================
#
# `attn_prefill_fa_mma` trzyma V w pamieci wspoldzielonej transponowane
# ([head_dim][key]), zeby `ld_matrix` czytal je nietransponowane. Transpozycja
# dzieje sie przy wstawianiu kafla i jest SKALARNA: kazdy watek laduje 8
# sasiednich wartosci head_dim i rozrzuca je osobnymi zapisami po 2 bajty.
#
# Rozklad bankow jest przy tym najgorszy z mozliwych. Watki 0-15 maja ten sam
# `row`, a ich adresy roznia sie o `8*BK` wartosci f16, czyli 512 bajtow —
# wszystkie trafiaja w ten sam bank. To 16-drozny konflikt razy osiem zapisow.
#
# Dopelnienie wiersza tego NIE naprawi: zapis potrzebuje kroku niepodzielnego
# przez 8 wartosci f16, a `ld_matrix` wymaga wierszy 16-bajtowych, czyli kroku
# podzielnego przez 8. Warunki sa sprzeczne. Ten benchmark mierzy, ile jest do
# odzyskania, zanim przepiszemy odczyt na `ldmatrix.trans`.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.gpu.host import DeviceBuffer, DeviceContext
from std.memory import stack_allocation
from std.time import perf_counter_ns

comptime HD = 128
comptime BK = 32
comptime BLOCK = 128
comptime TILES = 64      # kafli KV na blok, jak przy dlugim kontekscie
comptime BLOCKS = 512    # 16 kafli zapytan x 32 glowice, jak w prawdziwym wywolaniu
comptime ROUNDS = 7
comptime ITERS = 20


def stage_transposed(
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    sink: UnsafePointer[Float16, MutAnyOrigin],
):
    """Dzisiejsze wstawianie: K wektorowo, V skalarnym scatterem."""
    tid = Int(thread_idx.x)
    ks = stack_allocation[BK * HD, Float16, address_space = AddressSpace.SHARED]()
    vs = stack_allocation[HD * BK, Float16, address_space = AddressSpace.SHARED]()
    var acc = Float16(0.0)
    for tile in range(TILES):
        barrier()
        var e = tid * 8
        while e < BK * HD:
            row = e // HD
            col = e % HD
            base = ((tile * BK + row) % 1024) * HD + col
            (ks + e).store[width=8, alignment=16](
                (k_cache + base).load[width=8, alignment=16]()
            )
            vv = (v_cache + base).load[width=8, alignment=16]()
            comptime for i in range(8):
                vs[(col + i) * BK + row] = vv[i]
            e += BLOCK * 8
        barrier()
        acc += ks[tid] + vs[tid]
    if acc == Float16(1234.5):
        sink[Int(block_idx.x)] = acc


def stage_plain(
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    sink: UnsafePointer[Float16, MutAnyOrigin],
):
    """Wstawianie bez transpozycji: V idzie tak samo jak K, wektorowo.

    Wynik nie jest uzyteczny dla P*V bez zmiany odczytu na `ldmatrix.trans` —
    to jest DOLNA GRANICA kosztu wstawiania kafla, czyli nagroda do wziecia.
    """
    tid = Int(thread_idx.x)
    ks = stack_allocation[BK * HD, Float16, address_space = AddressSpace.SHARED]()
    vs = stack_allocation[BK * HD, Float16, address_space = AddressSpace.SHARED]()
    var acc = Float16(0.0)
    for tile in range(TILES):
        barrier()
        var e = tid * 8
        while e < BK * HD:
            row = e // HD
            col = e % HD
            base = ((tile * BK + row) % 1024) * HD + col
            (ks + e).store[width=8, alignment=16](
                (k_cache + base).load[width=8, alignment=16]()
            )
            (vs + e).store[width=8, alignment=16](
                (v_cache + base).load[width=8, alignment=16]()
            )
            e += BLOCK * 8
        barrier()
        acc += ks[tid] + vs[tid]
    if acc == Float16(1234.5):
        sink[Int(block_idx.x)] = acc


def _median(mut v: InlineArray[Float64, ROUNDS]) -> Float64:
    for a in range(ROUNDS):
        for b in range(a + 1, ROUNDS):
            if v[b] < v[a]:
                v[a], v[b] = v[b], v[a]
    return v[ROUNDS // 2]


def main() raises:
    var ctx = DeviceContext()
    var kc = ctx.enqueue_create_buffer[DType.float16](1024 * HD)
    var vc = ctx.enqueue_create_buffer[DType.float16](1024 * HD)
    var sink = ctx.enqueue_create_buffer[DType.float16](BLOCKS)

    # rozgrzewka zegarow
    for _ in range(200):
        ctx.enqueue_function[stage_transposed](
            kc.unsafe_ptr(), vc.unsafe_ptr(), sink.unsafe_ptr(),
            grid_dim=BLOCKS, block_dim=BLOCK,
        )
    ctx.synchronize()

    var t = InlineArray[Float64, ROUNDS](uninitialized=True)
    for r in range(ROUNDS):
        var s0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[stage_transposed](
                kc.unsafe_ptr(), vc.unsafe_ptr(), sink.unsafe_ptr(),
                grid_dim=BLOCKS, block_dim=BLOCK,
            )
        ctx.synchronize()
        t[r] = Float64(perf_counter_ns() - s0) / Float64(ITERS)
    var a = _median(t)

    for _ in range(50):
        ctx.enqueue_function[stage_plain](
            kc.unsafe_ptr(), vc.unsafe_ptr(), sink.unsafe_ptr(),
            grid_dim=BLOCKS, block_dim=BLOCK,
        )
    ctx.synchronize()
    for r in range(ROUNDS):
        var s0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[stage_plain](
                kc.unsafe_ptr(), vc.unsafe_ptr(), sink.unsafe_ptr(),
                grid_dim=BLOCKS, block_dim=BLOCK,
            )
        ctx.synchronize()
        t[r] = Float64(perf_counter_ns() - s0) / Float64(ITERS)
    var b = _median(t)

    print("V transponowane (dzis):", a / 1000.0, "us")
    print("V wektorowo (dolna granica):", b / 1000.0, "us")
    print("koszt samej transpozycji:", (a - b) / 1000.0, "us, czyli", a / b, "x")
