# =============================================================================
# Plik: nvfp4_gguf_wmma.mojo
# Opis: Kafelkowany GEMM NVFP4 na jednostkach macierzowych RDNA3 (WMMA), czytający
#       wprost bloki GGUF NVFP4 36 B / 64 wartości. Odpowiednik rodziny
#       `gemm_nvfp4_gguf_mma_*` dla kart bez instrukcji `mma`/`ldmatrix`.
# Przykład: gemm_nvfp4_gguf_wmma_f16_bm128 obsługuje długi prefill bez repacku.
# =============================================================================
#
# DLACZEGO K=16 JEST TU NATURALNE: blok GGUF NVFP4 ma 64 wartości i CZTERY skale
# UE4M3, po jednej na 16 kolejnych kolumn. Kafel WMMA ma dokładnie K=16, więc
# jedno wywołanie pokrywa równo jeden podblok skali — skalę wolno wtedy wmnożyć
# w rozpakowane wagi PRZED mnożeniem macierzowym, zamiast rozbijać akumulację.
# Iloczyn kodu e2m1 (najwyżej 6, jeden bit mantysy) i skali UE4M3 (trzy bity
# mantysy) mieści się w f16 dokładnie, a akumulacja i tak idzie w f32.
#
# Układ fragmentów wave32 jest opisany w `src/arch_wmma.mojo`; tu korzystamy
# z tego, że linia niesie CAŁY swój wiersz po K, więc zarówno 16 f16 aktywacji,
# jak i 8 bajtów kodów wagi są odczytem ciągłym.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

from src.arch_wmma import wmma_f16_16x16x16, wmma_acc_row
from src.gemv2 import _e2m1x8
from src.nvfp4_gguf_batch import _ue4m3_branchless

comptime TILE = 16
comptime BLOCK_VALUES = 64  # wartości opisane jednym blokiem GGUF
comptime BLOCK_BYTES = 36  # 4 bajty skal + 32 bajty kodów
comptime SUBBLOCKS = 4  # podbloki po 16 kolumn, każdy z własną skalą


@always_inline
def _weight_frag(
    weights: UnsafePointer[UInt8, MutAnyOrigin], base: Int, subblock: Int
) -> SIMD[DType.float16, 16]:
    """Szesnaście wag jednego wiersza dla podbloku, już przeskalowanych.

    Bajt `i` niesie kolumnę `i` w młodszym półbajcie i kolumnę `i + 8` w
    starszym — stąd fragment składa się z dwóch połówek, a nie z przeplotu.
    """
    codes = (weights + base + 4 + subblock * 8).load[width=8, alignment=1]()
    scale = _ue4m3_branchless(weights[base + subblock])
    low = (_e2m1x8(codes & 0x0F) * scale).cast[DType.float16]()
    high = (_e2m1x8(codes >> 4) * scale).cast[DType.float16]()
    return low.join(high)


def gemm_nvfp4_gguf_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """GEMM NVFP4 na WMMA: Y[t, r] = dot(w[r], x[t]) * output_scale.

    Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)), blok WAVES_M*WAVES_N*32
    wątków, `n_cols % 64 == 0` (niezmiennik bloku GGUF NVFP4). Kontrakt
    argumentów jest identyczny z rodziną `gemm_nvfp4_gguf_mma_*`, więc launcher
    wybiera wariant wyłącznie po architekturze.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var blocks_per_row = n_cols // BLOCK_VALUES

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks — ich wyniki
    # i tak nie są zapisywane, a zacisk trzyma odczyty w obrębie buforów.
    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols
    var w_base = InlineArray[Int, NTILE](fill=0)
    comptime for nt in range(NTILE):
        var n = base_n + nt * TILE + lane % 16
        if n > n_rows - 1:
            n = n_rows - 1
        w_base[nt] = n * blocks_per_row * BLOCK_BYTES

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    # Wagi rozpakowujemy RAZ NA BLOK do LDS. Bez tego kazda z WAVES_M fal wzdluz
    # tokenow dekwantyzowala DOKLADNIE TE SAME kolumny — a pomiar z podmieniona
    # na stala waga pokazal, ze sama dekwantyzacja to polowa czasu kernela
    # (25 wobec 48 TFLOPS). Kafel BN x 64 wartosci to 8 KiB dla BN=64.
    comptime WEIGHT_TILE = BN * BLOCK_VALUES
    ws = stack_allocation[
        WEIGHT_TILE, Float16, address_space = AddressSpace.SHARED
    ]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    for block in range(blocks_per_row):
        # Kazdy watek bierze kolejne podbloki (wiersz, podblok) az wyczerpie kafel.
        var slot = tid
        while slot < BN * SUBBLOCKS:
            local_row = slot // SUBBLOCKS
            subblock = slot % SUBBLOCKS
            var source_row = Int(block_idx.x) * BN + local_row
            if source_row > n_rows - 1:
                source_row = n_rows - 1
            frag = _weight_frag(
                weights,
                source_row * blocks_per_row * BLOCK_BYTES + block * BLOCK_BYTES,
                subblock,
            )
            (ws + local_row * BLOCK_VALUES + subblock * TILE).store(frag)
            slot += threads
        barrier()

        comptime for sub in range(SUBBLOCKS):
            var column = block * BLOCK_VALUES + sub * TILE
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + local_n * BLOCK_VALUES + sub * TILE).load[width=16]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(
                        acc[mt * NTILE + nt][i] * output_scale
                    )


# Dwa kafle, oba wybrane pomiarem na 7900 XT (sweep w
# `bench-amd/bench_gemm_nvfp4_wmma_tiles.mojo`, ksztalty projekcji 27B):
#  - BM256/BN64 na osmiu falach: 52/50/45 TFLOPS, najlepszy w kazdym ksztalcie,
#  - BM32/BN64 dla krotkich chunkow, gdzie duzy kafel nie ma czym wypelnic fal.
# Wariant BN32 z rodziny `mma` NIE ma tu odpowiednika: powstal ze strojenia pod
# NVIDIE i nic go na tej karcie nie uzasadnia.
#
# BM=512 SPRAWDZONE I ODRZUCONE (2026-07-29). Rachunek mowil, ze powinno pomoc:
# wagi rozpakowuja sie raz na blok, wiec calkowity koszt dekwantyzacji skaluje
# sie jak 1/BM. Pomiar mowi inaczej:
#   [4,2,8,2] BM512 na osmiu falach: 14/13/16 TFLOPS — MTILE=8 to szesnascie
#     akumulatorow po 8 VGPR na fale, rejestry sie nie mieszcza,
#   [8,2,4,2] BM512 na szesnastu falach: 48/47/48 TFLOPS — mieszcza sie, ale
#     polowa blokow to za malo rownoleglosci i wychodzi PONIZEJ BM256.
# Przy BM256 dekwantyzacja nie jest juz waskim gardlem, wiec jej dalsze
# tanienie niczego nie kupuje.
#
# PODWOJNY BUFOR WAG W LDS SPRAWDZONY I ODRZUCONY (2026-07-29). Nastepny kafel
# dekwantyzowal sie w trakcie liczenia na biezacym, jedna bariera na obrocie
# zamiast dwoch. W izolacji wygrywal na wszystkich trzech ksztaltach 27B:
# 52->55, 48->51, 50->53 TFLOPS. NA MODELU NIE DAL NIC — profil rocprof pokazal
# 3488,6 wobec 3477,9 ms na te same 608 wywolan, a pelny prefill 836,3 wobec
# 831,3 tok/s. Podwojenie kafla LDS zabiera zajetosc, co w realnym obciazeniu
# kasuje zysk z nakladania. Wniosek ogolny: izolowany sweep kafli NIE przenosi
# sie tu na model — kazda zmiane trzeba domierzyc na `bench`.
#
# BM=512 SPRAWDZONE I ODRZUCONE (2026-07-29). Rachunek mowil, ze powinno pomoc:
# wagi rozpakowuja sie raz na blok, wiec calkowity koszt dekwantyzacji skaluje
# sie jak 1/BM. Pomiar mowi inaczej:
#   [4,2,8,2] BM512 na osmiu falach: 14/13/16 TFLOPS — MTILE=8 to szesnascie
#     akumulatorow po 8 VGPR na fale, rejestry sie nie mieszcza,
#   [8,2,4,2] BM512 na szesnastu falach: 48/47/48 TFLOPS — mieszcza sie, ale
#     polowa blokow to za malo rownoleglosci i wychodzi PONIZEJ BM256.
# Przy BM256 dekwantyzacja nie jest juz waskim gardlem, wiec jej dalsze
# tanienie niczego nie kupuje. Nie powtarzac tej proby bez nowego argumentu.
comptime gemm_nvfp4_gguf_wmma_f16_bm32 = gemm_nvfp4_gguf_wmma_impl[2, 2, 1, 2]
comptime gemm_nvfp4_gguf_wmma_f16_bm256 = gemm_nvfp4_gguf_wmma_impl[4, 2, 4, 2]
