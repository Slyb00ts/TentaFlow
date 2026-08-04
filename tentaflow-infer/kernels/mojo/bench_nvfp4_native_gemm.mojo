# =============================================================================
# Plik: bench_nvfp4_native_gemm.mojo
# Opis: Rdzen GEMM-u na natywnym NVFP4 `mma` — poprawnosc przeciw CPU i pomiar.
# Przyklad: pixi run mojo bench_nvfp4_native_gemm.mojo
# =============================================================================
#
# GEMM na `kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64` — uklad operandow
# jest zweryfikowany w `probe_nvfp4_mma_layout.mojo` i `probe_nvfp4_mma_golden.mojo`.
#
# Dwa jadra stoja tu obok siebie i licza BITOWO TO SAMO:
#   `gemm_nvfp4`       — jeden warp na kafel 16x64, fragmenty wprost z pamieci
#                        globalnej. Rdzen referencyjny: prosty do przeczytania,
#                        sprawdzony przeciw CPU, i podloga pomiaru.
#   `gemm_nvfp4_tiled` — kafel BM x BN liczony przez NW warpow, oba operandy
#                        przez potok `cp.async` w dynamicznej pamieci
#                        wspoldzielonej, siatka przenumerowana pod L2.
# Kolejnosc sumowania po K jest w obu identyczna, wiec kafelkowanie mozna
# sprawdzac tym samym testem na rownosc CO DO BITU, a nie na tolerancje.
#
# Uklad danych wejsciowych jest taki, jaki ma model: A i B trzymaja kody e2m1
# po dwa na bajt (mlodsza polbajtowka to mniejsze k), a skale ue4m3 po jednej na
# 16 wartosci K. Dzieki temu kazdy fragment jest JEDNYM odczytem 4-bajtowym:
#
#   a0 -> A[wiersz][8q .. 8q+7]        = u32 pod indeksem row*(K/8) + 8*kb + q
#   a2 -> A[wiersz][32+8q .. ]         = ten sam indeks + 4
#   skale wiersza dla calego kroku K64 = u32 pod indeksem row*(K/64) + kb

from std.gpu import block_idx, thread_idx
from std.gpu.host import DeviceContext
from std.gpu.intrinsics import inlined_assembly
from std.gpu.memory import (
    AddressSpace,
    async_copy,
    async_copy_commit_group,
    async_copy_wait_group,
    external_memory,
)
from std.gpu.compute.mma import ld_matrix
from std.memory import bitcast
from std.gpu.sync import barrier
from std.sys import _RegisterPackType, size_of
from std.time import perf_counter_ns

comptime LANES = 32
comptime ROUNDS = 5
comptime ITERS = 10


def _smem_bytes[BM: Int, BN: Int, KC: Int, NS: Int]() -> Int:
    """Bajty pamieci wspoldzielonej kafla: kody + skale, razy glebokosc potoku.

    Kody nie maja juz wypelnienia — konflikty bankow zdejmuje permutacja
    kawalkow (`_swz`), nie odstep wiersza.
    """
    return 4 * NS * ((BM + BN) * 8 * KC + (BM + BN) * KC)


def _swz[KC: Int](r: Int, ch: Int) -> Int:
    """Przesuniecie u32 kawalka 16-bajtowego `ch` wiersza `r` po permutacji.

    Wiersz ma NCH = 2*KC kawalkow po 16 B, wiec 128-bajtowy blok bankow mieszci
    BRB = 8/NCH kolejnych WIERSZY. Wewnatrz bloku kawalki dostaja gniazdo
    przesuniete o numer bloku — dzieki temu osiem wierszy czytanych jednym
    `ldmatrix` (i osiem pasow piszacych `cp.async`) trafia w 32 rozne banki.
    Samo wypelnienie tego nie daje: dla KC=1 wiersz ma 32 B, wiec osiem wierszy
    to 256 B i przy KAZDYM odstepie zostaje kolizja dwudrozna po stronie zapisu.
    Przy okazji znikaja 4 u32 wypelnienia na wiersz.
    """
    comptime NCH = 2 * KC
    comptime BRB = 8 // NCH
    return (r // BRB) * 32 + 4 * (
        ((r % BRB) * NCH + ch + (r // BRB) % NCH) % 8
    )


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
    OUT: DType, N: Int, K: Int, NT: Int = 8
](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
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
            y[row * N + col] = Scalar[OUT](acc[t][i])


def _stage[
    ROWS: Int, KC: Int, NTHR: Int
](
    s: Int,
    buf: Int,
    tid: Int,
    row0: Int,
    row_words: Int,
    scale_words: Int,
    src: UnsafePointer[UInt32, MutAnyOrigin],
    src_s: UnsafePointer[UInt32, MutAnyOrigin],
    dst: UnsafePointer[
        UInt32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    dst_s: UnsafePointer[
        UInt32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
):
    """cp.async etapu `s` (KC blokow K64) kafla ROWS wierszy do slotu `buf`.

    Kody ida po 16 B (4 u32), skale po 4*KC B — wiersz skal to dokladnie KC
    kolejnych u32, wiec jeden `cp.async` na wiersz wystarczy do KC=4.
    """
    comptime TILE = ROWS * 8 * KC
    comptime STILE = ROWS * KC
    comptime CPR = 2 * KC          # kopii 16-bajtowych na wiersz
    comptime CODES = ROWS * CPR
    k_word = s * (8 * KC)

    comptime for p in range((CODES + NTHR - 1) // NTHR):
        cid = tid + p * NTHR
        if cid < CODES:
            r = cid // CPR
            ch = cid % CPR
            async_copy[16](
                (
                    src + (row0 + r) * row_words + k_word + ch * 4
                ).address_space_cast[AddressSpace.GLOBAL](),
                dst + buf * TILE + _swz[KC](r, ch),
            )

    comptime for p in range((ROWS + NTHR - 1) // NTHR):
        r = tid + p * NTHR
        if r < ROWS:
            async_copy[4 * KC](
                (src_s + (row0 + r) * scale_words + s * KC).address_space_cast[
                    AddressSpace.GLOBAL
                ](),
                dst_s + buf * STILE + r * KC,
            )


def gemm_nvfp4_tiled[
    OUT: DType,
    N: Int, K: Int, BM: Int, BN: Int, KC: Int, NW: Int, WM: Int, GM: Int,
    NS: Int,
](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    a: UnsafePointer[UInt32, MutAnyOrigin],
    sa: UnsafePointer[UInt32, MutAnyOrigin],
    b: UnsafePointer[UInt32, MutAnyOrigin],
    sb: UnsafePointer[UInt32, MutAnyOrigin],
    tiles_m: Int,
):
    """Y[M,N] = A[M,K] * B[N,K]^T na kaflu BM x BN, KC blokow K64 na etap.

    Blok NW warpow ustawionych WM x (NW/WM); kazdy warp liczy podkafel
    (MT*16) x (NT*8) i czyta fragmenty z pamieci wspoldzielonej, ktora oba
    operandy dostaja przez podwojnie buforowany `cp.async`. Wynik jest BITOWO
    IDENTYCZNY z wersja naiwna: kolejnosc sumowania po K sie nie zmienia.

    Kafle w pamieci wspoldzielonej nie maja wypelnienia: konflikty bankow
    zdejmuje permutacja kawalkow 16-bajtowych (`_swz`), po obu stronach —
    i przy `ldmatrix`, i przy `cp.async`.

    Siatka jest JEDNOWYMIAROWA i przenumerowana grupami po GM kafli M: bloki
    lecace obok siebie maja wspolne n0, wiec plat B, ktory czytaja, zostaje w
    L2 na caly czas ich zycia. Przy naturalnej kolejnosci (n najszybsze) fala
    bloków przemiata cale B dla jednego m i czyta je z DRAM-u ponownie dla
    kazdego kolejnego — to na tej maszynie kosztuje wielokrotnosc, bo 273 GB/s
    LPDDR5X jest tu ograniczeniem, nie tensor core.
    """
    comptime NCH = 2 * KC
    comptime BRB = 8 // NCH        # wierszy w 128-bajtowym bloku bankow
    comptime ATILE = BM * 8 * KC
    comptime BTILE = BN * 8 * KC
    comptime SATILE = BM * KC
    comptime SBTILE = BN * KC
    comptime NTHR = NW * LANES
    comptime WN = NW // WM
    comptime MT = BM // (WM * 16)
    comptime NT = BN // (WN * 8)
    comptime A_ROW = K // 8
    comptime S_ROW = K // 64
    comptime STAGES = K // (64 * KC)
    comptime TN = N // BN

    # Jeden blok dynamicznej pamieci wspoldzielonej pociety na cztery kafle:
    # statyczne `stack_allocation` ma twardy sufit 48 KiB, a NS>2 albo szerszy
    # kafel od razu go przekracza (limit opt-in na tej maszynie to 99 KiB).
    sm = external_memory[
        UInt32, address_space = AddressSpace.SHARED, alignment=16
    ]()
    a_sm = sm
    b_sm = sm + NS * ATILE
    sa_sm = sm + NS * (ATILE + BTILE)
    sb_sm = sm + NS * (ATILE + BTILE + SATILE)

    tid = Int(thread_idx.x)
    lane = tid % LANES
    wid = tid // LANES
    g = lane // 4
    q = lane % 4
    bid = Int(block_idx.x)
    grp = bid // (GM * TN)
    m_first = grp * GM
    gm = min(GM, tiles_m - m_first)
    rank = bid - m_first * TN
    m0 = (m_first + rank % gm) * BM
    n0 = (rank // gm) * BN
    warp_m = (wid % WM) * (MT * 16)
    warp_n = (wid // WM) * (NT * 8)
    # Pas 4r niesie skale wiersza r, pas 4r+1 wiersza r+8 (patrz wersja naiwna).
    s_off = 8 if (q == 1) else 0

    # Fragment `mma` k64 to DOKLADNIE to, co zwraca `ldmatrix`: pas 4g+q ma
    # dostac u32 numer q wiersza g, wiersza g+8 oraz oba te u32 przesuniete o
    # pol kafla — czyli cztery plytki 8x8 b16 czytane z ukladu wierszowego.
    # Adresy podaje sie w innym rozbiciu pasa (osiem wierszy na plytke) niz
    # to, w ktorym instrukcja potem oddaje dane, i to jest cala sztuczka:
    # jedna instrukcja zastepuje cztery ladowania 4-bajtowe dla A i dwa dla B.
    a_f16 = a_sm.bitcast[Float16]()
    b_f16 = b_sm.bitcast[Float16]()
    # Gniazdo w bloku nie zalezy od `mi`/`ni` (16 i 8 sa wielokrotnosciami BRB),
    # wiec czesc stala permutacji liczy sie raz na pas, a w petli zostaje sam
    # skok o blok.
    a_r0 = warp_m + lane % 8 + 8 * ((lane // 8) % 2)
    a_slot0 = (a_r0 % BRB) * NCH + (a_r0 // BRB) % NCH + lane // 16
    b_c0 = warp_n + lane % 8
    b_slot0 = (b_c0 % BRB) * NCH + (b_c0 // BRB) % NCH + (lane // 8) % 2
    # JEDEN wskaznik na krok K i na pas. Bez tego `% 8` zostaje w petli, adres
    # przestaje byc "wskaznik + stala" i ptxas trzyma go w osobnych rejestrach
    # — az do spilli przy 255.
    var a_ptr = InlineArray[
        UnsafePointer[
            Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
        ],
        KC,
    ](uninitialized=True)
    var b_ptr = InlineArray[
        UnsafePointer[
            Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
        ],
        KC,
    ](uninitialized=True)
    comptime for c in range(KC):
        a_ptr[c] = a_f16 + 2 * (
            (a_r0 // BRB) * 32 + 4 * ((a_slot0 + 2 * c) % 8)
        )
        b_ptr[c] = b_f16 + 2 * (
            (b_c0 // BRB) * 32 + 4 * ((b_slot0 + 2 * c) % 8)
        )

    var acc = InlineArray[SIMD[DType.float32, 4], MT * NT](
        fill=SIMD[DType.float32, 4](0.0)
    )

    # Fragmenty leza w DWOCH kompletach rejestrow: zanim pojdzie `mma` kroku k,
    # `ldmatrix` kroku k+1 jest juz wystawiony do drugiego kompletu. Bez tego
    # opoznienie `ldmatrix` stoi odsloniete na poczatku kazdego etapu, a przy
    # 1 CTA na SM nie ma innych warpow, ktore by je zaslonily.
    # Podwojnie buforowane sa TYLKO kody. Skale zostaja w pamieci wspoldzielonej
    # i sa czytane dopiero przy `mma`: to odczyty rozgloszeniowe (cala czworka
    # pasow bierze ten sam adres), a trzymanie ich w drugim komplecie kosztowalo
    # 2*(MT+NT) rejestrow i wpychalo jadro w spille przy 255.
    var af = InlineArray[SIMD[DType.uint32, 4], 2 * MT](uninitialized=True)
    var bf = InlineArray[SIMD[DType.uint32, 2], 2 * NT](uninitialized=True)

    comptime for i in range(NS - 1):
        _stage[BM, KC, NTHR](i, i, tid, m0, A_ROW, S_ROW, a, sa, a_sm, sa_sm)
        _stage[BN, KC, NTHR](i, i, tid, n0, A_ROW, S_ROW, b, sb, b_sm, sb_sm)
        async_copy_commit_group()
    async_copy_wait_group(Int32(NS - 2))
    barrier()

    comptime for mi in range(MT):
        af[mi] = bitcast[DType.uint32, 4](
            ld_matrix[8](a_ptr[0] + 2 * (mi * (16 // BRB) * 32))
        )
    comptime for ni in range(NT):
        bf[ni] = bitcast[DType.uint32, 2](
            ld_matrix[4](b_ptr[0] + 2 * (ni * (8 // BRB) * 32))
        )

    # Etapy ida PARAMI, zeby parzystosc kompletu rejestrow (`cur`) byla stala
    # w czasie kompilacji takze dla nieparzystego KC. Indeks kompletu liczony
    # z `s` w czasie wykonania zepchnalby fragmenty do pamieci lokalnej.
    var s = 0
    while s < STAGES:
        comptime for half in range(2):
            st = s + half
            buf = st % NS
            comptime for c in range(KC):
                comptime cur = (half * KC + c) % 2
                comptime nx = 1 - cur
                # Skale MUSZA byc odczytane ZANIM ponizej ruszy `cp.async`
                # nadpisujacy bufor (st-1) % NS: bariera konczaca poprzedni etap
                # ogradza tylko to, co przed nia. Odczyt przy `mma` — juz za
                # bariera tego etapu — scigalby sie z tym zapisem.
                var ascv = InlineArray[UInt32, MT](uninitialized=True)
                var bscv = InlineArray[UInt32, NT](uninitialized=True)
                comptime for mi in range(MT):
                    ascv[mi] = sa_sm[
                        buf * SATILE + (warp_m + mi * 16 + g + s_off) * KC + c
                    ]
                comptime for ni in range(NT):
                    bscv[ni] = sb_sm[
                        buf * SBTILE + (warp_n + ni * 8 + g) * KC + c
                    ]

                comptime nc = (c + 1) % KC
                var nbuf = buf

                comptime if c + 1 == KC:
                    # Ostatni krok K etapu: dopiero TERAZ wolno pisac do bufora
                    # (st-1) % NS, bo bariera konczaca poprzedni etap juz
                    # zagwarantowala, ze nikt go nie czyta. Stad JEDNA bariera
                    # na etap, nie dwie.
                    nxt = st + NS - 1
                    if nxt < STAGES:
                        _stage[BM, KC, NTHR](
                            nxt, nxt % NS, tid, m0, A_ROW, S_ROW,
                            a, sa, a_sm, sa_sm,
                        )
                        _stage[BN, KC, NTHR](
                            nxt, nxt % NS, tid, n0, A_ROW, S_ROW,
                            b, sb, b_sm, sb_sm,
                        )
                    async_copy_commit_group()
                    if st + 1 < STAGES:
                        nbuf = (st + 1) % NS
                        async_copy_wait_group(Int32(NS - 2))
                        barrier()

                comptime for mi in range(MT):
                    af[nx * MT + mi] = bitcast[DType.uint32, 4](
                        ld_matrix[8](
                            a_ptr[nc]
                            + 2 * (nbuf * ATILE + mi * (16 // BRB) * 32)
                        )
                    )
                comptime for ni in range(NT):
                    bf[nx * NT + ni] = bitcast[DType.uint32, 2](
                        ld_matrix[4](
                            b_ptr[nc]
                            + 2 * (nbuf * BTILE + ni * (8 // BRB) * 32)
                        )
                    )

                comptime for ni in range(NT):
                    comptime for mi in range(MT):
                        var r = mma_nvfp4(
                            af[cur * MT + mi][0], af[cur * MT + mi][1],
                            af[cur * MT + mi][2], af[cur * MT + mi][3],
                            bf[cur * NT + ni][0], bf[cur * NT + ni][1],
                            acc[mi * NT + ni],
                            ascv[mi], bscv[ni],
                        )
                        acc[mi * NT + ni] = SIMD[DType.float32, 4](
                            r[0], r[1], r[2], r[3]
                        )
        s += 2

    # d[0],d[1] to sasiadujace kolumny 2q i 2q+1, wiec ida JEDNYM zapisem
    # 8-bajtowym: osiem pasow o tym samym wierszu pokrywa wtedy 32 bajty
    # ciagiem, zamiast czterech rozrzuconych zapisow po 4 bajty.
    comptime for mi in range(MT):
        comptime for ni in range(NT):
            d = acc[mi * NT + ni]
            row = m0 + warp_m + mi * 16 + g
            col = n0 + warp_n + ni * 8 + 2 * q
            (y + row * N + col).store[width=2, alignment = 2 * size_of[OUT]()](
                SIMD[DType.float32, 2](d[0], d[1]).cast[OUT]()
            )
            (y + (row + 8) * N + col).store[
                width=2, alignment = 2 * size_of[OUT]()
            ](SIMD[DType.float32, 2](d[2], d[3]).cast[OUT]())


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


def _check[
    M: Int, N: Int, K: Int,
    BM: Int = 128, BN: Int = 256, KC: Int = 1, NW: Int = 8, WM: Int = 2,
    GM: Int = 8, NS: Int = 2,
](ctx: DeviceContext, tiled: Bool) raises:
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

    if tiled:
        ctx.enqueue_function[
            gemm_nvfp4_tiled[DType.float32, N, K, BM, BN, KC, NW, WM, GM, NS],
        ](
            y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
            b.unsafe_ptr(), sb.unsafe_ptr(), M // BM,
            grid_dim=(M // BM) * (N // BN), block_dim=NW * LANES,
            shared_mem_bytes = _smem_bytes[BM, BN, KC, NS](),
        )
    else:
        ctx.enqueue_function[gemm_nvfp4[DType.float32, N, K]](
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
    var tag = String("kafel ") + String(BM) + "x" + String(BN) + "x" + String(
        64 * KC
    ) + " " + String(NW) + "w" if tiled else String("naiwny")
    if bad == 0:
        print(
            tag + " M=" + String(M) + " N=" + String(N) + " K=" + String(K)
            + ": ZGADZA SIE co do bitu (" + String(M * N) + " elementow)"
        )
    else:
        print(tag, "ROZNICE", bad, "z", M * N, "| pierwsza:", first)


def _time[
    M: Int, N: Int, K: Int,
    BM: Int = 128, BN: Int = 256, KC: Int = 1, NW: Int = 8, WM: Int = 2,
    GM: Int = 8, NS: Int = 2, OUT: DType = DType.float16,
](ctx: DeviceContext, name: String, tiled: Bool) raises:
    var a = ctx.enqueue_create_buffer[DType.uint32](M * K // 8)
    var b = ctx.enqueue_create_buffer[DType.uint32](N * K // 8)
    var sa = ctx.enqueue_create_buffer[DType.uint32](M * K // 64)
    var sb = ctx.enqueue_create_buffer[DType.uint32](N * K // 64)
    var y = ctx.enqueue_create_buffer[OUT](M * N)

    # Bezczynne GPU siedzi na niskim zegarze — bez rozgrzewki pomiar klamie.
    for _ in range(300):
        if tiled:
            ctx.enqueue_function[
                    gemm_nvfp4_tiled[OUT, N, K, BM, BN, KC, NW, WM, GM, NS]
                ](
                y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
                b.unsafe_ptr(), sb.unsafe_ptr(), M // BM,
                grid_dim=(M // BM) * (N // BN), block_dim=NW * LANES,
                shared_mem_bytes = _smem_bytes[BM, BN, KC, NS](),
            )
        else:
            ctx.enqueue_function[gemm_nvfp4[OUT, N, K]](
                y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
                b.unsafe_ptr(), sb.unsafe_ptr(),
                grid_dim=(N // 64, M // 16), block_dim=LANES,
            )
    ctx.synchronize()

    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var t0 = perf_counter_ns()
        for _ in range(ITERS):
            if tiled:
                ctx.enqueue_function[
                    gemm_nvfp4_tiled[OUT, N, K, BM, BN, KC, NW, WM, GM, NS]
                ](
                    y.unsafe_ptr(), a.unsafe_ptr(), sa.unsafe_ptr(),
                    b.unsafe_ptr(), sb.unsafe_ptr(), M // BM,
                    grid_dim=(M // BM) * (N // BN), block_dim=NW * LANES,
                    shared_mem_bytes = _smem_bytes[BM, BN, KC, NS](),
                )
            else:
                ctx.enqueue_function[gemm_nvfp4[OUT, N, K]](
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
    _check[128, 256, 256, 128, 256, 1, 8, 2, 8, 3](ctx, True)

    print("")
    print("Sufit: 497 TFLOPS na tej instrukcji, 222 GB/s z DRAM.")
    print("Kafle bez wypelnienia (permutacja), wiec potok miesci sie glebiej.")
    print("")
    print("--- 128x256x64, 8w, potok N etapow ---")
    _time[1024, 4096, 4096, 128, 256, 1, 8, 2, 8, 3](ctx, "q/o      NS=3:", True)
    _time[1024, 4096, 4096, 128, 256, 1, 8, 2, 8, 4](ctx, "q/o      NS=4:", True)
    _time[1024, 4096, 4096, 128, 256, 1, 8, 2, 8, 6](ctx, "q/o      NS=6:", True)
    _time[1024, 11264, 4096, 128, 256, 1, 8, 2, 8, 3](ctx, "gate/up  NS=3:", True)
    _time[1024, 11264, 4096, 128, 256, 1, 8, 2, 8, 4](ctx, "gate/up  NS=4:", True)
    _time[1024, 11264, 4096, 128, 256, 1, 8, 2, 8, 6](ctx, "gate/up  NS=6:", True)
    _time[1024, 4096, 11264, 128, 256, 1, 8, 2, 8, 3](ctx, "down     NS=3:", True)
    _time[1024, 4096, 11264, 128, 256, 1, 8, 2, 8, 4](ctx, "down     NS=4:", True)
    _time[1024, 4096, 11264, 128, 256, 1, 8, 2, 8, 6](ctx, "down     NS=6:", True)

    print("")
    print("--- 128x256x128 (KC=2), 8w ---")
    _time[1024, 4096, 4096, 128, 256, 2, 8, 2, 8, 3](ctx, "q/o      NS=3:", True)
    _time[1024, 11264, 4096, 128, 256, 2, 8, 2, 8, 3](ctx, "gate/up  NS=3:", True)
    _time[1024, 4096, 11264, 128, 256, 2, 8, 2, 8, 3](ctx, "down     NS=3:", True)

    print("")
    print("--- 2048x2048x2048 (13 MiB, caly w L2); podloga 34,5 us ---")
    _time[2048, 2048, 2048, 128, 256, 1, 8, 2, 8, 3](ctx, "NS=3 :", True)
    _time[2048, 2048, 2048, 128, 256, 1, 8, 2, 8, 4](ctx, "NS=4 :", True)
    _time[2048, 2048, 2048, 128, 256, 1, 8, 2, 8, 6](ctx, "NS=6 :", True)
