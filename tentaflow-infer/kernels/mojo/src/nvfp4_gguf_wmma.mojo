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

from std.gpu import block_idx, thread_idx

from src.arch_wmma import wmma_f16_16x16x16
from src.gemv2 import _e2m1x8
from src.nvfp4_gguf_batch import _ue4m3_value

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
    scale = _ue4m3_value(weights[base + subblock])
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

    for block in range(blocks_per_row):
        for subblock in range(SUBBLOCKS):
            var column = block * BLOCK_VALUES + subblock * TILE
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                var b = _weight_frag(
                    weights, w_base[nt] + block * BLOCK_BYTES, subblock
                )
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                var m = base_m + mt * TILE + i * 2 + lane // 16
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(
                        acc[mt * NTILE + nt][i] * output_scale
                    )


# Kafle odwzorowujące rodzinę mma NVIDII: BM32/BN64, BM128/BN64, BM128/BN32.
comptime gemm_nvfp4_gguf_wmma_f16_bm32 = gemm_nvfp4_gguf_wmma_impl[2, 2, 1, 2]
comptime gemm_nvfp4_gguf_wmma_f16_bm128 = gemm_nvfp4_gguf_wmma_impl[2, 2, 4, 2]
comptime gemm_nvfp4_gguf_wmma_f16_bm128_bn32 = gemm_nvfp4_gguf_wmma_impl[2, 2, 4, 1]
