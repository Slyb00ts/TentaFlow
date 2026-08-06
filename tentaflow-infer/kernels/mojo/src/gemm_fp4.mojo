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

from src.mma_fp4 import _mma_mxf4, _mma_nvf4

comptime BLOCK_VALUES = 64
"""Wartości w bloku GGUF NVFP4 — i `k` jednej instrukcji."""

comptime BLOCK_WORDS = 9
"""36 bajtów bloku jako słowa 32-bitowe: jedno skal, osiem ładunku."""

comptime MXFP4_BYTES = 17
"""Blok GGUF MXFP4: bajt skali E8M0 i szesnaście bajtów par e2m1."""


@always_inline
def _mxfp4_word(
    src: UnsafePointer[UInt8, MutAnyOrigin], row_base: Int, blk: Int, word: Int
) -> UInt32:
    """Słowo `word` bloku fragmentu, ZŁOŻONE Z BLOKÓW GGUF MXFP4 W LOCIE.

    Blok MXFP4 ma 17 bajtów, więc nie da się go podać jednostce macierzowej tam,
    gdzie leży — ale jego BAJTY są już właściwymi bajtami, bo bajt skali GGML-a
    jest bajtem `ue8m0` instrukcji, a kodowanie półbajtu jest identyczne z e2m1.
    Zmienia się wyłącznie wyrównanie, więc para bloków składa się tutaj, przy
    wpisywaniu kafla do pamięci współdzielonej, zamiast w drugiej kopii wagi na
    całą kartę: dla 27B ta kopia to 17 GiB.
    """
    lo = src + row_base + 2 * blk * MXFP4_BYTES
    if word == 0:
        return UInt32(lo[0]) | (UInt32(lo[MXFP4_BYTES]) << 8)
    half = (word - 1) // 4
    b = lo + half * MXFP4_BYTES + 1 + 4 * ((word - 1) % 4)
    v = b.load[width=4, alignment=1]()
    return (
        UInt32(v[0])
        | (UInt32(v[1]) << 8)
        | (UInt32(v[2]) << 16)
        | (UInt32(v[3]) << 24)
    )


def _gemm_fp4_tile[
    WARPS_ROW: Int, WARPS_TOK: Int, MT: Int, NT: Int, KSTEP: Int, NVF4: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[UInt32, MutAnyOrigin],
    x_scale: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    base_row: Int,
    base_tok: Int,
    tok_end: Int,
    output_scale: Float32,
):
    """`y[t, r] = dot(w[r], x[t]) * output_scale * x_scale[t]`, oba operandy FP4.

    `NVF4` wybiera skale per 16 w E4M3 (`NVFP4Gguf`) albo per 32 w UE8M0
    (`MXFP4`). To JEDYNA różnica między tymi dwoma formatami w tym kaflu: blok
    36-bajtowy, adresowanie i miejsce słowa skali są wspólne, bo obie postaci
    zostały do tego samego fragmentu przepakowane.

    `base_row`, `base_tok` i `tok_end` przychodzą z zewnątrz, żeby ten sam kafel
    obsłużył pojedynczy GEMM i wariant zgrupowany, w którym o tych trzech
    liczbach decyduje tablica kafli ekspertów, a nie siatka.
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
                if NVF4 == 1:
                    stage_w[i] = weights.bitcast[UInt32]()[
                        (src_row * blocks_per_row + src_blk) * BLOCK_WORDS
                        + rest % BLOCK_WORDS
                    ]
                else:
                    stage_w[i] = _mxfp4_word(
                        weights,
                        src_row * blocks_per_row * 2 * MXFP4_BYTES,
                        src_blk,
                        rest % BLOCK_WORDS,
                    )
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
                        if NVF4 == 1:
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
                        else:
                            acc[mt * NT + nt] = _mma_mxf4(
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
                if row < n_rows and tok < tok_end:
                    y[tok * n_rows + row] = Float16(
                        acc[mt * NT + nt][i] * output_scale * x_scale[tok]
                    )


# Kafel jest tu ograniczony REJESTRAMI AKUMULATORA: `BROWS * BTOK` pól f32
# rozkłada się na wątki bloku, więc 256x128 przy 512 wątkach to już 64 rejestry
# na wątek zanim policzy się fragmenty. Większy kafel czyta za to wagi mniej
# razy, a przy 2048 tokenach to właśnie ruch, a nie jednostka macierzowa,
# wyznacza czas.
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
    """Jedna waga, wszystkie tokeny kroku; siatka wybiera kafel."""
    comptime BROWS = WARPS_ROW * MT * 16
    comptime BTOK = WARPS_TOK * NT * 8
    _gemm_fp4_tile[WARPS_ROW, WARPS_TOK, MT, NT, KSTEP, 1](
        y,
        weights.bitcast[UInt8](),
        x,
        x_scale,
        n_cols,
        n_rows,
        n_tokens,
        Int(block_idx.x) * BROWS,
        Int(block_idx.y) * BTOK,
        n_tokens,
        output_scale,
    )


def gemm_mxf4_grouped_impl[
    WARPS_ROW: Int, WARPS_TOK: Int, MT: Int, NT: Int, KSTEP: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    wtab: UnsafePointer[UnsafePointer[UInt8, MutAnyOrigin], MutAnyOrigin],
    x: UnsafePointer[UInt32, MutAnyOrigin],
    x_scale: UnsafePointer[Float32, MutAnyOrigin],
    tile_expert: UnsafePointer[Int32, MutAnyOrigin],
    tile_first: UnsafePointer[Int32, MutAnyOrigin],
    tile_end: UnsafePointer[Int32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Wszyscy eksperci kroku w JEDNYM uruchomieniu, na czterech bitach.

    Mieszanka MXFP4 uruchamiała jeden GEMM na eksperta na projekcję, a każdy z
    nich obejmował osiem bloków — karta o kilkudziesięciu multiprocesorach stała
    prawie bezczynnie, przechodząc trzydzieści tysięcy takich uruchomień na
    prompt. Tu `block_idx.y` indeksuje KAFEL, kafel mówi, do którego eksperta
    należy i które wiersze zgrupowanej aktywacji obejmuje, a siatka pokrywa
    wszystkich ekspertów naraz.

    `tile_end` to koniec bloku TEGO eksperta, a nie kafla, więc zacisk pobrania
    ląduje wewnątrz eksperta, do którego należy, i nigdy na wierszach sąsiada.
    """
    comptime BROWS = WARPS_ROW * MT * 16
    tile = Int(block_idx.y)
    _gemm_fp4_tile[WARPS_ROW, WARPS_TOK, MT, NT, KSTEP, 0](
        y,
        wtab[Int(tile_expert[tile])],
        x,
        x_scale,
        n_cols,
        n_rows,
        n_tokens,
        Int(block_idx.x) * BROWS,
        Int(tile_first[tile]),
        Int(tile_end[tile]),
        output_scale,
    )


# `KSTEP` jest teraz mały z tego samego powodu, dla którego kafel jest duży:
# pobranie następnego kroku żyje w REJESTRACH, więc każdy dodatkowy blok `k`
# to `(BROWS + BTOK) * 9 / THREADS` rejestrów na wątek. Przy `KSTEP = 4`
# wariant BM128/BN128 zaczynał zrzucać akumulator do pamięci lokalnej.
comptime gemm_nvfp4_mma_f16_bm64_bn64 = gemm_nvfp4_mma_impl[2, 2, 2, 4, 1]
comptime gemm_nvfp4_mma_f16_bm128_bn128 = gemm_nvfp4_mma_impl[2, 4, 4, 4, 1]
comptime gemm_nvfp4_mma_f16_bm128_bn256 = gemm_nvfp4_mma_impl[2, 4, 4, 8, 1]

# Kafel zgrupowany jest WĄSKI W TOKENACH, bo taki jest ruch: przy 256 ekspertach
# i ośmiu wybranych na token na jednego eksperta przypada kilkanaście wierszy, a
# kafel szerszy liczyłby zera. Wiersze wyjścia zostają szerokie, bo to one
# amortyzują odczyt wagi eksperta.
comptime gemm_mxf4_grouped_f16_bm128_bn16 = gemm_mxf4_grouped_impl[4, 1, 2, 2, 1]
comptime gemm_mxf4_grouped_f16_bm128_bn32 = gemm_mxf4_grouped_impl[4, 1, 2, 4, 1]
