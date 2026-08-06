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

    tid = Int(thread_idx.x)
    threads = Int(block_dim.x)
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

    var block0 = 0
    while block0 < blocks_per_row:
        # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks: ich
        # pola i tak nie są zapisywane, a zacisk trzyma odczyt w buforze.
        var slot = tid
        while slot < TILE_W:
            row = slot // (KSTEP * BLOCK_WORDS)
            rest = slot % (KSTEP * BLOCK_WORDS)
            kblk = rest // BLOCK_WORDS
            word = rest % BLOCK_WORDS
            var src_row = base_row + row
            if src_row > n_rows - 1:
                src_row = n_rows - 1
            var src_blk = block0 + kblk
            if src_blk > blocks_per_row - 1:
                src_blk = blocks_per_row - 1
            sw[slot] = weights[
                (src_row * blocks_per_row + src_blk) * BLOCK_WORDS + word
            ]
            slot += threads

        slot = tid
        while slot < TILE_X:
            tok = slot // (KSTEP * BLOCK_WORDS)
            rest = slot % (KSTEP * BLOCK_WORDS)
            kblk = rest // BLOCK_WORDS
            word = rest % BLOCK_WORDS
            var src_tok = base_tok + tok
            if src_tok > n_tokens - 1:
                src_tok = n_tokens - 1
            var src_blk = block0 + kblk
            if src_blk > blocks_per_row - 1:
                src_blk = blocks_per_row - 1
            sx[slot] = x[
                (src_tok * blocks_per_row + src_blk) * BLOCK_WORDS + word
            ]
            slot += threads
        barrier()

        comptime for kblk in range(KSTEP):
            if block0 + kblk < blocks_per_row:
                var wa = InlineArray[UInt32, MT * 4](fill=UInt32(0))
                var wsx = InlineArray[UInt32, MT](fill=UInt32(0))
                comptime for mt in range(MT):
                    # Pas dostarcza słowo skali wiersza `quad` (in_quad == 0)
                    # albo `quad + 8` (in_quad == 1); pozostałe dwa pasy czwórki
                    # nie są czytane przez instrukcję i wtedy `in_quad * 8`
                    # daje ten sam adres co pas 0 albo 1 wiersza dalej — bez
                    # znaczenia, bo wynik i tak jest ignorowany.
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
                    b0 = sx[
                        (stok * KSTEP + kblk) * BLOCK_WORDS + 1 + in_quad
                    ]
                    b1 = sx[
                        (stok * KSTEP + kblk) * BLOCK_WORDS + 5 + in_quad
                    ]
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


comptime gemm_nvfp4_mma_f16_bm64_bn64 = gemm_nvfp4_mma_impl[2, 2, 2, 4, 4]
comptime gemm_nvfp4_mma_f16_bm128_bn128 = gemm_nvfp4_mma_impl[2, 4, 4, 4, 4]
