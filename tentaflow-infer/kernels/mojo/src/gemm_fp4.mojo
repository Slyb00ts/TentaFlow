# ===== File: gemm_fp4.mojo — GEMM na natywnym blokowo-skalowanym FP4 =====
# `mma...kind::mxf4nvf4` liczy 16x8x64 na jedną instrukcję i — zmierzone —
# wydaje ją w tym samym takcie co `m16n8k32.e4m3`, czyli 488 wobec 244 TFLOP/s.
#
# Blok GGUF NVFP4 trafia w ten fragment BEZ PRZEPAKOWANIA, i to nie jest zbieg
# okoliczności, tylko skutek tego, że oba układy powstały wokół tej samej
# jednostki. Cztery bajty nagłówka bloku SĄ słowem skali instrukcji: bajt `b`
# opisuje szesnastkę `b`, a k-blok `b` fragmentu pokrywa dokładnie tę szesnastkę.
# Trzydzieści dwa bajty ładunku są czterema rejestrami A pasa, czytanymi
# zwykłym 32-bitowym load'em spod `4 + 4 * (4 * (r // 2) + t)`.
#
# Aktywacje kwantyzuje `quantize_act_nvfp4` do TEGO SAMEGO układu bajtów, więc
# przestawienie k wewnątrz szesnastki nie wymaga niczego: liczy się wyłącznie to,
# żeby oba operandy przestawiały je JEDNAKOWO.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import UnsafePointer, stack_allocation

from src.mma_fp4 import _mma_nvf4

comptime BLOCK_VALUES = 64
"""Wartości w bloku GGUF NVFP4 — i `k` jednej instrukcji."""

comptime BLOCK_WORDS = 9
"""36 bajtów bloku jako słowa 32-bitowe: jedno skal, osiem ładunku."""


def gemm_nvfp4_mma_impl[
    WARPS_ROW: Int, WARPS_TOK: Int, MT: Int, NT: Int, KSTEP: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt32, MutAnyOrigin],
    x: UnsafePointer[UInt32, MutAnyOrigin],
    x_scale: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """`y[t, r] = dot(w[r], x[t]) * output_scale * x_scale[t]`, oba operandy FP4.

    Siatka `(ceil(n_rows / BROWS), ceil(n_tokens / BTOK))`, blok
    `WARPS_ROW * WARPS_TOK * 32` wątków, `n_cols % 64 == 0`. Układ wyjścia jest
    ten sam co w rodzinie WMMA — `[token, row]` — więc wywołujący nie widzi,
    którym kernelem policzono.
    """
    comptime BROWS = WARPS_ROW * MT * 16
    comptime BTOK = WARPS_TOK * NT * 8
    comptime TILE_W = BROWS * KSTEP * BLOCK_WORDS
    comptime TILE_X = BTOK * KSTEP * BLOCK_WORDS
    comptime THREADS = WARPS_ROW * WARPS_TOK * 32
    comptime WSLOTS = (TILE_W + THREADS - 1) // THREADS
    comptime XSLOTS = (TILE_X + THREADS - 1) // THREADS
    comptime STRIDE = KSTEP * BLOCK_WORDS

    tid = Int(thread_idx.x)
    lane = tid % 32
    warp = tid // 32
    quad = lane // 4
    in_quad = lane % 4

    base_row = Int(block_idx.x) * BROWS
    base_tok = Int(block_idx.y) * BTOK
    warp_row = (warp // WARPS_TOK) * MT * 16
    warp_tok = (warp % WARPS_TOK) * NT * 8

    blocks_per_row = n_cols // BLOCK_VALUES

    sw = stack_allocation[TILE_W, UInt32, address_space = AddressSpace.SHARED]()
    sx = stack_allocation[TILE_X, UInt32, address_space = AddressSpace.SHARED]()

    var acc = InlineArray[SIMD[DType.float32, 4], MT * NT](
        fill=SIMD[DType.float32, 4](0.0, 0.0, 0.0, 0.0)
    )

    # Kolejny kafel jest ŚCIĄGANY W REJESTRY, zanim policzy się bieżący. Bez
    # tego jedyny rezydentny blok stoi na barierze przez całą latencję pamięci
    # globalnej, a jednostka macierzowa czeka: zmierzone 62,9 wobec 38,0 TFLOP/s
    # to właśnie ta zwłoka, a nie przepustowość instrukcji.
    var stage_w = InlineArray[UInt32, WSLOTS](fill=UInt32(0))
    var stage_x = InlineArray[UInt32, XSLOTS](fill=UInt32(0))

    @parameter
    def fetch(block0: Int):
        comptime for i in range(WSLOTS):
            slot = tid + i * THREADS
            if slot < TILE_W:
                row = slot // STRIDE
                rest = slot % STRIDE
                var src_row = base_row + row
                if src_row > n_rows - 1:
                    src_row = n_rows - 1
                var src_blk = block0 + rest // BLOCK_WORDS
                if src_blk > blocks_per_row - 1:
                    src_blk = blocks_per_row - 1
                stage_w[i] = weights[
                    (src_row * blocks_per_row + src_blk) * BLOCK_WORDS
                    + rest % BLOCK_WORDS
                ]
        comptime for i in range(XSLOTS):
            slot = tid + i * THREADS
            if slot < TILE_X:
                tok = slot // STRIDE
                rest = slot % STRIDE
                var src_tok = base_tok + tok
                if src_tok > n_tokens - 1:
                    src_tok = n_tokens - 1
                var src_blk = block0 + rest // BLOCK_WORDS
                if src_blk > blocks_per_row - 1:
                    src_blk = blocks_per_row - 1
                stage_x[i] = x[
                    (src_tok * blocks_per_row + src_blk) * BLOCK_WORDS
                    + rest % BLOCK_WORDS
                ]

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks: ich pola
    # i tak nie są zapisywane, a zacisk trzyma odczyt w obrębie bufora.
    fetch(0)

    var block0 = 0
    while block0 < blocks_per_row:
        comptime for i in range(WSLOTS):
            slot = tid + i * THREADS
            if slot < TILE_W:
                sw[slot] = stage_w[i]
        comptime for i in range(XSLOTS):
            slot = tid + i * THREADS
            if slot < TILE_X:
                sx[slot] = stage_x[i]
        barrier()

        if block0 + KSTEP < blocks_per_row:
            fetch(block0 + KSTEP)

        comptime for kblk in range(KSTEP):
            if block0 + kblk < blocks_per_row:
                var wa = InlineArray[UInt32, MT * 4](fill=UInt32(0))
                var wsx = InlineArray[UInt32, MT](fill=UInt32(0))
                comptime for mt in range(MT):
                    # Pas dostarcza słowo skali wiersza `quad` (in_quad == 0)
                    # albo `quad + 8` (in_quad == 1); pozostałe dwa pasy czwórki
                    # nie są czytane przez instrukcję, więc ich adres jest bez
                    # znaczenia.
                    srow = warp_row + mt * 16 + quad + 8 * (in_quad % 2)
                    wsx[mt] = sw[(srow * KSTEP + kblk) * BLOCK_WORDS]
                    comptime for r in range(4):
                        row = warp_row + mt * 16 + quad + 8 * (r % 2)
                        wa[mt * 4 + r] = sw[
                            (row * KSTEP + kblk) * BLOCK_WORDS
                            + 1
                            + 4 * (r // 2)
                            + in_quad
                        ]

                comptime for nt in range(NT):
                    stok = warp_tok + nt * 8 + quad
                    xs = sx[(stok * KSTEP + kblk) * BLOCK_WORDS]
                    b0 = sx[(stok * KSTEP + kblk) * BLOCK_WORDS + 1 + in_quad]
                    b1 = sx[(stok * KSTEP + kblk) * BLOCK_WORDS + 5 + in_quad]
                    comptime for mt in range(MT):
                        acc[mt * NT + nt] = _mma_nvf4(
                            wa[mt * 4],
                            wa[mt * 4 + 1],
                            wa[mt * 4 + 2],
                            wa[mt * 4 + 3],
                            b0,
                            b1,
                            wsx[mt],
                            xs,
                            acc[mt * NT + nt],
                        )
        barrier()
        block0 += KSTEP

    comptime for mt in range(MT):
        comptime for nt in range(NT):
            comptime for i in range(4):
                row = base_row + warp_row + mt * 16 + quad + 8 * (i // 2)
                tok = base_tok + warp_tok + nt * 8 + 2 * in_quad + i % 2
                if row < n_rows and tok < n_tokens:
                    y[tok * n_rows + row] = Float16(
                        acc[mt * NT + nt][i] * output_scale * x_scale[tok]
                    )


# Kafel jest tu ograniczony REJESTRAMI AKUMULATORA: `BROWS * BTOK` pól f32
# rozkłada się na wątki bloku, więc 256x128 przy 512 wątkach to już 64 rejestry
# na wątek zanim policzy się fragmenty. Większy kafel czyta za to wagi mniej
# razy, a przy 2048 tokenach to właśnie ruch, a nie jednostka macierzowa,
# wyznacza czas.
# `KSTEP` jest teraz mały z tego samego powodu, dla którego kafel jest duży:
# pobranie następnego kroku żyje w REJESTRACH, więc każdy dodatkowy blok `k`
# to `(BROWS + BTOK) * 9 / THREADS` rejestrów na wątek. Przy `KSTEP = 4`
# wariant BM128/BN128 zaczynał zrzucać akumulator do pamięci lokalnej.
comptime gemm_nvfp4_mma_f16_bm64_bn64 = gemm_nvfp4_mma_impl[2, 2, 2, 4, 1]
comptime gemm_nvfp4_mma_f16_bm128_bn128 = gemm_nvfp4_mma_impl[2, 4, 4, 4, 1]
comptime gemm_nvfp4_mma_f16_bm128_bn256 = gemm_nvfp4_mma_impl[2, 4, 4, 8, 1]
